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
pub mod catalog_loader;
pub mod commands;
pub mod context;
pub mod context_window;
pub mod credentials;
pub mod diagnostics;
pub mod events;
pub mod extensions;
mod fmt_util;
pub mod login_required;
pub mod oauth;
pub mod oauth_anthropic;
pub mod oauth_device;
pub mod oauth_github_copilot;
pub mod oauth_http;
pub mod oauth_registry;
pub mod oauth_types;
pub mod paths;
pub mod print_mode;
pub mod prompt_templates;
pub mod provider_catalog;
pub mod provider_config;
pub mod provider_runtime;
mod pystr;
/// Python `repr()`-compatible rendering of a JSON value (`str(dict)` parity) —
/// the single canonical implementation, reused by `rho-tui`'s fallback tool-call
/// invocation so it never diverges from the session/branch-summary rendering.
pub use pystr::python_repr;
pub mod reload;
pub mod rendering;
pub mod resources;
pub mod session;
pub mod session_export;
pub mod session_manager;
pub mod session_stats;
pub mod skills;
pub mod system_prompt;
pub mod thinking;
pub mod tools;

pub use commands::{
    BUILTIN_TUI_THEME_NAMES, CommandContext, CommandHandler, CommandRegistry, CommandResult,
    CommandSession, LOGIN_PROVIDER_ALIASES, SlashCommand, create_default_command_registry,
    format_reload_summary,
};
pub use events::{CodingSessionEvent, SessionOwnEvent};
pub use login_required::LoginRequiredProvider;
pub use print_mode::{
    MemorySessionStorage, PrintModeConfig, SessionPrintModeConfig, run_print_mode,
    run_session_print_mode,
};
pub use prompt_templates::{
    PromptTemplate, expand_prompt_template_command, load_prompt_templates,
    load_prompt_templates_with_diagnostics, render_prompt_template,
};
pub use rendering::{
    EventRenderer, FinalTextRenderer, JsonEventRenderer, PrintOutputMode, TranscriptRenderer,
    create_event_renderer,
};
pub use session::{
    CodingSession, CodingSessionConfig, ModelChoice, SessionError, StreamingBehavior,
    jsonl_session_storage, parse_terminal_command,
};
pub use session_export::{
    DEFAULT_EXPORT_TITLE, SessionExportError, default_session_export_artifact_path,
    default_session_export_path, export_session_artifact, export_session_html,
    export_session_jsonl, normalize_export_format, render_session_html,
};
pub use session_manager::{CodingSessionRecord, SessionManager, SessionManagerError};
pub use skills::{
    Skill, SkillInvocation, build_skill_index, expand_skill_command, format_skill_invocation,
    load_skills, load_skills_with_diagnostics, parse_skill_invocation,
};
pub use system_prompt::{BuildSystemPromptOptions, Date, build_system_prompt};
pub use tools::{
    create_bash_tool, create_coding_tools, create_edit_tool, create_read_tool, create_write_tool,
};

pub use credentials::{
    ApiKeyCredential, CredentialStoreError, FileCredentialStore, OAuthCredential, StoredCredential,
    credentials_path,
};
pub use oauth::{
    OAuthError, OpenAICodexOAuthProvider, account_id_from_access_token,
    oauth_credential_is_expired, refresh_openai_codex_token,
};
pub use oauth_anthropic::{AnthropicOAuthProvider, refresh_anthropic_token};
pub use oauth_github_copilot::{
    GitHubCopilotOAuthProvider, github_copilot_base_url, login_github_copilot,
    refresh_github_copilot_token,
};
pub use oauth_http::{OAuthHttpClient, OAuthHttpRequest, OAuthHttpResponse, ReqwestOAuthClient};
pub use oauth_registry::{
    get_oauth_provider, get_oauth_providers, oauth_provider_ids, register_oauth_provider,
    reset_oauth_providers, unregister_oauth_provider,
};
pub use oauth_types::{OAuthProvider, OAuthRuntimeAuth};
