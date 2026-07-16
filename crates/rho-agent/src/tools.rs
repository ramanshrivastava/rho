//! Provider-neutral tool definitions and execution results (tau
//! `tau_agent/tools.py`).
//!
//! [`AgentToolResult`] is the wire type (it appears inside tool-execution
//! events). [`AgentTool`] is the *behavior* type the loop executes — carried
//! here (not just the wire result) as of M2, since the loop needs to invoke
//! tools. Its executor is an async closure returning a [`AgentToolResult`], with
//! a synchronous progress callback ([`ToolUpdateCallback`]) and a polled
//! [`CancellationToken`].
//!
//! ## Errors are data
//!
//! tau's loop wraps tool execution in `except Exception` and turns any failure
//! into an `is_error` result (never propagating out of the loop). Python uses
//! exceptions for that control flow; rho models it as a `Result`: the executor
//! returns `Result<AgentToolResult, ToolError>`, and the loop maps `Err(e)` to
//! `_error_result(e)` with `is_error = true`. Genuine Rust panics are *not*
//! caught — unlike a Python `Exception`, a panic signals a bug, not a tool-level
//! failure. (tau re-raises `asyncio.CancelledError`; rho has no analogue because
//! cancellation is polled, not thrown.)

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::messages::{TextContent, ToolResultContent};
use crate::provider::CancellationToken;
use crate::types::{JsonMap, JsonValue};

/// Final or partial result produced by a tool (tau `AgentToolResult`).
///
/// `content` is always serialized; `details`/`added_tool_names`/`terminate` are
/// omitted when `None` (`exclude_none`). Note `details` distinguishes an empty
/// object `{}` (present) from absent — the fixtures include both, so it is an
/// `Option<JsonValue>`, and an explicit `{}` round-trips as `{}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentToolResult {
    /// Result content blocks. Always serialized (even `[]`).
    ///
    /// Accepts tau's convenience shape on input: a bare string normalizes to a
    /// single text block (empty string → `[]`), matching
    /// `AgentToolResult._normalize_text_content`.
    #[serde(default, deserialize_with = "crate::messages::string_or_blocks")]
    pub content: Vec<ToolResultContent>,
    /// Free-form details; omitted when `None`, `{}` preserved when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Tools this result dynamically added to the toolset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Whether this result should terminate the agent loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl AgentToolResult {
    /// Build a tool result from content blocks (other fields default to `None`).
    pub fn new(content: Vec<ToolResultContent>) -> Self {
        Self {
            content,
            ..Self::default()
        }
    }

    /// Concatenated text of every [`TextContent`] block (tau's `.text`).
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ToolResultContent::Text(t) => Some(t.text.as_str()),
                ToolResultContent::Image(_) => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// AgentTool — the behavior the loop executes
// ---------------------------------------------------------------------------

/// A tool execution failed. tau raises an `Exception`; rho returns this as data.
///
/// The loop maps it to an `is_error` tool result carrying the message (tau's
/// `_error_result(str(exc))`).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

impl From<String> for ToolError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Synchronous progress callback a tool may invoke during execution (tau
/// `ToolUpdateCallback`).
///
/// Updates are buffered and replayed as `tool_execution_update` events *after*
/// the tool returns (matching tau's `_run_tool`), so this is a plain sync `Fn`.
pub type ToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

/// A tool's async executor (tau `ToolExecutor`).
///
/// Sync-returns a boxed future — mirroring tau's `Awaitable`-returning callable —
/// so it composes into the loop's `async-stream` body. See the module docs for
/// the "errors are data" `Result` contract.
pub type ToolExecutor = Arc<
    dyn Fn(
            String,                             // tool_call_id
            JsonMap,                            // arguments
            Option<Arc<dyn CancellationToken>>, // signal
            ToolUpdateCallback,                 // on_update
        ) -> BoxFuture<'static, Result<AgentToolResult, ToolError>>
        + Send
        + Sync,
>;

/// How a tool's calls are scheduled (tau `ToolExecutionMode`).
///
/// **Parity note:** tau carries this on every tool (default `parallel`) but
/// `run_agent_loop` executes tool calls **strictly sequentially regardless** —
/// there is no parallel path in `tau_agent.loop`. rho preserves the field for the
/// provider/coding layers (M3/M4) that read it, and matches the loop's sequential
/// behavior. See `dev-notes/phase-2.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionMode {
    /// Calls run one after another.
    Sequential,
    /// tau's default; still sequential in the loop (see the note above).
    #[default]
    Parallel,
}

/// A tool exposed to the portable agent loop (tau `AgentTool`).
///
/// Only the fields the loop needs are modeled here; the renderer / argument-prep
/// hooks tau carries are provider- and coding-layer concerns (M3/M4). `Clone` is
/// cheap: the executor is an `Arc`.
#[derive(Clone)]
pub struct AgentTool {
    /// Tool name (the key the loop dispatches on).
    pub name: String,
    /// Human-readable label.
    pub label: String,
    /// Tool description.
    pub description: String,
    /// JSON-schema parameters (tau `parameters` / `input_schema`).
    pub parameters: JsonMap,
    /// The async executor.
    pub execute_fn: ToolExecutor,
    /// Optional prompt snippet.
    pub prompt_snippet: Option<String>,
    /// Optional prompt guidelines.
    pub prompt_guidelines: Vec<String>,
    /// Scheduling mode (see [`ToolExecutionMode`]).
    pub execution_mode: ToolExecutionMode,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

impl AgentTool {
    /// Build a tool from a name, label, description, parameters, and executor.
    /// Optional fields default (`prompt_snippet` none, guidelines empty,
    /// `execution_mode` = `Parallel`), matching tau's dataclass defaults.
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonMap,
        execute_fn: ToolExecutor,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            parameters,
            execute_fn,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
            execution_mode: ToolExecutionMode::Parallel,
        }
    }

    /// Alias used by provider payload builders (tau `input_schema`).
    #[must_use]
    pub fn input_schema(&self) -> &JsonMap {
        &self.parameters
    }

    /// Execute one validated tool call (tau `AgentTool.execute`).
    pub fn execute(
        &self,
        tool_call_id: String,
        arguments: JsonMap,
        signal: Option<Arc<dyn CancellationToken>>,
        on_update: ToolUpdateCallback,
    ) -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
        (self.execute_fn)(tool_call_id, arguments, signal, on_update)
    }
}

/// Convenience: an error result with a single text block and `details = {}`
/// (tau `_error_result`). Public because both the loop and harness build these.
#[must_use]
pub fn error_result(message: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContent::Text(TextContent::new(message))],
        details: Some(JsonValue::Object(serde_json::Map::new())),
        added_tool_names: None,
        terminate: None,
    }
}
