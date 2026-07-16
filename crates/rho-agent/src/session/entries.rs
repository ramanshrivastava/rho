//! Append-only session entry models (tau `tau_agent/session/entries.py`).
//!
//! Unlike transcript messages, session-entry **top-level** keys are
//! `snake_case` (`parent_id`, `replaces_entry_ids`, `created_at`) — so these
//! structs carry **no** `rename_all` and let serde use the field names verbatim.
//! The wrapped `message` is still a `camelCase` [`AgentMessage`]: both casings
//! appear on the same JSONL line.
//!
//! The discriminator `type` is **not first** (`id`, `parent_id`, `timestamp`
//! precede it), which is exactly why an internally-tagged serde enum is
//! unusable here (it would hoist `type` to the front). We reproduce the order
//! with an untagged union whose variant structs place a `monostate::MustBe!`
//! `type` field in its true fourth position.
//!
//! Timestamps are **floats in seconds** (`1731234567.0`), distinct from the
//! integer-millisecond timestamps on messages. `serde_json` prints whole `f64`s
//! with a trailing `.0`, matching tau.

use std::time::{SystemTime, UNIX_EPOCH};

use monostate::MustBe;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::messages::AgentMessage;
use crate::types::JsonMap;

/// Return a fresh session-entry id (tau's `new_entry_id`: `uuid4().hex`).
///
/// `Uuid::new_v4().simple()` renders 32 lowercase hex digits with no hyphens,
/// exactly matching Python's `uuid4().hex`.
#[must_use]
pub fn new_entry_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// Current Unix timestamp in **seconds** as an `f64` (tau's `current_timestamp`).
///
/// Distinct from message timestamps, which are integer milliseconds.
#[must_use]
pub fn current_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A transcript message entry (tau `MessageEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("message"),
    /// The wrapped (camelCase) transcript message.
    pub message: AgentMessage,
}

/// A model-selection change entry (tau `ModelChangeEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelChangeEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("model_change"),
    /// The newly selected model id.
    pub model: String,
}

/// A thinking-level change entry (tau `ThinkingLevelChangeEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingLevelChangeEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("thinking_level_change"),
    /// The new thinking level (absent means "off").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

/// A compaction entry that replaces older entries during replay (tau
/// `CompactionEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("compaction"),
    /// The compaction summary text.
    pub summary: String,
    /// Ids of the entries this compaction replaces during replay.
    ///
    /// A list default; `exclude_none` omits only `None`, never an empty list, so
    /// this is always serialized (even `[]`).
    #[serde(default)]
    pub replaces_entry_ids: Vec<String>,
}

/// A branch summary entry (tau `BranchSummaryEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSummaryEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("branch_summary"),
    /// The branch summary text.
    pub summary: String,
    /// Root entry id of the summarized branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_root_id: Option<String>,
}

/// A human-readable session label entry (tau `LabelEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("label"),
    /// The human-readable label.
    pub label: String,
}

/// The active branch leaf pointer entry (tau `LeafEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeafEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("leaf"),
    /// The entry id this leaf points at (the active branch tip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
}

/// Basic session metadata entry (tau `SessionInfoEntry`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInfoEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("session_info"),
    /// Session creation time (Unix seconds, float).
    pub created_at: f64,
    /// Working directory the session was started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Session title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Extension/application-owned session data (tau `CustomEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomEntry {
    /// Unique entry id.
    pub id: String,
    /// Parent entry id (absent for roots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Entry timestamp (Unix seconds, float).
    pub timestamp: f64,
    #[serde(rename = "type")]
    kind: MustBe!("custom"),
    /// Owning extension/application namespace.
    pub namespace: String,
    /// Free-form namespaced data.
    #[serde(default)]
    pub data: JsonMap,
}

