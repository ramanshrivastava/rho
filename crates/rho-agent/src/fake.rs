//! Deterministic scriptable provider for tests (tau `tau_ai/fake.py`).
//!
//! ## Why it lives in `rho-agent`, not `rho-ai`
//!
//! tau's `FakeProvider` sits in `tau_ai`. rho can't follow that placement: the
//! M2 loop/harness golden tests (in `rho-agent`) must drive the provider seam,
//! and `rho-agent` **must not** depend on `rho-ai` (the layering contract). So
//! the fake is a first-class `rho-agent` util behind the default-on `fake`
//! feature — usable by this crate's tests today and by `rho-ai`/`rho-coding`
//! tests later, without inverting the dependency graph. (This also dissolves the
//! very `tau_agent` ↔ `tau_ai` coupling the crate split exists to prevent.)
//!
//! It replays pre-scripted [`AssistantMessageEvent`] sequences — one per
//! `stream_response` call — and records each call's `(model, system, messages,
//! tools)` snapshot, exactly like tau's fake, so tests can assert what the loop
//! passed the provider (`provider.calls[i]`). The replay loop polls the
//! cancellation signal before each event, matching tau.

use std::sync::{Arc, Mutex};

use futures::stream::{self, BoxStream, StreamExt};

use crate::messages::AgentMessage;
use crate::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use crate::provider_events::AssistantMessageEvent;
use crate::tools::AgentTool;

/// One recorded `stream_response` invocation (tau's `provider.calls` tuple):
/// the model, system prompt, and message/tool **snapshots** taken at call time.
#[derive(Clone)]
pub struct RecordedCall {
    /// Requested model id.
    pub model: String,
    /// System prompt.
    pub system: String,
    /// Snapshot of the message list the loop passed.
    pub messages: Vec<AgentMessage>,
    /// Snapshot of the tool list the loop passed.
    pub tools: Vec<AgentTool>,
}

/// A provider that replays predefined assistant event streams (tau
/// `FakeProvider`).
#[derive(Clone)]
pub struct FakeProvider {
    streams: Arc<Mutex<std::collections::VecDeque<Vec<AssistantMessageEvent>>>>,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl FakeProvider {
    /// Build a fake from a list of scripted streams (consumed front-to-back, one
    /// per `stream_response` call; an exhausted fake replays an empty stream).
    #[must_use]
    pub fn new(streams: Vec<Vec<AssistantMessageEvent>>) -> Self {
        Self {
            streams: Arc::new(Mutex::new(streams.into())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The recorded calls so far (tau's `provider.calls`).
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// Number of `stream_response` invocations recorded.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }
}

impl ModelProvider for FakeProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        // Record + pop the next script synchronously (tau does both eagerly in
        // the sync method body, before returning the async iterator).
        self.calls.lock().expect("calls lock").push(RecordedCall {
            model: model.to_string(),
            system: system.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        });
        let events = self
            .streams
            .lock()
            .expect("streams lock")
            .pop_front()
            .unwrap_or_default();

        // Replay lazily, polling the cancellation signal before each event
        // (tau: `if signal is not None and signal.is_cancelled(): return`).
        let stream = stream::iter(events).take_while(move |_event| {
            let stop = signal.as_ref().is_some_and(|s| s.is_cancelled());
            futures::future::ready(!stop)
        });
        let boxed: BoxStream<'static, AssistantMessageEvent> = stream.boxed();
        boxed
    }
}
