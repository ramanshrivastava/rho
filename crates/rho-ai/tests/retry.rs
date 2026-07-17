//! Retry-envelope tests ported from tau's `tests/test_tau_ai.py` (the
//! transient-status retry, non-transient no-retry, and cancellation-aborts-backoff
//! cases). These exercise the shared engine over real HTTP via a mock server that
//! can return a different status per attempt.
//!
//! The pure delay-curve / status-class / cancellation-wait units live in
//! `src/retry.rs`; these assert the *envelope* wiring around them.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use mock_provider::MockServer;
use rho_agent::clock::{Clock, FixedClock};
use rho_agent::provider::{
    AssistantEventStream, CancellationToken, ModelProvider, SimpleCancellationToken,
};
use rho_agent::provider_events::AssistantMessageEvent;
use rho_ai::{OpenAICompatibleConfig, OpenAICompatibleProvider};
use support::user;

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::fixture())
}

async fn event_types(mut stream: AssistantEventStream) -> Vec<String> {
    let mut kinds = Vec::new();
    while let Some(event) = stream.next().await {
        kinds.push(event_kind(&event));
    }
    kinds
}

fn event_kind(event: &AssistantMessageEvent) -> String {
    let value = serde_json::to_value(event).expect("event to value");
    value["type"].as_str().unwrap_or("?").to_string()
}

/// A mock server whose per-attempt response is scripted. Serves each entry of
/// `responses` in turn (last entry repeats), recording the attempt count.
struct SequencedServer {
    server: MockServer,
    _responses: Arc<Mutex<Vec<(u16, String)>>>,
}

#[tokio::test]
async fn retries_transient_status_then_succeeds() {
    // First attempt 500, second 200 with a normal stream. The engine must retry
    // once and yield the full canonical sequence.
    let ok_body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let responses = Arc::new(Mutex::new(vec![
        (500u16, "try again".to_string()),
        (200u16, ok_body.to_string()),
    ]));
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = spawn_sequenced(responses.clone(), attempts.clone()).await;

    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k")
            .with_base_url(server.base_url())
            .with_max_retries(1)
            .with_max_retry_delay_seconds(0.0),
    )
    .with_clock(clock());
    let stream = provider.stream_response("m", "You are Tau.", &[user("Say ok")], &[], None);
    let kinds = event_types(stream).await;
    server.shutdown().await;

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "should make two attempts"
    );
    assert_eq!(
        kinds,
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
}

#[tokio::test]
async fn credentials_resolved_once_across_retries() {
    // 500 then 200: two attempts, but the OpenAI credential resolver must run
    // exactly once (tau resolves before the retry loop).
    let ok_body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let responses = Arc::new(Mutex::new(vec![
        (500u16, "try again".to_string()),
        (200u16, ok_body.to_string()),
    ]));
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = spawn_sequenced(responses, attempts.clone()).await;

    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let calls = resolver_calls.clone();
    let resolver: rho_ai::env::RuntimeProviderAuthResolver = Arc::new(move || {
        let calls = calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(rho_ai::RuntimeProviderAuth {
                api_key: "resolved-key".to_string(),
                base_url: None,
                headers: None,
            })
        })
    });

    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k")
            .with_base_url(server.base_url())
            .with_max_retries(1)
            .with_max_retry_delay_seconds(0.0)
            .with_credential_resolver(resolver),
    )
    .with_clock(clock());
    let stream = provider.stream_response("m", "You are Tau.", &[user("Say ok")], &[], None);
    let _ = event_types(stream).await;
    server.shutdown().await;

    assert_eq!(attempts.load(Ordering::SeqCst), 2, "two HTTP attempts");
    assert_eq!(
        resolver_calls.load(Ordering::SeqCst),
        1,
        "credentials must be resolved once, not per attempt"
    );
}

#[tokio::test]
async fn does_not_retry_non_transient_status() {
    let body = r#"{"error":{"message":"The selected model is unavailable."}}"#;
    let responses = Arc::new(Mutex::new(vec![(400u16, body.to_string())]));
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = spawn_sequenced(responses.clone(), attempts.clone()).await;

    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k")
            .with_base_url(server.base_url())
            .with_provider_name("test-openai")
            .with_max_retries(3)
            .with_max_retry_delay_seconds(0.0),
    )
    .with_clock(clock());
    let mut stream =
        provider.stream_response("test-model", "You are Tau.", &[user("Say ok")], &[], None);
    let mut last = None;
    while let Some(event) = stream.next().await {
        last = Some(event);
    }
    server.shutdown().await;

    assert_eq!(attempts.load(Ordering::SeqCst), 1, "no retry on 400");
    let AssistantMessageEvent::Error(err) = last.expect("an event") else {
        panic!("expected error event");
    };
    assert_eq!(
        err.error.error_message.as_deref(),
        Some(
            "test-openai request failed with status 400 for model test-model: The selected model is unavailable."
        )
    );
}

#[tokio::test]
async fn cancellation_stops_retry_backoff() {
    // Always 503; with a delay budget the engine would back off, but a pre-cancelled
    // signal must abort after the first attempt and surface a terminal error.
    let responses = Arc::new(Mutex::new(vec![(503u16, "try later".to_string())]));
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = spawn_sequenced(responses.clone(), attempts.clone()).await;

    let signal = SimpleCancellationToken::new();
    signal.cancel();
    let token: Arc<dyn CancellationToken> = Arc::new(signal);

    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k")
            .with_base_url(server.base_url())
            .with_max_retries(2)
            .with_max_retry_delay_seconds(1.0),
    )
    .with_clock(clock());
    let stream = provider.stream_response("m", "You are Tau.", &[user("Say ok")], &[], Some(token));
    let kinds = event_types(stream).await;
    server.shutdown().await;

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "cancellation aborts before a second attempt"
    );
    assert_eq!(kinds, vec!["start", "error"]);
}

/// Spawn a server that returns a distinct response per attempt (last repeats),
/// counting attempts. Implemented by rebuilding a fresh mock per response through
/// a small dispatcher.
async fn spawn_sequenced(
    responses: Arc<Mutex<Vec<(u16, String)>>>,
    attempts: Arc<AtomicUsize>,
) -> SequencedServer {
    // The mock server serves a fixed response; to vary by attempt we encode the
    // whole script into a single stateful server via a custom body selection.
    // MockConfig is static, so we instead run a tiny per-attempt server: the
    // simplest faithful approach is a status/body that flips using an atomic.
    let first = responses.lock().unwrap()[0].clone();
    let script = responses.clone();
    let counter = attempts.clone();
    let server = MockServer::spawn_with(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        let script = script.lock().unwrap();
        let idx = n.min(script.len() - 1);
        let (status, body) = script[idx].clone();
        (status, body.into_bytes())
    })
    .await;
    let _ = first;
    SequencedServer {
        server,
        _responses: responses,
    }
}

impl std::ops::Deref for SequencedServer {
    type Target = MockServer;
    fn deref(&self) -> &MockServer {
        &self.server
    }
}

impl SequencedServer {
    async fn shutdown(self) {
        self.server.shutdown().await;
    }
}
