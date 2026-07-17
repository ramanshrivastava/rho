//! SSE event-stream golden tests: feeding each recorded provider body through
//! the adapter must reproduce the canonical `AssistantMessageEvent` sequence
//! **byte-identical** to `fixtures/sse/<provider>/<case>.events.jsonl`.
//!
//! The body is served over real HTTP by the in-process [`mock_provider`] server
//! (exercising the adapter's reqwest + incremental line-splitting path), the
//! clock is pinned to tau's frozen fixture value, and each emitted event is
//! serialized with `serde_json::to_string` (compact, matching tau's
//! `model_dump_json`). A diff here is a bug in the rho adapter, never the fixture.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use mock_provider::{MockConfig, MockServer};
use rho_agent::clock::{Clock, FixedClock};
use rho_agent::fake::FakeProvider;
use rho_agent::messages::AgentMessage;
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::tools::AgentTool;
use rho_ai::{
    AnthropicConfig, AnthropicProvider, GoogleGenerativeAIProvider, MistralConversationsProvider,
    OpenAICodexConfig, OpenAICodexProvider, OpenAICompatibleConfig, OpenAICompatibleProvider,
};
use support::{bash_tool, codex_creds_resolver, read_tool, user};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sse")
}

fn read_fixture(rel: &str) -> String {
    let path = fixtures_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()))
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::fixture())
}

