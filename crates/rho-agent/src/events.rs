//! Pi-compatible agent-level events (tau `tau_agent/events.py`).
//!
//! The `AgentEvent` union the agent loop emits. Tag *values* are `snake_case`
//! (`agent_start`, `tool_execution_end`) while field *keys* are `camelCase`
//! (`toolCallId`, `assistantMessageEvent`) — the same split as messages. Same
//! untagged + `monostate` idiom throughout.

use monostate::MustBe;
use serde::{Deserialize, Serialize};

use crate::messages::{AgentMessage, ToolResultMessage};
use crate::provider_events::AssistantMessageEvent;
use crate::tools::AgentToolResult;
use crate::types::JsonMap;

/// `agent_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_start"),
}

/// `agent_end` — carries the full final message list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_end"),
    /// Final message list produced by the run.
    #[serde(default)]
    pub messages: Vec<AgentMessage>,
}

/// `turn_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("turn_start"),
}

/// `turn_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("turn_end"),
    /// The assistant message that ended the turn.
    pub message: AgentMessage,
    /// Tool results produced during the turn.
    #[serde(default)]
    pub tool_results: Vec<ToolResultMessage>,
}

/// `message_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("message_start"),
    /// The message being started.
    pub message: AgentMessage,
}

/// `message_update` — a message snapshot plus the provider event that produced it.
///
/// tau declares `serialization_alias="assistantMessageEvent"`, which is exactly
/// what `camelCase` produces from `assistant_message_event`, so no explicit
/// rename is required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageUpdateEvent {
    #[serde(rename = "type")]
    kind: MustBe!("message_update"),
    /// The message snapshot after applying the provider event.
    pub message: AgentMessage,
    /// The provider event that produced this update.
    pub assistant_message_event: AssistantMessageEvent,
}

/// `message_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("message_end"),
    /// The completed message.
    pub message: AgentMessage,
}

/// `tool_execution_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolExecutionStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("tool_execution_start"),
    /// The tool call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The call arguments.
    #[serde(default)]
    pub args: JsonMap,
}

/// `tool_execution_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolExecutionUpdateEvent {
    #[serde(rename = "type")]
    kind: MustBe!("tool_execution_update"),
    /// The tool call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The call arguments.
    #[serde(default)]
    pub args: JsonMap,
    /// The partial result so far.
    pub partial_result: AgentToolResult,
}

/// `tool_execution_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolExecutionEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("tool_execution_end"),
    /// The tool call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The final result.
    pub result: AgentToolResult,
    /// Whether the tool failed.
    pub is_error: bool,
}

/// The agent-level event union (tau `AgentEvent`, discriminated on `type`).
///
/// `large_enum_variant` is allowed on purpose: bare tag events (`agent_start`)
/// sit alongside events carrying a full message snapshot, an inherent imbalance
/// in this 1:1 port of tau's Pydantic union. See the note on
/// [`crate::session::entries::SessionEntry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum AgentEvent {
    /// Agent loop started.
    AgentStart(AgentStartEvent),
    /// Agent loop ended.
    AgentEnd(AgentEndEvent),
    /// Turn started.
    TurnStart(TurnStartEvent),
    /// Turn ended.
    TurnEnd(TurnEndEvent),
    /// Message started.
    MessageStart(MessageStartEvent),
    /// Message updated by a provider event.
    MessageUpdate(MessageUpdateEvent),
    /// Message ended.
    MessageEnd(MessageEndEvent),
    /// Tool execution started.
    ToolExecutionStart(ToolExecutionStartEvent),
    /// Tool execution progress update.
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    /// Tool execution ended.
    ToolExecutionEnd(ToolExecutionEndEvent),
}
