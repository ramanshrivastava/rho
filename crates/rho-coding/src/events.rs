//! Coding-session events (port of tau's `tau_coding/events.py`).
//!
//! [`CodingSessionEvent`] is tau's `AgentEvent | SessionOwnEvent`. The
//! print-mode slice emits [`CodingSessionEvent::Agent`] (the harness stream);
//! the [`CodingSession`](crate::session::CodingSession) emits the session-owned
//! variants (`agent_settled`, `queue_update`, `compaction_*`, `auto_retry_*`,
//! `entry_appended`, …).
//!
//! These use the same `camelCase` + `exclude_none` idioms as the agent wire
//! types and round-trip byte-for-byte against `fixtures/wire/session-events/`.

use monostate::MustBe;
use serde::{Deserialize, Serialize};

use rho_agent::events::AgentEvent;
use rho_agent::messages::AgentMessage;
use rho_agent::session::entries::SessionEntry;

/// `agent_end` (session-owned; carries the final message list + retry flag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgentEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_end"),
    /// Final message list.
    #[serde(default)]
    pub messages: Vec<AgentMessage>,
    /// Whether the session will retry.
    #[serde(default)]
    pub will_retry: bool,
}

impl SessionAgentEndEvent {
    /// Build an `agent_end` session event.
    #[must_use]
    pub fn new(messages: Vec<AgentMessage>, will_retry: bool) -> Self {
        Self {
            kind: MustBe!("agent_end"),
            messages,
            will_retry,
        }
    }
}

/// `agent_settled`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettledEvent {
    #[serde(rename = "type")]
    kind: MustBe!("agent_settled"),
}

impl AgentSettledEvent {
    /// Build an `agent_settled` event.
    #[must_use]
    pub fn new() -> Self {
        Self {
            kind: MustBe!("agent_settled"),
        }
    }
}

impl Default for AgentSettledEvent {
    fn default() -> Self {
        Self::new()
    }
}

/// `queue_update` (steering / follow-up snapshots, always serialized).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueUpdateEvent {
    #[serde(rename = "type")]
    kind: MustBe!("queue_update"),
    /// Queued steering messages.
    #[serde(default)]
    pub steering: Vec<String>,
    /// Queued follow-up messages (serialized as `followUp`).
    #[serde(default)]
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

/// tau `CompactionReason = Literal["manual", "threshold", "overflow"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// User-triggered compaction.
    Manual,
    /// Automatic threshold-triggered compaction.
    Threshold,
    /// Post-overflow recovery compaction.
    Overflow,
}

/// `compaction_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStartEvent {
    #[serde(rename = "type")]
    kind: MustBe!("compaction_start"),
    /// Why compaction was triggered.
    pub reason: CompactionReason,
}

impl CompactionStartEvent {
    /// Build a `compaction_start` event.
    #[must_use]
    pub fn new(reason: CompactionReason) -> Self {
        Self {
            kind: MustBe!("compaction_start"),
            reason,
        }
    }
}

/// `compaction_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("compaction_end"),
    /// Why compaction was triggered.
    pub reason: CompactionReason,
    /// Optional free-form result payload (`object | None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Whether the compaction was aborted.
    #[serde(default)]
    pub aborted: bool,
    /// Whether the session will retry after this compaction.
    #[serde(default)]
    pub will_retry: bool,
    /// Error message, if the compaction failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl CompactionEndEvent {
    /// Build a `compaction_end` event.
    #[must_use]
    pub fn new(reason: CompactionReason) -> Self {
        Self {
            kind: MustBe!("compaction_end"),
            reason,
            result: None,
            aborted: false,
            will_retry: false,
            error_message: None,
        }
    }
}

/// `entry_appended` (a durable session entry was written).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryAppendedEvent {
    #[serde(rename = "type")]
    kind: MustBe!("entry_appended"),
    /// The entry that was appended.
    pub entry: SessionEntry,
}

