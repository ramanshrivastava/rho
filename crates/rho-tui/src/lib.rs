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
//! The crate is built immediate-mode: every frame is rebuilt from the pure
//! [`state::TuiState`], which the [`adapter::TuiEventAdapter`] mutates from the
//! session event stream. See `dev-notes/phase-5.md` for the retained-mode
//! (Textual) → immediate-mode (ratatui) re-derivation notes.
//!
//! Layering: depends on [`rho_coding`] (+ `rho_agent` for the wire types the
//! state/adapter consume). `rho-coding` does **not** depend on ratatui.

pub mod adapter;
pub mod app;
pub mod autocomplete;
pub mod ext_ui;
pub mod login;
pub mod modals;
pub mod motion;
mod pystr;
pub mod state;
pub mod terminal_title;
pub mod theme;
pub mod widgets;

pub use adapter::TuiEventAdapter;
pub use autocomplete::{
    CompletionInputs, CompletionItem, CompletionOption, CompletionState, build_completion_state,
};
pub use state::{ChatItem, ChatItemRole, TuiState};
pub use terminal_title::{TerminalTitleController, build_terminal_title};
pub use theme::{
    BUILTIN_TUI_THEME_NAMES, TuiConfigError, TuiKeybindings, TuiSettings, TuiTheme, TuiThemeName,
    get_tui_theme, load_tui_settings, save_tui_settings,
};
