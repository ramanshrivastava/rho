//! In-memory session state reconstruction (tau `tau_agent/session/memory.py`).
//!
//! ## Replay, don't mutate
//!
//! A session is an append-only entry log; the current state is *derived* by
//! replaying it, never by mutating a live object. [`SessionState::from_entries`]
//! folds the log into messages + metadata. Compaction is a **replacement** during
//! replay (the summary stands in for the entries it replaces, in their original
//! position), and a branch summary appends a framed user message — exactly as
//! tau does. Because state is a pure function of the log, the same entries always
//! reproduce the same state.
//!
//! [`SessionState::from_entries_at_leaf`] restricts replay to a single
//! root-to-leaf path (via [`crate::session::tree::path_to_entry`]); `None` means
//! "the empty path before the first root entry" (tau's explicit-`None` case).

use crate::messages::{AgentMessage, UserMessage};
use crate::session::entries::{CompactionEntry, CustomEntry, SessionEntry, SessionInfoEntry};
use crate::session::tree::{SessionTreeError, path_to_entry};

/// Current session state derived from append-only entries (tau `SessionState`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    /// The replayed transcript (compaction/branch summaries folded in).
    pub messages: Vec<AgentMessage>,
    /// The last selected model, if any.
    pub model: Option<String>,
    /// The last thinking level, if any.
    pub thinking_level: Option<String>,
    /// The last label, if any.
    pub label: Option<String>,
    /// The active branch leaf id, if any.
    pub active_leaf_id: Option<String>,
    /// The most recent session-info entry, if any.
    pub session_info: Option<SessionInfoEntry>,
    /// Every custom entry replayed, in order.
    pub custom_entries: Vec<CustomEntry>,
    /// Every compaction entry replayed, in order.
    pub compaction_entries: Vec<CompactionEntry>,
    /// The entry id backing each surviving message row, aligned with `messages`.
    pub context_entry_ids: Vec<String>,
    /// The exact entries that were replayed.
    pub entries: Vec<SessionEntry>,
}

impl SessionState {
    /// Replay every entry in storage order (tau's default `from_entries`).
    #[must_use]
    pub fn from_entries(entries: &[SessionEntry]) -> Self {
        Self::replay(entries.to_vec(), None)
    }

    /// Replay only the root-to-leaf path for `leaf_id`.
    ///
    /// `None` replays the empty path (state before the first root entry); `Some`
    /// walks `parent_id` pointers from the leaf. Errors if the entries do not form
    /// a valid tree (tau raises `SessionTreeError`).
    pub fn from_entries_at_leaf(
        entries: &[SessionEntry],
        leaf_id: Option<&str>,
    ) -> Result<Self, SessionTreeError> {
        let replay_entries = match leaf_id {
            Some(id) => path_to_entry(entries, id)?,
            None => Vec::new(),
        };
        Ok(Self::replay(replay_entries, leaf_id.map(str::to_string)))
    }

    fn replay(replay_entries: Vec<SessionEntry>, resolved_leaf_id: Option<String>) -> Self {
        let mut message_rows: Vec<(String, AgentMessage)> = Vec::new();
        let mut model: Option<String> = None;
        let mut thinking_level: Option<String> = None;
        let mut label: Option<String> = None;
        let mut active_leaf_id: Option<String> = resolved_leaf_id;
        let mut session_info: Option<SessionInfoEntry> = None;
        let mut custom_entries: Vec<CustomEntry> = Vec::new();
        let mut compaction_entries: Vec<CompactionEntry> = Vec::new();

        for entry in &replay_entries {
            match entry {
                SessionEntry::Message(e) => {
                    message_rows.push((e.id.clone(), e.message.clone()));
                }
                SessionEntry::ModelChange(e) => model = Some(e.model.clone()),
                SessionEntry::ThinkingLevelChange(e) => {
                    thinking_level.clone_from(&e.thinking_level);
                }
                SessionEntry::Label(e) => label = Some(e.label.clone()),
                SessionEntry::Leaf(e) => active_leaf_id.clone_from(&e.entry_id),
                SessionEntry::SessionInfo(e) => session_info = Some(e.clone()),
                SessionEntry::Custom(e) => custom_entries.push(e.clone()),
                SessionEntry::Compaction(e) => {
                    compaction_entries.push(e.clone());
                    message_rows = apply_compaction(message_rows, e);
                }
                SessionEntry::BranchSummary(e) => {
                    message_rows.push((
                        e.id.clone(),
                        AgentMessage::User(UserMessage::new(format_branch_summary(&e.summary))),
                    ));
                }
            }
        }

        let (context_entry_ids, messages): (Vec<String>, Vec<AgentMessage>) =
            message_rows.into_iter().unzip();

        Self {
            messages,
            model,
            thinking_level,
            label,
            active_leaf_id,
            session_info,
            custom_entries,
            compaction_entries,
            context_entry_ids,
            entries: replay_entries,
        }
    }
}

/// Replace the entries a compaction covers with its summary, in position (tau
/// `_apply_compaction`).
///
/// The summary user-message lands where the first replaced entry sat; if none of
/// the current rows are covered, it is appended (matching tau's `inserted_summary`
/// fallback).
fn apply_compaction(
    message_rows: Vec<(String, AgentMessage)>,
    entry: &CompactionEntry,
) -> Vec<(String, AgentMessage)> {
    let replaced: std::collections::HashSet<&str> = entry
        .replaces_entry_ids
        .iter()
        .map(String::as_str)
        .collect();
    let mut retained: Vec<(String, AgentMessage)> = Vec::new();
    let mut inserted_summary = false;

    for (entry_id, message) in message_rows {
        if !replaced.contains(entry_id.as_str()) {
            retained.push((entry_id, message));
            continue;
        }
        if !inserted_summary {
            retained.push((
                entry.id.clone(),
                AgentMessage::User(UserMessage::new(format_compaction_summary(&entry.summary))),
            ));
            inserted_summary = true;
        }
    }

    if !inserted_summary {
        retained.push((
            entry.id.clone(),
            AgentMessage::User(UserMessage::new(format_compaction_summary(&entry.summary))),
        ));
    }
    retained
}

fn format_compaction_summary(summary: &str) -> String {
    format!("Previous conversation summary:\n{summary}")
}

fn format_branch_summary(summary: &str) -> String {
    format!(
        "The following is a summary of a branch that this conversation came back from:\n<summary>\n{summary}\n</summary>"
    )
}
