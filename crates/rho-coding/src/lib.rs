//! `rho-coding` — the coding-agent application layer (port of tau's `tau_coding`).
//!
//! M4a lands the first runnable vertical slice:
//!
//! - [`tools`] — the four built-in coding tools (`read`/`write`/`edit`/`bash`)
//!   with tau-parity truncation, image handling, per-file locking, a faithful
//!   `difflib` port for `edit`, and process-group-killing `bash`.
//! - [`system_prompt`] — deterministic Pi-style system-prompt assembly.
//! - [`events`] — the `CodingSessionEvent` union rendered by print mode.
//! - [`rendering`] — the `text` / `json` / `transcript` print-mode renderers.
//! - [`print_mode`] — the harness-driven `rho -p` slice.
//!
//! Deferred to M4b (the full `CodingSession` surface): session persistence,
//! slash/terminal commands, project-context discovery, skills, extensions, the
//! provider catalog, OAuth, and HTML export. See `dev-notes/phase-4a.md`.

pub mod branch_summary;
pub mod context;
pub mod context_window;
pub mod diagnostics;
pub mod events;
mod fmt_util;
pub mod paths;
pub mod print_mode;
mod pystr;
pub mod rendering;
pub mod resources;
pub mod session;
pub mod session_manager;
pub mod system_prompt;
pub mod thinking;
pub mod tools;

pub use events::{CodingSessionEvent, SessionOwnEvent};
pub use print_mode::{
    MemorySessionStorage, PrintModeConfig, SessionPrintModeConfig, run_print_mode,
    run_session_print_mode,
};
pub use rendering::{
    EventRenderer, FinalTextRenderer, JsonEventRenderer, PrintOutputMode, TranscriptRenderer,
    create_event_renderer,
};
pub use session::{
    CodingSession, CodingSessionConfig, SessionError, StreamingBehavior, jsonl_session_storage,
    parse_terminal_command,
};
pub use session_manager::{CodingSessionRecord, SessionManager};
pub use system_prompt::{BuildSystemPromptOptions, Date, build_system_prompt};
pub use tools::{
    create_bash_tool, create_coding_tools, create_edit_tool, create_read_tool, create_write_tool,
};
