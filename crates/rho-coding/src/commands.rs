//! Slash-command registry for rho coding sessions.
//!
//! Port of tau's `tau_coding/commands.py`. This is a self-contained registry
//! that parses `/command` input and returns a [`CommandResult`] describing what
//! the frontend should do. It never touches the provider/model machinery
//! directly — it only reads session accessors (via the [`CommandSession`] trait)
//! and, for a few commands, mutates the session.
//!
//! Every user-facing string is byte-identical to tau's output.

// Slash-command handlers share a single `fn(CommandContext) -> CommandResult`
// signature (tau's uniform `CommandHandler` type), so they must take the context
// by value even when a specific handler only reads it — `needless_pass_by_value`
// is a false positive here.
#![allow(clippy::needless_pass_by_value)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::provider_catalog::{BUILTIN_PROVIDER_CATALOG, builtin_provider_entry};
use crate::reload::{CodingReloadSummary, ReloadCategorySummary};
use crate::resources::ResourceDiagnostic;
use crate::session_manager::{CodingSessionRecord, SessionManager};
use crate::skills::Skill;
use crate::system_prompt::ProjectContextFile;
use crate::thinking::normalize_thinking_level;

/// Built-in TUI theme names. `rho` is rho's own default identity theme
/// (rust-oxide accents, warm neutrals — an owner-sanctioned look/feel divergence
/// from tau, see `dev-notes/phase-5.md`); the three `tau-*` themes stay
/// selectable for byte-for-byte tau parity. rho's `tui.json` is rho-local (tau
/// never reads it), so extending this vocabulary does not touch wire/session
/// parity.
pub const BUILTIN_TUI_THEME_NAMES: [&str; 4] = ["rho", "tau-dark", "tau-light", "high-contrast"];

/// Login-provider aliases in insertion order (tau `LOGIN_PROVIDER_ALIASES`).
///
/// Each entry maps an alias to `(provider, login_method)`. The ordering matters:
/// the "unknown login provider" message lists the catalog names followed by these
/// alias keys in this order.
pub const LOGIN_PROVIDER_ALIASES: [(&str, (&str, &str)); 2] = [
    ("anthropic-api", ("anthropic", "api-key")),
    ("anthropic-subscription", ("anthropic", "subscription")),
];

fn login_provider_alias(name: &str) -> Option<(&'static str, &'static str)> {
    LOGIN_PROVIDER_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, value)| *value)
}

/// Session attributes available to slash-command handlers (tau `CommandSession`
/// `Protocol`).
///
/// The integrator implements this on `CodingSession`. Accessor methods borrow or
/// return owned equivalents of tau's `@property` fields; the mutating methods
/// mirror tau's `set_model` / `reload_provider_settings` / `ensure_session_indexed`.
pub trait CommandSession {
    /// Current working directory (tau `cwd`).
    fn cwd(&self) -> &Path;
    /// Active model id (tau `model`).
    fn model(&self) -> &str;
    /// Active provider name (tau `provider_name`).
    fn provider_name(&self) -> &str;
    /// Models available for the active provider (tau `available_models`).
    fn available_models(&self) -> Vec<String>;
    /// Providers available to the session (tau `available_providers`).
    fn available_providers(&self) -> Vec<String>;
    /// Number of registered tools (tau `len(session.tools)`).
    fn tools_len(&self) -> usize;
    /// Loaded skills (tau `skills`).
    fn skills(&self) -> &[Skill];
    /// Loaded prompt templates (tau `prompt_templates`).
    fn prompt_templates(&self) -> &[PromptTemplate];
    /// Active project-context files (tau `context_files`).
    fn context_files(&self) -> &[ProjectContextFile];
    /// Estimated active-context token count (tau `context_token_estimate`).
    fn context_token_estimate(&self) -> i64;
    /// Auto-compact token threshold, if configured (tau `auto_compact_token_threshold`).
    fn auto_compact_token_threshold(&self) -> Option<i64>;
    /// Context-window size in tokens (tau `context_window_tokens`).
    fn context_window_tokens(&self) -> i64;
    /// Active thinking level (tau `thinking_level`).
    fn thinking_level(&self) -> &str;
    /// Thinking levels available for the active model (tau `available_thinking_levels`).
    fn available_thinking_levels(&self) -> Vec<String>;
    /// Resource-load diagnostics (tau `resource_diagnostics`).
    fn resource_diagnostics(&self) -> &[ResourceDiagnostic];
    /// Rendered system prompt (tau `system_prompt`).
    fn system_prompt(&self) -> &str;
    /// Active session id, if persisted (tau `session_id`).
    fn session_id(&self) -> Option<&str>;
    /// Active session title, if named (tau `session_title`).
    ///
    /// Returned owned because the title lives in the session-manager index
    /// record (fetched by value), not borrowed from `self`.
    fn session_title(&self) -> Option<String>;
    /// Session index manager, if available (tau `session_manager`).
    fn session_manager(&self) -> Option<&SessionManager>;

