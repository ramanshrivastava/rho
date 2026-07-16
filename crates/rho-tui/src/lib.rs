//! `rho-tui` — the interactive terminal UI (port of tau's Textual `tui` package).
//!
//! This crate will host the [`ratatui`](https://docs.rs/ratatui)-based
//! application that drives a [`rho_coding`] `CodingSession`: the transcript view,
//! the composer, autocomplete, the session/model selection modals, thinking
//! controls, and the terminal-title integration.
//!
//! The port targets visual parity with tau's TUI, validated in M5 against golden
//! render snapshots.
//!
//! Layering: depends on [`rho_coding`].
//!
//! Milestone M0 ships this crate as an empty scaffold; the TUI lands in M5.