impl EntryAppendedEvent {
    /// Build an `entry_appended` event.
    #[must_use]
    pub fn new(entry: SessionEntry) -> Self {
        Self {
            kind: MustBe!("entry_appended"),
            entry,
        }
    }
}

/// `session_info_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoChangedEvent {
    #[serde(rename = "type")]
    kind: MustBe!("session_info_changed"),
    /// New session name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl SessionInfoChangedEvent {
    /// Build a `session_info_changed` event.
    #[must_use]
    pub fn new(name: Option<String>) -> Self {
        Self {
            kind: MustBe!("session_info_changed"),
            name,
        }
    }
}

/// `thinking_level_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangedEvent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking_level_changed"),
    /// The new thinking level.
    pub level: String,
}

impl ThinkingLevelChangedEvent {
    /// Build a `thinking_level_changed` event.
    #[must_use]
    pub fn new(level: impl Into<String>) -> Self {
        Self {
            kind: MustBe!("thinking_level_changed"),
            level: level.into(),
        }
    }
}

/// `auto_retry_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRetryEndEvent {
    #[serde(rename = "type")]
    kind: MustBe!("auto_retry_end"),
    /// Whether the retry ultimately succeeded.
    pub success: bool,
    /// The attempt count reached.
    pub attempt: i64,
    /// The final error, if it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_error: Option<String>,
}

impl AutoRetryEndEvent {
    /// Build an `auto_retry_end` event.
    #[must_use]
    pub fn new(success: bool, attempt: i64, final_error: Option<String>) -> Self {
        Self {
            kind: MustBe!("auto_retry_end"),
            success,
            attempt,
            final_error,
        }
    }
}

/// The session-owned event union (tau `SessionOwnEvent`).
///
/// The size imbalance (bare-tag `AgentSettled` next to message-carrying
/// variants) mirrors tau's Pydantic union — the same 1:1-port trade-off the
/// agent `AgentEvent` union documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum SessionOwnEvent {
    /// Session agent-end.
    SessionAgentEnd(SessionAgentEndEvent),
    /// Agent settled.
    AgentSettled(AgentSettledEvent),
    /// Queue update.
    QueueUpdate(QueueUpdateEvent),
    /// Compaction started.
    CompactionStart(CompactionStartEvent),
    /// Compaction ended.
    CompactionEnd(CompactionEndEvent),
    /// A durable entry was appended.
    EntryAppended(EntryAppendedEvent),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl From<SessionOwnEvent> for CodingSessionEvent {
    fn from(event: SessionOwnEvent) -> Self {
        Self::Session(event)
    }
}

macro_rules! session_own_from {
    ($($ty:ident => $variant:ident),+ $(,)?) => {
        $(
            impl From<$ty> for SessionOwnEvent {
                fn from(event: $ty) -> Self {
                    Self::$variant(event)
                }
            }
            impl From<$ty> for CodingSessionEvent {
                fn from(event: $ty) -> Self {
                    Self::Session(SessionOwnEvent::$variant(event))
                }
            }
        )+
    };
}

session_own_from! {
    SessionAgentEndEvent => SessionAgentEnd,
    AgentSettledEvent => AgentSettled,
    QueueUpdateEvent => QueueUpdate,
    CompactionStartEvent => CompactionStart,
    CompactionEndEvent => CompactionEnd,
    EntryAppendedEvent => EntryAppended,
    SessionInfoChangedEvent => SessionInfoChanged,
    ThinkingLevelChangedEvent => ThinkingLevelChanged,
    AutoRetryStartEvent => AutoRetryStart,
    AutoRetryEndEvent => AutoRetryEnd,
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

    #[test]
    fn compaction_end_omits_none_result_and_error() {
        let event = CompactionEndEvent::new(CompactionReason::Threshold);
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"compaction_end","reason":"threshold","aborted":false,"willRetry":false}"#
        );
    }
}