    /// Reason thinking controls are unavailable (tau `getattr(session,
    /// "thinking_unavailable_reason", None)`). Defaults to `None`.
    fn thinking_unavailable_reason(&self) -> Option<String> {
        None
    }
    /// Context-usage breakdown `(system_tokens, message_tokens, tool_tokens)`
    /// (tau `getattr(session, "context_usage", None)`). Defaults to `None`.
    fn context_usage_breakdown(&self) -> Option<(i64, i64, i64)> {
        None
    }

    /// Set the active model (tau `set_model`).
    fn set_model(&mut self, model: &str);
    /// Reload provider settings; `Err` mirrors tau raising `ValueError` (tau
    /// `reload_provider_settings`).
    fn reload_provider_settings(&mut self) -> Result<(), String>;
    /// Ensure the active session is recorded in the resume index (tau
    /// `ensure_session_indexed`).
    fn ensure_session_indexed(&mut self);
}

// Re-export for the trait signatures above; kept local so the module's public
// surface names the same types tau's Protocol referenced.
use crate::prompt_templates::PromptTemplate;

/// Result of handling a coding-session slash command (tau `CommandResult`).
///
/// The many `bool` flags mirror tau's `CommandResult` dataclass field-for-field;
/// each names a distinct frontend action, so they are not consolidated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct CommandResult {
    /// Whether the registry handled the input as a command.
    pub handled: bool,
    /// Request session exit.
    pub exit_requested: bool,
    /// Request a transcript clear.
    pub clear_requested: bool,
    /// Request a resource reload.
    pub reload_requested: bool,
    /// Request a fresh session.
    pub new_session_requested: bool,
    /// Compact instructions (empty string means "no instructions").
    pub compact_summary: Option<String>,
    /// Request a session export.
    pub export_requested: bool,
    /// Export destination path.
    pub export_destination: Option<PathBuf>,
    /// Export format (`html`/`jsonl`).
    pub export_format: Option<String>,
    /// Resume a specific session id.
    pub resume_session_id: Option<String>,
    /// Request the resume picker.
    pub resume_picker_requested: bool,
    /// Request the branch/tree picker.
    pub tree_picker_requested: bool,
    /// Request the login provider picker.
    pub login_picker_requested: bool,
    /// Request the custom-provider login flow.
    pub custom_provider_login_requested: bool,
    /// Provider to log in to.
    pub login_provider: Option<String>,
    /// Login method (`api-key`/`subscription`).
    pub login_method: Option<String>,
    /// Request the logout provider picker.
    pub logout_picker_requested: bool,
    /// Provider to log out of.
    pub logout_provider: Option<String>,
    /// Request the model picker.
    pub model_picker_requested: bool,
    /// Request the scoped-models picker.
    pub scoped_models_picker_requested: bool,
    /// Request the theme picker.
    pub theme_picker_requested: bool,
    /// New thinking level to apply.
    pub thinking_level: Option<String>,
    /// New theme to apply.
    pub theme: Option<String>,
    /// A user-facing message to display.
    pub message: Option<String>,
}

impl CommandResult {
    /// An unhandled result (ordinary prompt).
    fn unhandled() -> Self {
        Self::default()
    }

    /// A handled result carrying only a message.
    fn message(message: impl Into<String>) -> Self {
        Self {
            handled: true,
            message: Some(message.into()),
            ..Self::default()
        }
    }
}

/// Runtime context passed to slash-command handlers (tau `CommandContext`).
pub struct CommandContext<'a> {
    /// The session being operated on.
    pub session: &'a mut dyn CommandSession,
    /// The registry that dispatched this command.
    pub registry: &'a CommandRegistry,
    /// The full stripped command text.
    pub text: String,
    /// The resolved command name.
    pub name: String,
    /// The command arguments (already trimmed).
    pub args: String,
}

