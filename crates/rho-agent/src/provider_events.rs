//! Pi-compatible assistant stream events (tau `tau_agent/provider_events.py`).
//!
//! Same idioms as [`crate::messages`]: `camelCase` keys, `snake_case` tag
//! *values* (`text_delta`, `toolcall_end`, …), untagged union + `monostate`
//! discriminator on `type`. Every variant carries a `partial`/`message`
//! snapshot of the assistant message built so far.

use monostate::MustBe;
use serde::{Deserialize, Serialize};

use crate::messages::{AssistantMessage, ToolCall};

/// `start` — the stream opened with an (empty) partial message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("start"),
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `text_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("text_start"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `text_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextDeltaEvent {
    #[serde(rename = "type")]
    kind: MustBe!("text_delta"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Incremental text.
    pub delta: String,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `text_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("text_end"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// The finished text.
    pub content: String,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `thinking_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThinkingStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking_start"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `thinking_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThinkingDeltaEvent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking_delta"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Incremental reasoning text.
    pub delta: String,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `thinking_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThinkingEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking_end"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// The finished reasoning text.
    pub content: String,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `toolcall_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("toolcall_start"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `toolcall_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallDeltaEvent {
    #[serde(rename = "type")]
    kind: MustBe!("toolcall_delta"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// Incremental raw tool-argument JSON.
    pub delta: String,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// `toolcall_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("toolcall_end"),
    /// Index of the content block being streamed.
    pub content_index: i64,
    /// The completed tool call.
    pub tool_call: ToolCall,
    /// Assistant message so far.
    pub partial: AssistantMessage,
}

/// Terminal reason for a successful stream (tau `DoneReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
    /// Natural stop.
    Stop,
    /// Hit max length.
    Length,
    /// Stopped to run tools.
    ToolUse,
}

/// Terminal reason for a failed stream (tau `ErrorReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    /// Aborted by the user.
    Aborted,
    /// Errored.
    Error,
}

/// `done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantDoneEvent {
    #[serde(rename = "type")]
    kind: MustBe!("done"),
    /// Why the stream finished.
    pub reason: DoneReason,
    /// The completed assistant message.
    pub message: AssistantMessage,
}

/// `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantErrorEvent {
    #[serde(rename = "type")]
    kind: MustBe!("error"),
    /// Why the stream failed.
    pub reason: ErrorReason,
    /// The assistant message carrying the error.
    pub error: AssistantMessage,
}

/// The assistant stream event union (tau `AssistantMessageEvent`,
/// discriminated on `type`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantMessageEvent {
    /// Stream opened.
    Start(AssistantStartEvent),
    /// Text block opened.
    TextStart(TextStartEvent),
    /// Text delta.
    TextDelta(TextDeltaEvent),
    /// Text block closed.
    TextEnd(TextEndEvent),
    /// Thinking block opened.
    ThinkingStart(ThinkingStartEvent),
    /// Thinking delta.
    ThinkingDelta(ThinkingDeltaEvent),
    /// Thinking block closed.
    ThinkingEnd(ThinkingEndEvent),
    /// Tool call opened.
    ToolCallStart(ToolCallStartEvent),
    /// Tool call delta.
    ToolCallDelta(ToolCallDeltaEvent),
    /// Tool call closed.
    ToolCallEnd(ToolCallEndEvent),
    /// Stream finished successfully.
    Done(AssistantDoneEvent),
    /// Stream failed.
    Error(AssistantErrorEvent),
}