/// The append-only session entry union (tau `SessionEntry`, discriminated on
/// `type`).
///
/// `large_enum_variant` is allowed on purpose: this union mirrors tau's Pydantic
/// discriminated union 1:1, where some variants carry a full transcript message
/// and others are bare pointers — an inherent size imbalance. Boxing the heavy
/// variant would distort the ported shape for no M1 benefit; the memory trade-off
/// is revisited (with boxing) when the benchmark milestone lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum SessionEntry {
    /// Transcript message.
    Message(MessageEntry),
    /// Model change.
    ModelChange(ModelChangeEntry),
    /// Thinking-level change.
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    /// Compaction.
    Compaction(CompactionEntry),
    /// Branch summary.
    BranchSummary(BranchSummaryEntry),
    /// Label.
    Label(LabelEntry),
    /// Leaf pointer.
    Leaf(LeafEntry),
    /// Session metadata.
    SessionInfo(SessionInfoEntry),
    /// Custom extension data.
    Custom(CustomEntry),
}

impl SessionEntry {
    /// The entry's unique id (present on every variant via `BaseSessionEntry`).
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Message(e) => &e.id,
            Self::ModelChange(e) => &e.id,
            Self::ThinkingLevelChange(e) => &e.id,
            Self::Compaction(e) => &e.id,
            Self::BranchSummary(e) => &e.id,
            Self::Label(e) => &e.id,
            Self::Leaf(e) => &e.id,
            Self::SessionInfo(e) => &e.id,
            Self::Custom(e) => &e.id,
        }
    }

    /// The entry's parent id, if any (absent for roots).
    #[must_use]
    pub fn parent_id(&self) -> Option<&str> {
        let parent = match self {
            Self::Message(e) => &e.parent_id,
            Self::ModelChange(e) => &e.parent_id,
            Self::ThinkingLevelChange(e) => &e.parent_id,
            Self::Compaction(e) => &e.parent_id,
            Self::BranchSummary(e) => &e.parent_id,
            Self::Label(e) => &e.parent_id,
            Self::Leaf(e) => &e.parent_id,
            Self::SessionInfo(e) => &e.parent_id,
            Self::Custom(e) => &e.parent_id,
        };
        parent.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------
//
// Each entry auto-generates its `id` (uuid) and `timestamp` (now) via tau's
// factories; `parent_id` defaults to `None` and is set by the caller (it is
// `pub`). This mirrors tau's `BaseSessionEntry` default_factory fields.

impl MessageEntry {
    /// Build a message entry wrapping `message`.
    pub fn new(message: AgentMessage) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("message"),
            message,
        }
    }
}

impl ModelChangeEntry {
    /// Build a model-change entry.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("model_change"),
            model: model.into(),
        }
    }
}

impl ThinkingLevelChangeEntry {
    /// Build a thinking-level-change entry.
    pub fn new(thinking_level: Option<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("thinking_level_change"),
            thinking_level,
        }
    }
}

impl CompactionEntry {
    /// Build a compaction entry.
    pub fn new(summary: impl Into<String>, replaces_entry_ids: Vec<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("compaction"),
            summary: summary.into(),
            replaces_entry_ids,
        }
    }
}

impl BranchSummaryEntry {
    /// Build a branch-summary entry.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("branch_summary"),
            summary: summary.into(),
            branch_root_id: None,
        }
    }
}

impl LabelEntry {
    /// Build a label entry.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("label"),
            label: label.into(),
        }
    }
}

impl LeafEntry {
    /// Build a leaf-pointer entry.
    pub fn new(entry_id: Option<String>) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("leaf"),
            entry_id,
        }
    }
}

impl SessionInfoEntry {
    /// Build a session-info entry (`created_at` = now; `cwd`/`title` unset).
    pub fn new() -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("session_info"),
            created_at: current_timestamp(),
            cwd: None,
            title: None,
        }
    }
}

impl CustomEntry {
    /// Build a custom (extension-owned) entry.
    pub fn new(namespace: impl Into<String>, data: JsonMap) -> Self {
        Self {
            id: new_entry_id(),
            parent_id: None,
            timestamp: current_timestamp(),
            kind: MustBe!("custom"),
            namespace: namespace.into(),
            data,
        }
    }
}