/// A slash-command handler (tau `CommandHandler`).
pub type CommandHandler = fn(CommandContext<'_>) -> CommandResult;

/// A registered slash command and its user-facing metadata (tau `SlashCommand`).
#[derive(Clone)]
pub struct SlashCommand {
    /// Canonical command name (no leading slash).
    pub name: String,
    /// Human-facing description.
    pub description: String,
    /// Usage string.
    pub usage: String,
    /// The handler function.
    pub handler: CommandHandler,
    /// Aliases that resolve to this command.
    pub aliases: Vec<String>,
    /// Additional search terms (for TUI completion).
    pub search_terms: Vec<String>,
}

impl SlashCommand {
    /// Construct a command with no aliases or search terms.
    #[must_use]
    pub fn new(name: &str, usage: &str, description: &str, handler: CommandHandler) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            usage: usage.to_string(),
            handler,
            aliases: Vec::new(),
            search_terms: Vec::new(),
        }
    }

    /// Attach aliases.
    #[must_use]
    pub fn aliases(mut self, aliases: &[&str]) -> Self {
        self.aliases = aliases.iter().map(|alias| (*alias).to_string()).collect();
        self
    }

    /// Attach search terms.
    #[must_use]
    pub fn search_terms(mut self, terms: &[&str]) -> Self {
        self.search_terms = terms.iter().map(|term| (*term).to_string()).collect();
        self
    }
}

/// Parse, register, list, and execute slash commands (tau `CommandRegistry`).
#[derive(Default)]
pub struct CommandRegistry {
    commands: BTreeMap<String, SlashCommand>,
    aliases: HashMap<String, String>,
}

impl CommandRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a slash command and its aliases.
    ///
    /// Returns `Err` on a duplicate command/alias, mirroring tau raising
    /// `ValueError`. (`create_default_command_registry` is infallible in
    /// practice and `.expect()`s the result.)
    pub fn register(&mut self, command: SlashCommand) -> Result<(), String> {
        let name = normalize_name(&command.name);
        if self.commands.contains_key(&name) {
            return Err(format!("Duplicate slash command: /{name}"));
        }
        let aliases: Vec<String> = command
            .aliases
            .iter()
            .map(|alias| normalize_name(alias))
            .collect();
        for normalized_alias in &aliases {
            if self.commands.contains_key(normalized_alias)
                || self.aliases.contains_key(normalized_alias)
            {
                return Err(format!(
                    "Duplicate slash command alias: /{normalized_alias}"
                ));
            }
        }
        for normalized_alias in aliases {
            self.aliases.insert(normalized_alias, name.clone());
        }
        self.commands.insert(name, command);
        Ok(())
    }

    /// Return a command by name or alias.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        let normalized = normalize_name(name);
        let command_name = self.aliases.get(&normalized).cloned().unwrap_or(normalized);
        self.commands.get(&command_name)
    }

    /// Return registered commands sorted by name.
    #[must_use]
    pub fn list_commands(&self) -> Vec<&SlashCommand> {
        self.commands.values().collect()
    }

    /// Execute a slash command, or return unhandled for ordinary prompts.
    pub fn execute<'a>(&'a self, session: &'a mut dyn CommandSession, text: &str) -> CommandResult {
        let stripped = text.trim();
        if !stripped.starts_with('/') {
            return CommandResult::unhandled();
        }
        if stripped.starts_with("/skill:") {
            return CommandResult::unhandled();
        }

        let (mut name, mut args) = parse_command(stripped);
        if name.is_empty() {
            return CommandResult::unhandled();
        }

        let mut handler = self.get(&name).map(|command| command.handler);
        if handler.is_none() && name == "scoped" && args.to_lowercase() == "models" {
            handler = self.get("scoped-models").map(|command| command.handler);
            name = "scoped-models".to_string();
            args = String::new();
        }
        let Some(handler) = handler else {
            return CommandResult::unhandled();
        };

        handler(CommandContext {
            session,
            registry: self,
            text: stripped.to_string(),
            name,
            args,
        })
    }
}

