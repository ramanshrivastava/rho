//! Host-native payload types crossing the extension boundary.
//!
//! These mirror tau's `tau_coding.extensions.api` dataclasses, but are expressed
//! in host-neutral terms (plain data + `serde_json::Value` for free-form JSON)
//! so this crate stays free of any dependency on `rho-agent`'s message/tool
//! types. `rho-coding` maps between these payloads and `AgentTool` /
//! `AgentMessage` at the integration seam.
//!
//! The types deliberately match the `rho:extension` WIT records one-for-one; the
//! wasmtime host (see [`crate::wasm`]) converts between these and the generated
//! bindings, while [`crate::NoopExtensionHost`] and test doubles use them
//! directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool a guest registers during `init` (tau `AgentTool`, guest-shaped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDef {
    /// Tool name (unique; first registration wins).
    pub name: String,
    /// Display label.
    pub label: String,
    /// Model-facing description.
    pub description: String,
    /// JSON-schema object for the tool's parameters.
    pub parameters: Value,
    /// Optional system-prompt snippet contributed by the tool.
    pub prompt_snippet: Option<String>,
}

/// A slash command a guest registers during `init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDef {
    /// Command name (without the leading slash).
    pub name: String,
    /// User-facing description.
    pub description: String,
    /// Usage string (defaults to `/<name>` when absent).
    pub usage: Option<String>,
    /// Command aliases.
    pub aliases: Vec<String>,
}

/// Result of executing a guest-registered tool (host-neutral `AgentToolResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolCallResult {
    /// The result's text content.
    pub text: String,
    /// Optional structured details.
    pub details: Option<Value>,
}

/// `tool_call` hook event (tau `ToolCallHookEvent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallEvent {
    /// The tool about to execute.
    pub tool_name: String,
    /// The tool's arguments.
    pub arguments: Value,
}

/// `tool_call` hook outcome (tau `ToolCallHookResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolCallOutcome {
    /// Block execution; the model sees `reason` instead.
    pub block: bool,
    /// Optional block reason.
    pub reason: Option<String>,
    /// Replacement arguments; `None` leaves them unchanged.
    pub arguments: Option<Value>,
}

/// `tool_result` hook event (tau `ToolResultHookEvent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultEvent {
    /// The tool that executed.
    pub tool_name: String,
    /// The (possibly rewritten) arguments the tool ran with.
    pub arguments: Value,
    /// The result's text content.
    pub result_text: String,
    /// The result's structured details, if any.
    pub result_details: Option<Value>,
}

/// `tool_result` hook outcome (tau `ToolResultHookResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolResultOutcome {
    /// Override the result's text content.
    pub content: Option<String>,
    /// Override the result's structured details.
    pub details: Option<Value>,
}

/// `input` hook event (tau `InputEvent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Raw prompt text, before expansion.
    pub text: String,
    /// `"interactive"` or `"extension"`.
    pub source: String,
    /// `"steer"` / `"follow_up"`, or `None` on the idle prompt path.
    pub streaming_behavior: Option<String>,
}

/// The action an `input` hook requests (tau `InputHookResult.action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputAction {
    /// Leave the text unchanged.
    Continue,
    /// Replace the text with `text` (transforms chain across handlers).
    Transform,
    /// Consume the input entirely.
    Handled,
}

/// `input` hook outcome (tau `InputHookResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputOutcome {
    /// What the handler wants to do with the input.
    pub action: InputAction,
    /// Replacement text (for `Transform`).
    pub text: Option<String>,
    /// Optional message shown to the user (for `Handled`).
    pub message: Option<String>,
}

/// `turn_start` event (tau `TurnStartEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartEvent {
    /// Zero-based turn index within the agent run.
    pub turn_index: u32,
    /// Millisecond wall-clock timestamp.
    pub timestamp: u64,
}

/// `turn_end` event (tau `TurnEndEvent`); message + results carried as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEndEvent {
    /// Zero-based turn index within the agent run.
    pub turn_index: u32,
    /// The assistant message for the turn, serialized.
    pub message: Value,
    /// The tool-result messages for the turn, serialized.
    pub tool_results: Value,
}

/// A lifecycle event (`session_start` / `session_shutdown`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleEvent {
    /// `"startup" | "reload" | "new" | "resume" | "branch" | "quit"`.
    pub reason: String,
}

/// A generic agent event forwarded to `agent_event` wildcard subscribers, and to
/// per-type subscribers by its `type` tag (tau's canonical event fan-out).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHookEvent {
    /// The canonical event `type` (e.g. `"message_end"`).
    pub event_type: String,
    /// The full event, serialized to JSON.
    pub payload: Value,
}
