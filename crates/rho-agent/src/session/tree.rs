//! Session tree traversal helpers (tau `tau_agent/session/tree.py`).
//!
//! Branch paths are reconstructed by walking `parent_id` pointers from a leaf
//! back to the root, then reversing — a faithful port of `path_to_entry`, cycle
//! and missing-parent detection included.

use std::collections::{HashMap, HashSet};

use crate::session::entries::SessionEntry;

/// Session entries do not form a valid traversable tree (tau `SessionTreeError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionTreeError {
    /// Two entries share an id.
    #[error("Duplicate session entry id: {0}")]
    DuplicateId(String),
    /// A referenced entry id is absent.
    #[error("Missing session entry: {0}")]
    MissingEntry(String),
    /// A `parent_id` chain loops back on itself.
    #[error("Cycle detected at session entry: {0}")]
    Cycle(String),
}

/// Index entries by id, rejecting duplicates (tau `entries_by_id`).
pub fn entries_by_id(
    entries: &[SessionEntry],
) -> Result<HashMap<&str, &SessionEntry>, SessionTreeError> {
    let mut result: HashMap<&str, &SessionEntry> = HashMap::new();
    for entry in entries {
        if result.contains_key(entry.id()) {
            return Err(SessionTreeError::DuplicateId(entry.id().to_string()));
        }
        result.insert(entry.id(), entry);
    }
    Ok(result)
}

/// Return the root-to-leaf path for `leaf_id` (tau `path_to_entry`).
///
/// Walks `parent_id` from the leaf to the root, detecting cycles and missing
/// entries, then reverses so the result is root-first.
pub fn path_to_entry(
    entries: &[SessionEntry],
    leaf_id: &str,
) -> Result<Vec<SessionEntry>, SessionTreeError> {
    let by_id = entries_by_id(entries)?;
    let mut path: Vec<SessionEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current: Option<String> = Some(leaf_id.to_string());

    while let Some(id) = current {
        if seen.contains(&id) {
            return Err(SessionTreeError::Cycle(id));
        }
        seen.insert(id.clone());
        let Some(entry) = by_id.get(id.as_str()) else {
            return Err(SessionTreeError::MissingEntry(id));
        };
        path.push((*entry).clone());
        current = entry.parent_id().map(str::to_string);
    }

    path.reverse();
    Ok(path)
}