/// Create rho's built-in slash-command registry (tau
/// `create_default_command_registry`).
///
/// Registers the same 17 commands in the same order with identical
/// `name`/`usage`/`description`/`aliases`/`search_terms`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn create_default_command_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    let expect = "builtin slash commands are unique";
    registry
        .register(
            SlashCommand::new("quit", "/quit", "Exit the current session.", exit_command)
                .aliases(&["exit"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new("new", "/new", "Start a new session.", new_command)
                .search_terms(&["clear", "reset"]),
        )
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "compact",
            "/compact [instructions]",
            "Summarize and compact active context.",
            compact_command,
        ))
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "export",
            "/export [--format html|jsonl] [destination]",
            "Export the current session.",
            export_command,
        ))
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "session",
                "/session",
                "Show session info and stats.",
                status_command,
            )
            .search_terms(&["info"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "system",
                "/system",
                "Show the active system prompt without saving it.",
                system_command,
            )
            .search_terms(&["prompt", "instructions"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "skill",
                "/skill:<name> [request]",
                "Expand a loaded skill into your prompt.",
                skill_command,
            )
            .search_terms(&["skills"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "hotkeys",
                "/hotkeys",
                "Show common keyboard shortcuts.",
                hotkeys_command,
            )
            .search_terms(&["keys", "shortcuts", "bindings"]),
        )
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "reload",
            "/reload",
            "Reload local resources and project context.",
            reload_command,
        ))
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "resume",
                "/resume [session-id]",
                "Resume a previous session.",
                resume_command,
            )
            .search_terms(&["history", "previous"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "tree",
                "/tree",
                "Branch from a previous session entry.",
                tree_command,
            )
            .search_terms(&["branch", "history", "fork"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "name",
                "/name <new name>",
                "Rename the current session.",
                name_command,
            )
            .search_terms(&["rename", "title"]),
        )
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "model",
            "/model",
            "Choose the active model.",
            model_command,
        ))
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "scoped-models",
                "/scoped-models",
                "Choose models available to quick-cycle with Ctrl+P.",
                scoped_models_command,
            )
            .search_terms(&["scope", "quick", "cycle", "ctrl+p"]),
        )
        .expect(expect);
    registry
        .register(
            SlashCommand::new(
                "theme",
                "/theme [name]",
                "Show or set the TUI theme.",
                theme_command,
            )
            .search_terms(&["light", "dark", "contrast"]),
        )
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "login",
            "/login [provider]",
            "Connect a provider with OAuth or an API key.",
            login_command,
        ))
        .expect(expect);
    registry
        .register(SlashCommand::new(
            "logout",
            "/logout [provider]",
            "Remove saved credentials for a built-in provider.",
            logout_command,
        ))
        .expect(expect);
    registry
}

/// `/help` — list all registered commands (unregistered by default; port of
/// tau `_help_command`).
pub fn help_command(context: CommandContext<'_>) -> CommandResult {
    let mut lines = vec!["Available commands:".to_string()];
    for command in context.registry.list_commands() {
        lines.push(format!("{}\t{}", command.usage, command.description));
    }
    CommandResult::message(lines.join("\n"))
}

/// `/quit` — request session exit (tau `_exit_command`).
pub fn exit_command(_context: CommandContext<'_>) -> CommandResult {
    CommandResult {
        handled: true,
        exit_requested: true,
        message: Some("Exiting session.".to_string()),
        ..CommandResult::default()
    }
}

/// `/new` — request a fresh session (tau `_new_command`).
pub fn new_command(_context: CommandContext<'_>) -> CommandResult {
    CommandResult {
        handled: true,
        new_session_requested: true,
        ..CommandResult::default()
    }
}

/// `/compact` — request a context compaction (tau `_compact_command`).
pub fn compact_command(context: CommandContext<'_>) -> CommandResult {
    CommandResult {
        handled: true,
        compact_summary: Some(context.args.trim().to_string()),
        ..CommandResult::default()
    }
}

/// `/export` — request a session export (tau `_export_command`).
pub fn export_command(context: CommandContext<'_>) -> CommandResult {
    match parse_export_args(&context.args) {
        Ok((export_format, destination)) => CommandResult {
            handled: true,
            export_requested: true,
            export_destination: destination,
            export_format,
            ..CommandResult::default()
        },
        Err(message) => CommandResult::message(message),
    }
}

/// `/session` — show session info and stats (tau `_status_command`).
pub fn status_command(context: CommandContext<'_>) -> CommandResult {
    let session = &*context.session;
    let context_usage = session.context_usage_breakdown();
    let mut lines = vec![
        format!("Model: {}", session.model()),
        format!("CWD: {}", session.cwd().display()),
        format!("Tools: {}", session.tools_len()),
        format!("Skills: {}", session.skills().len()),
        format!("Prompt templates: {}", session.prompt_templates().len()),
        format!("Context files: {}", session.context_files().len()),
        format!(
            "Estimated context tokens: {}",
            session.context_token_estimate()
        ),
        format!("Context window: {}", session.context_window_tokens()),
    ];
    if let Some((system_tokens, message_tokens, tool_tokens)) = context_usage {
        lines.push(format!(
            "Context token breakdown: system={system_tokens}, messages={message_tokens}, tools={tool_tokens}"
        ));
    }
    lines.extend(thinking_status_lines(session));
    lines.push(format!(
        "Resource diagnostics: {}",
        session.resource_diagnostics().len()
    ));
    if let Some(threshold) = session.auto_compact_token_threshold() {
        lines.push(format!("Auto compact threshold: {threshold}"));
    }
    if let Some(session_id) = session.session_id() {
        lines.push(format!("Session: {session_id}"));
    }
    if let Some(title) = session.session_title().filter(|title| !title.is_empty()) {
        lines.push(format!("Session name: {title}"));
    }
    CommandResult::message(lines.join("\n"))
}

/// `/system` — show the active system prompt (tau `_system_command`).
pub fn system_command(context: CommandContext<'_>) -> CommandResult {
    if !context.args.is_empty() {
        return CommandResult::message("Usage: /system");
    }
    CommandResult::message(context.session.system_prompt().to_string())
}

