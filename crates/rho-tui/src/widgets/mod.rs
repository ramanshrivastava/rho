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

pub mod composer;
pub mod footer;
pub mod sidebar;
pub mod status;
pub mod style;
pub mod transcript;

pub use composer::{
    build_completion_lines, build_working_status_line, prompt_prefix, render_completion_popup,
    render_prompt_prefix, render_queued_messages, render_working_status,
};
pub use footer::{FooterMode, footer_hints, render_footer};
pub use sidebar::{SidebarInfo, render_sidebar};
pub use status::{
    StatusInfo, build_compact_session_info, context_file_label, render_compact_session_info,
};
pub use style::{RoleStyles, chat_role_styles, parse_color, parse_style, role_styles};
pub use transcript::{
    TranscriptCache, bench_brag_line, build_transcript_lines, render_splash, render_transcript,
    should_show_splash, transcript_is_empty,
};
