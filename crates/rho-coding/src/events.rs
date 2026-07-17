//! Coding-session events consumed by the renderers (port of tau's
//! `tau_coding/events.py`).
//!
//! [`CodingSessionEvent`] is tau's `AgentEvent | SessionOwnEvent`. The M4a
//! print-mode slice only ever emits [`CodingSessionEvent::Agent`] (the harness
//! stream); the session-owned variants exist so the renderers — and the ported
//! rendering tests — can handle the full surface. These are **serialize-only**
//! (the JSON renderer dumps them; nothing parses them back in M4a), using the
//! same `camelCase` + `exclude_none` idioms as the agent wire types.

use monostate::MustBe;
use serde::Serialize;

use rho_agent::events::AgentEvent;
use rho_agent::messages::AgentMessage;

/// `agent_end` (session-owned; carries the final message list + retry flag).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgentEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_end"),
    /// Final message list.
    pub messages: Vec<AgentMessage>,
    /// Whether the session will retry.
    pub will_retry: bool,
}

/// `agent_settled`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettledEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_settled"),
}

/// `queue_update` (steering / follow-up snapshots, always serialized).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueUpdateEvent {
    #[serde(rename = "type")]
    kind: MustBe!("queue_update"),
    /// Queued steering messages.
    pub steering: Vec<String>,
    /// Queued follow-up messages (serialized as `followUp`).
    pub follow_up: Vec<String>,
}

impl QueueUpdateEvent {
    /// Build a `queue_update` event.
    #[must_use]
    pub fn new(steering: Vec<String>, follow_up: Vec<String>) -> Self {
        Self {
            kind: MustBe!("queue_update"),
            steering,
            follow_up,
        }
    }
}

/// `session_info_changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoChangedEvent {
    #[serde(rename = "type")]
    kind: MustBe!("session_info_changed"),
    /// New session name, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `thinking_level_changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangedEvent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking_level_changed"),
    /// The new thinking level.
    pub level: String,
}

/// `auto_retry_start`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRetryStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("auto_retry_start"),
    /// Retry attempt number.
    pub attempt: i64,
    /// Maximum attempts.
    pub max_attempts: i64,
    /// Delay before this attempt (ms).
    pub delay_ms: i64,
    /// The error that triggered the retry.
    pub error_message: String,
}

impl AutoRetryStartEvent {
    /// Build an `auto_retry_start` event.
    #[must_use]
    pub fn new(attempt: i64, max_attempts: i64, delay_ms: i64, error_message: String) -> Self {
        Self {
            kind: MustBe!("auto_retry_start"),
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        }
    }
}

/// `auto_retry_end`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRetryEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("auto_retry_end"),
    /// Whether the retry ultimately succeeded.
    pub success: bool,
    /// The attempt count reached.
    pub attempt: i64,
    /// The final error, if it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_error: Option<String>,
}

/// The session-owned event union (tau `SessionOwnEvent`). Serialize-only in M4a.
///
/// `EntryAppendedEvent`, `CompactionStart`/`CompactionEnd` are deferred to M4b
/// (they belong to the `CodingSession` machinery this slice does not build).
///
/// The size imbalance (bare-tag `AgentSettled` next to message-carrying
/// variants) mirrors tau's Pydantic union — the same 1:1-port trade-off the
/// agent `AgentEvent` union documents.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum SessionOwnEvent {
    /// Session agent-end.
    SessionAgentEnd(SessionAgentEndEvent),
    /// Agent settled.
    AgentSettled(AgentSettledEvent),
    /// Queue update.
    QueueUpdate(QueueUpdateEvent),
    /// Session info changed.
    SessionInfoChanged(SessionInfoChangedEvent),
    /// Thinking level changed.
    ThinkingLevelChanged(ThinkingLevelChangedEvent),
    /// Auto-retry started.
    AutoRetryStart(AutoRetryStartEvent),
    /// Auto-retry ended.
    AutoRetryEnd(AutoRetryEndEvent),
}

/// A coding-session event (tau `CodingSessionEvent = AgentEvent | SessionOwnEvent`).
///
/// Serializes as the inner event (untagged), so the JSON renderer emits exactly
/// what tau's `model_dump_json(by_alias=True, exclude_none=True)` produces.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum CodingSessionEvent {
    /// A core agent-loop event.
    Agent(AgentEvent),
    /// A session-owned event.
    Session(SessionOwnEvent),
}

impl From<AgentEvent> for CodingSessionEvent {
    fn from(event: AgentEvent) -> Self {
        Self::Agent(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_update_serializes_with_camel_case_arrays() {
        let event = QueueUpdateEvent::new(vec!["adjust".into()], vec!["after".into()]);
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"queue_update","steering":["adjust"],"followUp":["after"]}"#
        );
    }
}