/// `/hotkeys` — list common keyboard shortcuts (tau `_hotkeys_command`).
pub fn hotkeys_command(_context: CommandContext<'_>) -> CommandResult {
    let lines = [
        "Common keyboard shortcuts:",
        "- Enter: submit prompt",
        "- Shift+Enter: insert newline",
        "- Alt+Enter: queue follow-up while running",
        "- Esc: cancel active run",
        "- Ctrl+K: open slash-command completions",
        "- Ctrl+R: open session picker",
        "- Shift+Tab: cycle thinking mode",
        "- Ctrl+T: toggle thinking tokens",
        "- Ctrl+O: collapse or expand tool output",
        "- Ctrl+C: clear prompt input",
        "- Ctrl+D: quit",
    ];
    CommandResult::message(lines.join("\n"))
}

/// `/skills` — list loaded skills (unregistered by default; tau `_skills_command`).
pub fn skills_command(context: CommandContext<'_>) -> CommandResult {
    let session = &*context.session;
    if session.skills().is_empty() {
        let mut lines = vec!["No skills loaded.".to_string()];
        if !session.resource_diagnostics().is_empty() {
            lines.push(String::new());
            lines.extend(format_diagnostics(
                session.resource_diagnostics(),
                Some("skill"),
            ));
        }
        return CommandResult::message(lines.join("\n"));
    }

    let mut lines = vec!["Available skills:".to_string()];
    let mut skills: Vec<&Skill> = session.skills().iter().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    for skill in skills {
        let description = skill
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
            .unwrap_or("No description");
        lines.push(format!("- {}: {description}", skill.name));
    }
    lines.push("Use a skill with /skill:<name> [request].".to_string());
    if !session.resource_diagnostics().is_empty() {
        lines.push(String::new());
        lines.extend(format_diagnostics(
            session.resource_diagnostics(),
            Some("skill"),
        ));
    }
    CommandResult::message(lines.join("\n"))
}

/// `/resources` — summarize loaded resources (unregistered by default; tau
/// `_resources_command`).
pub fn resources_command(context: CommandContext<'_>) -> CommandResult {
    let session = &*context.session;
    let mut lines = vec![
        format!("Skills: {}", session.skills().len()),
        format!("Prompt templates: {}", session.prompt_templates().len()),
        format!("Context files: {}", session.context_files().len()),
    ];
    if session.resource_diagnostics().is_empty() {
        lines.push("Resource diagnostics: none".to_string());
    } else {
        lines.push(String::new());
        lines.extend(format_diagnostics(session.resource_diagnostics(), None));
    }
    CommandResult::message(lines.join("\n"))
}

/// `/reload` — request an async resource reload (tau `_reload_command`).
pub fn reload_command(_context: CommandContext<'_>) -> CommandResult {
    // Reload owns async extension lifecycle hooks, so frontends execute it from
    // their async command path rather than inside this synchronous registry.
    CommandResult {
        handled: true,
        reload_requested: true,
        ..CommandResult::default()
    }
}

/// `/context` — list active project-context files (unregistered by default; tau
/// `_context_command`).
pub fn context_command(context: CommandContext<'_>) -> CommandResult {
    let session = &*context.session;
    if session.context_files().is_empty() {
        let mut lines = vec!["No project context files loaded.".to_string()];
        if !session.resource_diagnostics().is_empty() {
            lines.push(String::new());
            lines.extend(format_diagnostics(
                session.resource_diagnostics(),
                Some("context"),
            ));
        }
        return CommandResult::message(lines.join("\n"));
    }

    let mut lines = vec!["Active project context files:".to_string()];
    lines.extend(
        session
            .context_files()
            .iter()
            .map(|context_file| format!("- {}", context_file.path)),
    );
    if !session.resource_diagnostics().is_empty() {
        lines.push(String::new());
        lines.extend(format_diagnostics(
            session.resource_diagnostics(),
            Some("context"),
        ));
    }
    CommandResult::message(lines.join("\n"))
}

/// `/skill` — nudge toward the `/skill:<name>` syntax (tau `_skill_command`).
pub fn skill_command(_context: CommandContext<'_>) -> CommandResult {
    CommandResult::message("Use /skill:<name> [request] to expand a loaded skill into your prompt.")
}

/// `/resume` — resume a previous session (tau `_resume_command`).
pub fn resume_command(context: CommandContext<'_>) -> CommandResult {
    if context.args.is_empty() {
        return CommandResult {
            handled: true,
            resume_picker_requested: true,
            ..CommandResult::default()
        };
    }
    let Some(manager) = context.session.session_manager() else {
        return CommandResult::message("Session manager is not available.");
    };
    let session_id = context.args.trim();
    if manager.get_session(session_id).ok().flatten().is_none() {
        return CommandResult::message(format!("Unknown session: {session_id}"));
    }
    CommandResult {
        handled: true,
        resume_session_id: Some(session_id.to_string()),
        ..CommandResult::default()
    }
}

