//! Append-only session persistence (tau `tau_agent/session/`).
//!
//! [`entries`] holds the `SessionEntry` union; [`jsonl`] holds the line codec and
//! the Tau-v1 legacy migration; [`tree`] reconstructs branch paths from
//! `parent_id` pointers; [`memory`] replays the log into a [`memory::SessionState`];
//! [`storage`] is the append-only [`storage::SessionStorage`] trait plus its JSONL
//! implementation.

pub mod entries;
pub mod jsonl;
pub mod memory;
pub mod storage;
pub mod tree;
