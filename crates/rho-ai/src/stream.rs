//! The canonical-event accumulator (tau `tau_ai/stream.py`
//! `canonicalize_provider_stream`), reimagined as a direct-emit utility.
//!
//! ## Why this is not a port of `_provider_events`
//!
//! tau streams provider output twice: an adapter first emits transitional
//! `ProviderEvent`s (`_provider_events.py`), then `canonicalize_provider_stream`
//! rewrites them into the public `AssistantMessageEvent`s. The M3 plan locks in
//! collapsing that: rho adapters drive **this** accumulator directly, which emits
//! the canonical events. The transitional pydantic layer — its own models, its
//! own serialization — is gone. What remains is the *observable* contract of
//! `canonicalize_provider_stream`: the same event order, the same content-index
//! bookkeeping, the same finish-reason mapping, the same "stream ended without a
//! terminal event" error. Adapters feed [`Delta`]s (a transient in-process enum,
//! never serialized) and this turns them into canonical events.
//!
//! ## Snapshots
//!
//! Every event carries a `partial` snapshot of the assistant message so far. tau
//! deep-copies the message per event; rho mutates one working copy and clones it
//! into each event (the wire type owns its snapshot). Same observable protocol.

use std::sync::Arc;

use rho_agent::clock::Clock;
use rho_agent::messages::{
    AssistantContent, AssistantMessage, AssistantMessageDiagnostic, StopReason, TextContent,
    ThinkingContent, ToolCall, Usage,
};
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantErrorEvent, AssistantMessageEvent, AssistantStartEvent,
    DoneReason, ErrorReason, TextDeltaEvent, TextEndEvent, TextStartEvent, ThinkingDeltaEvent,
    ThinkingEndEvent, ThinkingStartEvent, ToolCallEndEvent, ToolCallStartEvent,
};

use crate::types::JsonMap;

/// A transient provider output signal fed to the [`StreamAccumulator`].
///
/// This is *not* a wire type (contrast tau's serialized `ProviderEvent`): it is
/// an in-process hand-off from an adapter's SSE parser to the accumulator, and
/// it is never (de)serialized. The response-start signal is modeled by calling
/// [`StreamAccumulator::response_start`] directly; retries are invisible here
/// (they produce no canonical output, exactly as `canonicalize` dropped them).
#[derive(Debug, Clone)]
pub enum Delta {
    /// A streamed text fragment.
    Text(String),
    /// A streamed thinking/reasoning fragment.
    Thinking(String),
    /// A completed tool call.
    ToolCall(ToolCall),
    /// The response finished: carries the provider's assembled message (for
    /// usage/metadata) and the raw finish reason.
    End {
        /// The provider's assembled message (authoritative for usage/metadata).
        message: AssistantMessage,
        /// The raw provider finish reason, if any.
        finish_reason: Option<String>,
    },
    /// A provider-level error to surface as a terminal error event.
    Error {
        /// Human-readable, secret-free error message.
        message: String,
        /// Optional structured diagnostic details.
        data: Option<JsonMap>,
    },
}

/// Which kind of block is currently open in the streamed content
/// (tau `active_kind`: `"text"` / `"thinking"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveKind {
    Text,
    Thinking,
}

/// Accumulates provider [`Delta`]s into canonical [`AssistantMessageEvent`]s
/// (tau `canonicalize_provider_stream`).
pub struct StreamAccumulator {
    partial: AssistantMessage,
    active_index: Option<usize>,
    active_kind: Option<ActiveKind>,
    started: bool,
    terminal: bool,
    api: String,
    provider: String,
    model: String,
    timestamp_ms: i64,
}

impl StreamAccumulator {
    /// Build an accumulator for one response. `api`/`provider`/`model` stamp the
    /// assistant message; `clock` fixes the message timestamp (so goldens
    /// reproduce tau's frozen clock).
    #[must_use]
    pub fn new(
        api: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        clock: &Arc<dyn Clock>,
    ) -> Self {
        let api = api.into();
        let provider = provider.into();
        let model = model.into();
        let timestamp_ms = clock.now_ms();
        let mut partial = AssistantMessage::default()
            .with_api(api.clone())
            .with_provider(provider.clone())
            .with_model(model.clone());
        partial.timestamp = timestamp_ms;
        Self {
            partial,
            active_index: None,
            active_kind: None,
            started: false,
            terminal: false,
            api,
            provider,
            model,
            timestamp_ms,
        }
    }