/// `/tree` — request the branch picker (tau `_tree_command`).
pub fn tree_command(context: CommandContext<'_>) -> CommandResult {
    if !context.args.is_empty() {
        return CommandResult::message("Usage: /tree");
    }
    CommandResult {
        handled: true,
        tree_picker_requested: true,
        ..CommandResult::default()
    }
}

/// `/name` — show or set the current session name (tau `_name_command`).
pub fn name_command(context: CommandContext<'_>) -> CommandResult {
    let session_id = match context.session.session_id() {
        Some(session_id) if context.session.session_manager().is_some() => session_id.to_string(),
        _ => return CommandResult::message("Session manager is not available."),
    };

    if context.args.is_empty() {
        let manager = context
            .session
            .session_manager()
            .expect("session manager present");
        let title = manager
            .get_session(&session_id)
            .ok()
            .flatten()
            .and_then(|record| record.title)
            .or_else(|| context.session.session_title())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Untitled session".to_string());
        return CommandResult::message(format!(
            "Current session name: {title}\nUsage: /name <new name>"
        ));
    }

    let name = match validated_session_name(&context.args) {
        Ok(name) => name,
        Err(message) => return CommandResult::message(message),
    };

    let session_indexed = context
        .session
        .session_manager()
        .expect("session manager present")
        .get_session(&session_id)
        .ok()
        .flatten()
        .is_some();
    if !session_indexed {
        context.session.ensure_session_indexed();
    }

    let model = context.session.model().to_string();
    let provider_name = context.session.provider_name().to_string();
    let manager = context
        .session
        .session_manager()
        .expect("session manager present");
    let updated =
        manager.touch_session(&session_id, Some(&model), Some(&provider_name), Some(&name));
    match updated {
        None => CommandResult::message(format!("Unknown current session: {session_id}")),
        Some(record) => CommandResult::message(format!(
            "Session renamed: {}",
            record.title.as_deref().unwrap_or_default()
        )),
    }
}

/// Format the indexed-session list for the current cwd (unregistered helper;
/// tau `_format_sessions`).
pub fn format_sessions(context: &CommandContext<'_>) -> String {
    let Some(manager) = context.session.session_manager() else {
        return "Session manager is not available.".to_string();
    };
    let records = manager
        .list_sessions(Some(context.session.cwd()))
        .unwrap_or_default();
    if records.is_empty() {
        return "No sessions found.".to_string();
    }
    let mut lines = vec!["Indexed sessions:".to_string()];
    for record in &records {
        lines.push(format_session_record(record));
    }
    lines.join("\n")
}

/// `/model` — show the model picker or switch models (tau `_model_command`).
pub fn model_command(context: CommandContext<'_>) -> CommandResult {
    if let Some(refresh_error) = refresh_provider_settings(context.session) {
        return refresh_error;
    }

    if !context.args.is_empty() {
        let model = context.args.trim().to_string();
        let mut available = context.session.available_models();
        if !available.is_empty() && !available.iter().any(|candidate| candidate == &model) {
            available.sort();
            available.dedup();
            let models = available.join(", ");
            return CommandResult::message(format!(
                "Unknown model for provider {}: {model}\nAvailable models: {models}",
                context.session.provider_name(),
            ));
        }
        context.session.set_model(&model);
        return CommandResult::message(format!("Current model: {model}"));
    }

    CommandResult {
        handled: true,
        model_picker_requested: true,
        ..CommandResult::default()
    }
}

/// `/scoped-models` — request the scoped-models picker (tau
/// `_scoped_models_command`).
pub fn scoped_models_command(context: CommandContext<'_>) -> CommandResult {
    if let Some(refresh_error) = refresh_provider_settings(context.session) {
        return refresh_error;
    }

    if !context.args.is_empty() {
        return CommandResult::message("Usage: /scoped-models");
    }
    CommandResult {
        handled: true,
        scoped_models_picker_requested: true,
        ..CommandResult::default()
    }
}