/// Serialize a provider's event stream to JSONL (trailing newline, matching the
/// fixture layout).
async fn stream_to_jsonl(mut stream: rho_agent::provider::AssistantEventStream) -> String {
    let mut lines = Vec::new();
    while let Some(event) = stream.next().await {
        lines.push(serde_json::to_string(&event).expect("serialize event"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Spawn a mock server replaying `<case>.sse`, drive the provider built from the
/// server URL, and assert the event JSONL matches `<case>.events.jsonl`.
async fn check<P, B>(
    build: B,
    model: &str,
    messages: Vec<AgentMessage>,
    tools: Vec<AgentTool>,
    case: &str,
    status: u16,
) where
    P: ModelProvider + 'static,
    B: FnOnce(String) -> P,
{
    let body = read_fixture(&format!("{case}.sse"));
    let server = MockServer::spawn(MockConfig::sse(body).with_status(status)).await;
    let provider = build(server.base_url());
    let stream = provider.stream_response(model, "You are Tau.", &messages, &tools, None);
    let actual = stream_to_jsonl(stream).await;
    server.shutdown().await;
    let expected = read_fixture(&format!("{case}.events.jsonl"));
    assert_eq!(actual, expected, "\n{case}.events.jsonl BYTE MISMATCH");
}

fn openai(base_url: String) -> OpenAICompatibleProvider {
    OpenAICompatibleProvider::new(OpenAICompatibleConfig::new("k").with_base_url(base_url))
        .with_clock(clock())
}

fn openai_no_retry(base_url: String) -> OpenAICompatibleProvider {
    OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k")
            .with_base_url(base_url)
            .with_max_retries(0),
    )
    .with_clock(clock())
}

fn anthropic(base_url: String) -> AnthropicProvider {
    AnthropicProvider::new(AnthropicConfig::new("k").with_base_url(base_url)).with_clock(clock())
}

fn google(base_url: String) -> GoogleGenerativeAIProvider {
    GoogleGenerativeAIProvider::new(OpenAICompatibleConfig::new("k").with_base_url(base_url))
        .with_clock(clock())
}

fn mistral(base_url: String) -> MistralConversationsProvider {
    MistralConversationsProvider::new(OpenAICompatibleConfig::new("k").with_base_url(base_url))
        .with_clock(clock())
}

fn codex(base_url: String) -> OpenAICodexProvider {
    OpenAICodexProvider::new(OpenAICodexConfig::new(codex_creds_resolver()).with_base_url(base_url))
        .with_clock(clock())
}

// --- anthropic --------------------------------------------------------------

#[tokio::test]
async fn anthropic_text() {
    check(
        anthropic,
        "claude-x",
        vec![user("Say hello")],
        vec![],
        "anthropic/text",
        200,
    )
    .await;
}

#[tokio::test]
async fn anthropic_thinking() {
    check(
        anthropic,
        "claude-x",
        vec![user("Say hello")],
        vec![],
        "anthropic/thinking",
        200,
    )
    .await;
}

#[tokio::test]
async fn anthropic_tool_calls() {
    check(
        anthropic,
        "claude-x",
        vec![user("run ls")],
        vec![bash_tool()],
        "anthropic/tool_calls",
        200,
    )
    .await;
}

// --- openai_compatible ------------------------------------------------------

#[tokio::test]
async fn openai_compatible_text() {
    check(
        openai,
        "gpt-x",
        vec![user("Say hello")],
        vec![],
        "openai_compatible/text",
        200,
    )
    .await;
}

#[tokio::test]
async fn openai_compatible_reasoning() {
    check(
        openai,
        "gpt-x",
        vec![user("Say hello")],
        vec![],
        "openai_compatible/reasoning",
        200,
    )
    .await;
}

#[tokio::test]
async fn openai_compatible_tool_calls() {
    check(
        openai,
        "gpt-x",
        vec![user("run ls")],
        vec![read_tool()],
        "openai_compatible/tool_calls",
        200,
    )
    .await;
}

#[tokio::test]
async fn openai_compatible_error() {
    check(
        openai_no_retry,
        "gpt-x",
        vec![user("Say hello")],
        vec![],
        "openai_compatible/error",
        400,
    )
    .await;
}

// --- openai_codex -----------------------------------------------------------

#[tokio::test]
async fn openai_codex_text() {
    check(
        codex,
        "gpt-5.5",
        vec![user("Say hello")],
        vec![],
        "openai_codex/text",
        200,
    )
    .await;
}

#[tokio::test]
async fn openai_codex_reasoning() {
    check(
        codex,
        "gpt-5.5",
        vec![user("Say hello")],
        vec![],
        "openai_codex/reasoning",
        200,
    )
    .await;
}

#[tokio::test]
async fn openai_codex_tool_calls() {
    check(
        codex,
        "gpt-5.5",
        vec![user("run ls")],
        vec![read_tool()],
        "openai_codex/tool_calls",
        200,
    )
    .await;
}

// --- google -----------------------------------------------------------------

#[tokio::test]
async fn google_text() {
    check(
        google,
        "gemini-2.5-flash",
        vec![user("Say hello")],
        vec![],
        "google/text",
        200,
    )
    .await;
}

#[tokio::test]
async fn google_tool_calls() {
    check(
        google,
        "gemini-2.5-flash",
        vec![user("run ls")],
        vec![bash_tool()],
        "google/tool_calls",
        200,
    )
    .await;
}

// --- mistral ----------------------------------------------------------------

#[tokio::test]
async fn mistral_text() {
    check(
        mistral,
        "mistral-large",
        vec![user("Say hello")],
        vec![],
        "mistral/text",
        200,
    )
    .await;
}

#[tokio::test]
async fn mistral_tool_calls() {
    check(
        mistral,
        "mistral-large",
        vec![user("run ls")],
        vec![read_tool()],
        "mistral/tool_calls",
        200,
    )
    .await;
}

// --- fake -------------------------------------------------------------------

#[tokio::test]
async fn fake_replays_scripted_events() {
    // The fake provider replays canonical events verbatim; feeding the recorded
    // input script back through it must reproduce the same output.
    let input = read_fixture("fake/text.input.jsonl");
    let script: Vec<AssistantMessageEvent> = input
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| serde_json::from_str(line).expect("parse scripted event"))
        .collect();
    let provider = FakeProvider::new(vec![script]);
    let stream = provider.stream_response("fake", "You are Tau.", &[user("hi")], &[], None);
    let actual = stream_to_jsonl(stream).await;
    let expected = read_fixture("fake/text.events.jsonl");
    assert_eq!(actual, expected, "\nfake/text.events.jsonl BYTE MISMATCH");
}