    /// The message timestamp this accumulator stamps (so adapters build their
    /// `End` message with a matching timestamp).
    #[must_use]
    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    /// Whether a terminal event (done/error) has been emitted.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn snapshot(&self) -> AssistantMessage {
        self.partial.clone()
    }

    fn ensure_started(&mut self, out: &mut Vec<AssistantMessageEvent>) {
        if !self.started {
            self.started = true;
            out.push(AssistantMessageEvent::Start(AssistantStartEvent::new(
                self.snapshot(),
            )));
        }
    }

    /// Signal that the provider began streaming (tau `ProviderResponseStartEvent`
    /// → emit `start` once).
    pub fn response_start(&mut self) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        out
    }

    /// Apply one [`Delta`], returning the canonical events it produces.
    pub fn apply(&mut self, delta: Delta) -> Vec<AssistantMessageEvent> {
        match delta {
            Delta::Text(text) => self.text_delta(&text),
            Delta::Thinking(text) => self.thinking_delta(&text),
            Delta::ToolCall(tool_call) => self.tool_call(tool_call),
            Delta::End {
                message,
                finish_reason,
            } => self.response_end(&message, finish_reason.as_deref()),
            Delta::Error { message, data } => self.error(message, data),
        }
    }

    /// End the active text/thinking block before the provider changes channels
    /// (tau `_end_active_block`). Emits the matching `*EndEvent` with a snapshot
    /// taken *before* any new block is appended.
    fn end_active_block(&self, out: &mut Vec<AssistantMessageEvent>) {
        let Some(index) = self.active_index else {
            return;
        };
        match &self.partial.content[index] {
            AssistantContent::Text(block) => {
                out.push(AssistantMessageEvent::TextEnd(TextEndEvent::new(
                    content_index(index),
                    block.text.clone(),
                    self.snapshot(),
                )));
            }
            AssistantContent::Thinking(block) => {
                out.push(AssistantMessageEvent::ThinkingEnd(ThinkingEndEvent::new(
                    content_index(index),
                    block.thinking.clone(),
                    self.snapshot(),
                )));
            }
            AssistantContent::ToolCall(_) => {}
        }
    }

    fn text_delta(&mut self, delta: &str) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        if self.active_kind != Some(ActiveKind::Text) {
            self.end_active_block(&mut out);
            let index = self.partial.content.len();
            self.active_index = Some(index);
            self.active_kind = Some(ActiveKind::Text);
            self.partial
                .content
                .push(AssistantContent::Text(TextContent::new("")));
            out.push(AssistantMessageEvent::TextStart(TextStartEvent::new(
                content_index(index),
                self.snapshot(),
            )));
        }
        let index = self.active_index.expect("active index set");
        if let AssistantContent::Text(block) = &mut self.partial.content[index] {
            block.text.push_str(delta);
        }
        out.push(AssistantMessageEvent::TextDelta(TextDeltaEvent::new(
            content_index(index),
            delta,
            self.snapshot(),
        )));
        out
    }

    fn thinking_delta(&mut self, delta: &str) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        if self.active_kind != Some(ActiveKind::Thinking) {
            self.end_active_block(&mut out);
            let index = self.partial.content.len();
            self.active_index = Some(index);
            self.active_kind = Some(ActiveKind::Thinking);
            self.partial
                .content
                .push(AssistantContent::Thinking(ThinkingContent::new("")));
            out.push(AssistantMessageEvent::ThinkingStart(
                ThinkingStartEvent::new(content_index(index), self.snapshot()),
            ));
        }
        let index = self.active_index.expect("active index set");
        if let AssistantContent::Thinking(block) = &mut self.partial.content[index] {
            block.thinking.push_str(delta);
        }
        out.push(AssistantMessageEvent::ThinkingDelta(
            ThinkingDeltaEvent::new(content_index(index), delta, self.snapshot()),
        ));
        out
    }

    fn tool_call(&mut self, tool_call: ToolCall) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        self.end_active_block(&mut out);
        self.active_index = None;
        self.active_kind = None;
        let index = self.partial.content.len();
        self.partial
            .content
            .push(AssistantContent::ToolCall(tool_call.clone()));
        out.push(AssistantMessageEvent::ToolCallStart(
            ToolCallStartEvent::new(content_index(index), self.snapshot()),
        ));
        out.push(AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(
            content_index(index),
            tool_call,
            self.snapshot(),
        )));
        out
    }

    fn response_end(
        &mut self,
        message: &AssistantMessage,
        finish_reason: Option<&str>,
    ) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);

        self.end_active_block(&mut out);
        self.active_index = None;
        self.active_kind = None;

        // Preserve the exact streamed content order; the provider's message
        // remains authoritative only for response metadata/usage.
        let mut final_message = message.clone();
        final_message.api.clone_from(&self.api);
        final_message.provider.clone_from(&self.provider);
        final_message.model.clone_from(&self.model);
        // tau's adapters build the response message with `default_factory`
        // (`current_timestamp_ms`) — the injected clock. Adapters can't see the
        // clock, so the accumulator stamps it here (matching the `partial`s).
        final_message.timestamp = self.timestamp_ms;
        final_message.content.clone_from(&self.partial.content);
        if final_message.content.is_empty() && !message.content.is_empty() {
            final_message.content.clone_from(&message.content);
        }
        // Carry provider replay metadata (thinking/text signatures, redaction)
        // from the parser's assembled message onto the canonical streamed blocks
        // without reordering them (tau `_copy_replay_metadata`).
        copy_replay_metadata(&mut final_message, message);
        let has_tools = !final_message.tool_calls().is_empty();
        let reason = map_finish_reason(finish_reason, has_tools);
        final_message.stop_reason = reason.into_stop_reason();
        self.terminal = true;
        out.push(AssistantMessageEvent::Done(AssistantDoneEvent::new(
            reason,
            final_message,
        )));
        out
    }

    pub(crate) fn error(
        &mut self,
        message: String,
        data: Option<JsonMap>,
    ) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        let mut error = self.partial.clone();
        error.stop_reason = StopReason::Error;
        error.error_message = Some(message);
        error.diagnostics = Some(vec![AssistantMessageDiagnostic {
            diagnostic_type: "provider_error".to_string(),
            timestamp: self.timestamp_ms,
            error: None,
            details: data,
        }]);
        self.terminal = true;
        out.push(AssistantMessageEvent::Error(AssistantErrorEvent::new(
            ErrorReason::Error,
            error,
        )));
        out
    }

    /// Finalize the stream after the adapter returns (tau `canonicalize`'s
    /// post-loop block): emit `start` if it never opened, and a terminal error if
    /// no terminal event was produced (cancellation, or a truncated stream).
    pub fn finish(&mut self) -> Vec<AssistantMessageEvent> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(AssistantMessageEvent::Start(AssistantStartEvent::new(
                self.snapshot(),
            )));
        }
        if !self.terminal {
            let mut error = self.partial.clone();
            error.stop_reason = StopReason::Error;
            error.error_message =
                Some("Provider stream ended without a terminal event".to_string());
            error.usage = Usage::default();
            self.terminal = true;
            out.push(AssistantMessageEvent::Error(AssistantErrorEvent::new(
                ErrorReason::Error,
                error,
            )));
        }
        out
    }
}