/// `/thinking` — show or set the thinking mode (unregistered by default; tau
/// `_thinking_command`).
pub fn thinking_command(context: CommandContext<'_>) -> CommandResult {
    let session = &*context.session;
    let available = session.available_thinking_levels();
    if context.args.is_empty() {
        let mut lines = thinking_status_lines(session);
        if available.is_empty() {
            lines.insert(
                1,
                format!(
                    "Current model: {}:{}",
                    session.provider_name(),
                    session.model()
                ),
            );
        } else {
            lines.push(format!("Available modes: {}", available.join(", ")));
        }
        return CommandResult::message(lines.join("\n"));
    }

    if available.is_empty() {
        let mut message = format!(
            "Thinking controls are unavailable for {}:{}",
            session.provider_name(),
            session.model()
        );
        if let Some(reason) = thinking_unavailable_reason(session) {
            message = format!("{message}: {reason}");
        }
        return CommandResult::message(message);
    }
    let level = match normalize_thinking_level(Some(&context.args)) {
        Ok(level) => level,
        Err(message) => return CommandResult::message(message),
    };
    if !available.iter().any(|candidate| candidate == &level) {
        let modes = available.join(", ");
        return CommandResult::message(format!(
            "Thinking mode {level} is not available for {}:{}\nAvailable modes: {modes}",
            session.provider_name(),
            session.model()
        ));
    }
    CommandResult {
        handled: true,
        thinking_level: Some(level),
        ..CommandResult::default()
    }
}

fn thinking_status_lines(session: &dyn CommandSession) -> Vec<String> {
    if !session.available_thinking_levels().is_empty() {
        return vec![format!("Thinking mode: {}", session.thinking_level())];
    }
    let mut lines = vec!["Thinking mode: unavailable".to_string()];
    if let Some(reason) = thinking_unavailable_reason(session) {
        lines.push(format!("Thinking unavailable: {reason}"));
    }
    lines
}

fn thinking_unavailable_reason(session: &dyn CommandSession) -> Option<String> {
    session
        .thinking_unavailable_reason()
        .filter(|reason| !reason.is_empty())
}

/// `/theme` — show or set the TUI theme (tau `_theme_command`).
pub fn theme_command(context: CommandContext<'_>) -> CommandResult {
    if context.args.is_empty() {
        return CommandResult {
            handled: true,
            theme_picker_requested: true,
            ..CommandResult::default()
        };
    }

    let theme_name = context.args.trim();
    if !BUILTIN_TUI_THEME_NAMES.contains(&theme_name) {
        let themes = BUILTIN_TUI_THEME_NAMES.join(", ");
        return CommandResult::message(format!(
            "Unknown theme: {theme_name}\nAvailable themes: {themes}"
        ));
    }
    CommandResult {
        handled: true,
        theme: Some(theme_name.to_string()),
        ..CommandResult::default()
    }
}

/// `/login` — connect a provider (tau `_login_command`).
pub fn login_command(context: CommandContext<'_>) -> CommandResult {
    let provider_name = context.args.trim();
    if matches!(provider_name, "custom" | "new" | "add") {
        return CommandResult {
            handled: true,
            custom_provider_login_requested: true,
            ..CommandResult::default()
        };
    }
    if !provider_name.is_empty() {
        let (resolved_provider, login_method) = match login_provider_alias(provider_name) {
            Some((provider, method)) => (provider.to_string(), Some(method.to_string())),
            None => (provider_name.to_string(), None),
        };
        let Some(entry) = builtin_provider_entry(&resolved_provider) else {
            let mut providers: Vec<String> = BUILTIN_PROVIDER_CATALOG
                .iter()
                .map(|entry| entry.name.clone())
                .collect();
            providers.extend(
                LOGIN_PROVIDER_ALIASES
                    .iter()
                    .map(|(alias, _)| (*alias).to_string()),
            );
            let providers = providers.join(", ");
            return CommandResult::message(format!(
                "Unknown login provider: {resolved_provider}\nAvailable providers: {providers}"
            ));
        };
        return CommandResult {
            handled: true,
            login_provider: Some(entry.name),
            login_method,
            ..CommandResult::default()
        };
    }

    CommandResult {
        handled: true,
        login_picker_requested: true,
        ..CommandResult::default()
    }
}

/// `/logout` — remove saved credentials (tau `_logout_command`).
pub fn logout_command(context: CommandContext<'_>) -> CommandResult {
    let provider_name = context.args.trim();
    if !provider_name.is_empty() {
        let Some(entry) = builtin_provider_entry(provider_name) else {
            let providers = BUILTIN_PROVIDER_CATALOG
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return CommandResult::message(format!(
                "Unknown logout provider: {provider_name}\nAvailable providers: {providers}"
            ));
        };
        return CommandResult {
            handled: true,
            logout_provider: Some(entry.name),
            ..CommandResult::default()
        };
    }

    CommandResult {
        handled: true,
        logout_picker_requested: true,
        ..CommandResult::default()
    }
}

fn format_session_record(record: &CodingSessionRecord) -> String {
    let title = record
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("Untitled");
    format!(
        "- {}: {title} ({}) {}",
        record.id,
        record.model,
        record.cwd.display()
    )
}

