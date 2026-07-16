//! Append-only session persistence (tau `tau_agent/session/`).
//!
//! [`entries`] holds the `SessionEntry` union; [`jsonl`] holds the line codec and
//! the Tau-v1 legacy migration.

pub mod entries;
pub mod jsonl;
