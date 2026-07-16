//! Provider-neutral tool result wire type.
//!
//! Port of the serialized surface of tau's `tau_agent/tools.py`. Only
//! [`AgentToolResult`] is a wire type (it appears inside tool-execution events);
//! the `AgentTool` executor machinery is behavior, not wire format, and lands in
//! a later milestone.

use serde::{Deserialize, Serialize};

use crate::messages::ToolResultContent;
use crate::types::JsonValue;

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
}