/// Format resource diagnostics, optionally filtered by `kind` (tau
/// `_format_diagnostics`).
pub fn format_diagnostics(diagnostics: &[ResourceDiagnostic], kind: Option<&str>) -> Vec<String> {
    let filtered: Vec<&ResourceDiagnostic> = diagnostics
        .iter()
        .filter(|diagnostic| kind.is_none_or(|kind| diagnostic.kind == kind))
        .collect();
    if filtered.is_empty() {
        return vec!["Resource diagnostics: none".to_string()];
    }
    let mut lines = vec!["Resource diagnostics:".to_string()];
    lines.extend(
        filtered
            .iter()
            .map(|diagnostic| format!("- {}", diagnostic.format())),
    );
    lines
}

fn refresh_provider_settings(session: &mut dyn CommandSession) -> Option<CommandResult> {
    match session.reload_provider_settings() {
        Ok(()) => None,
        Err(exc) => Some(CommandResult::message(format!(
            "Could not refresh provider settings: {exc}"
        ))),
    }
}

/// Format a reload summary block (tau `format_reload_summary`).
#[must_use]
pub fn format_reload_summary(summary: &CodingReloadSummary) -> String {
    let lines = [
        "Reloaded local coding resources and project context.".to_string(),
        "Resources:".to_string(),
        format!("- Skills: {}", format_reload_category(&summary.skills)),
        format!(
            "- Prompt templates: {}",
            format_reload_category(&summary.prompt_templates)
        ),
        format!(
            "- Extensions: {}",
            format_reload_category(&summary.extensions)
        ),
        "Context:".to_string(),
        format!(
            "- Project context files: {}",
            format_reload_category(&summary.context_files)
        ),
        format!(
            "- Next-turn system prompt: {}",
            if summary.system_prompt_rebuilt {
                "rebuilt"
            } else {
                "unchanged"
            }
        ),
        "Diagnostics:".to_string(),
        format!(
            "- Resource diagnostics: {}",
            format_reload_category(&summary.diagnostics)
        ),
        "Provider config:".to_string(),
        "- Not refreshed by /reload; use /login or /model for provider/model settings.".to_string(),
    ];
    lines.join("\n")
}

fn format_reload_category(summary: &ReloadCategorySummary) -> String {
    let status = if summary.changed {
        "changed"
    } else {
        "unchanged"
    };
    let suffix = match format_count_delta(summary.delta()) {
        Some(delta) => format!(", {delta}"),
        None => String::new(),
    };
    format!("{} total ({status}{suffix})", summary.after)
}

fn format_count_delta(delta: isize) -> Option<String> {
    if delta == 0 {
        return None;
    }
    // Python `f"{delta:+d}"`: always sign-prefixed.
    Some(format!("{delta:+}"))
}

fn parse_command(text: &str) -> (String, String) {
    // Drop the leading '/' (always a single ASCII byte here).
    let body = &text[1..];
    match body.split_once(' ') {
        Some((command, args)) => (normalize_name(command), args.trim().to_string()),
        None => (normalize_name(body), String::new()),
    }
}

fn parse_export_args(args: &str) -> Result<(Option<String>, Option<PathBuf>), String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let mut export_format: Option<String> = None;
    let mut destination: Option<PathBuf> = None;
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        if part == "--format" {
            index += 1;
            if index >= parts.len() {
                return Err("Usage: /export [--format html|jsonl] [destination]".to_string());
            }
            export_format = Some(parts[index].to_string());
        } else if let Some(value) = part.strip_prefix("--format=") {
            export_format = Some(value.to_string());
        } else if part.starts_with('-') {
            return Err(format!("Unknown export option: {part}"));
        } else if destination.is_none() {
            destination = Some(expanduser(part));
        } else {
            return Err("Usage: /export [--format html|jsonl] [destination]".to_string());
        }
        index += 1;
    }
    Ok((export_format, destination))
}

fn validated_session_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err("Usage: /name <new name>".to_string());
    }
    if name.contains(['\r', '\n', '\t']) {
        return Err("Session name must be a single line.".to_string());
    }
    Ok(name.to_string())
}

fn normalize_name(name: &str) -> String {
    let trimmed = name.trim();
    let no_slash = trimmed.strip_prefix('/').unwrap_or(trimmed);
    no_slash.to_lowercase()
}

/// Expand a leading `~` / `~/` using `$HOME` (Python `Path.expanduser`).
///
/// `~user` forms are left unexpanded (not exercised by tau's command paths and
/// require platform user-database lookups).
fn expanduser(path: &str) -> PathBuf {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
    if let Some(home) = home {
        if path == "~" {
            return PathBuf::from(home);
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests;
