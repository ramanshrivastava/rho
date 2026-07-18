//! Ratatui render widgets — the immediate-mode visual layer.
//!
//! tau's `widgets.py` is a tree of retained-mode Textual widgets that mutate in
//! place. rho re-derives the same *content, layout regions, and per-theme
//! colors* as pure functions of [`crate::state::TuiState`], rebuilt every frame.
//!
//! - [`style`] parses tau's theme style-strings into ratatui [`ratatui::style::Style`]s.
//! - [`transcript`] renders the streaming transcript (role blocks, markdown,
//!   tool call/result bodies) — the port of `TranscriptView` + `render_chat_item`.
//!
//! The status bar, footer hints, and input composer widgets live alongside these
//! and are composed by the app layer (`app.rs`).

pub mod style;
pub mod transcript;

pub use style::{RoleStyles, chat_role_styles, parse_color, parse_style, role_styles};
pub use transcript::{build_transcript_lines, render_transcript};