/// Convert a content-block index to the wire `content_index` (`i64`). Indices
/// are tiny; the saturating conversion never triggers in practice.
fn content_index(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

/// Map a raw provider finish reason to a canonical [`DoneReason`]
/// (tau `stream.py::_finish_reason`).
#[must_use]
pub fn map_finish_reason(value: Option<&str>, has_tools: bool) -> DoneReason {
    if has_tools || matches!(value, Some("tool_calls" | "tool_use" | "toolUse")) {
        DoneReason::ToolUse
    } else if matches!(
        value,
        Some("length" | "max_tokens" | "MAX_TOKENS" | "incomplete")
    ) {
        DoneReason::Length
    } else {
        DoneReason::Stop
    }
}

trait DoneReasonExt {
    fn into_stop_reason(self) -> StopReason;
}

impl DoneReasonExt for DoneReason {
    fn into_stop_reason(self) -> StopReason {
        match self {
            DoneReason::Stop => StopReason::Stop,
            DoneReason::Length => StopReason::Length,
            DoneReason::ToolUse => StopReason::ToolUse,
        }
    }
}

/// Copy provider replay metadata from `source` onto `target` without changing
/// block order (tau `_copy_replay_metadata`). Thinking blocks receive the
/// `thinking_signature` and `redacted` flag; text blocks receive the
/// `text_signature`. Blocks are matched positionally within each kind, stopping
/// at the shorter list (Python `zip(..., strict=False)`).
fn copy_replay_metadata(target: &mut AssistantMessage, source: &AssistantMessage) {
    let mut source_thinking = source.content.iter().filter_map(|block| match block {
        AssistantContent::Thinking(thinking) => Some(thinking),
        _ => None,
    });
    for target_block in &mut target.content {
        if let AssistantContent::Thinking(target_thinking) = target_block {
            let Some(source_block) = source_thinking.next() else {
                break;
            };
            target_thinking
                .thinking_signature
                .clone_from(&source_block.thinking_signature);
            target_thinking.redacted = source_block.redacted;
        }
    }

    let mut source_text = source.content.iter().filter_map(|block| match block {
        AssistantContent::Text(text) => Some(text),
        _ => None,
    });
    for target_block in &mut target.content {
        if let AssistantContent::Text(target_text) = target_block {
            let Some(source_block) = source_text.next() else {
                break;
            };
            target_text
                .text_signature
                .clone_from(&source_block.text_signature);
        }
    }
}

/// Build canonical ordered assistant blocks from parser accumulators
/// (tau `assistant_content`): a text block first (only if non-empty), then the
/// tool calls in order.
#[must_use]
pub fn assistant_content(text: &str, tool_calls: Vec<ToolCall>) -> Vec<AssistantContent> {
    let mut blocks: Vec<AssistantContent> = Vec::new();
    if !text.is_empty() {
        blocks.push(AssistantContent::Text(TextContent::new(text)));
    }
    blocks.extend(tool_calls.into_iter().map(AssistantContent::ToolCall));
    blocks
}

/// Build the adapter's assembled response message (tau's per-adapter
/// `AssistantMessage(content=..., usage=...)`), stamping the timestamp from the
/// injected clock so `done`/`error` events reproduce tau's frozen clock.
#[must_use]
pub fn assistant_message(
    content: Vec<AssistantContent>,
    usage: Usage,
    timestamp_ms: i64,
) -> AssistantMessage {
    let mut message = AssistantMessage::default();
    message.content = content;
    message.usage = usage;
    message.timestamp = timestamp_ms;
    message
}
