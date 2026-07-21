//! Persistent coding-session wrapper built on `AgentHarness` (port of tau's
//! `tau_coding/session.py`, dispatch-1 session core).
//!
//! `AgentHarness` owns the in-memory agent brain; [`CodingSession`] owns the
//! coding-session environment around it: durable append-only session entries,
//! the default coding tools, compaction/branch/thinking machinery, and the
//! session-owned event stream.
//!
//! ## Dispatch-1 scope
//!
//! This is the *session core*. The provider catalog (`provider_config`),
//! slash/terminal command registry, skills, prompt templates, HTML export,
//! OAuth, and the WASM extension runtime are dispatch-2 / later milestones. In
//! this port `provider_settings`/`runtime_provider_config` are always absent, so
//! every provider-catalog branch collapses to its `None` default (thinking
//! levels = the full [`THINKING_LEVELS`] set, context window =
//! [`DEFAULT_CONTEXT_WINDOW_TOKENS`], model fixed by config). Extensions are a
//! no-op: input hooks pass through and session-owned events are emitted directly
//! rather than mirrored to an extension bus. See `dev-notes/phase-4b1.md`.
//!
//! A handful of pedantic clippy lints are allowed module-wide (same rationale as
//! `tools/difflib.rs`): the port mirrors tau's terse index arithmetic and
//! Python-style names, and idiomatizing them would diverge from the source.

#![allow(
    clippy::similar_names,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::option_option,
    clippy::items_after_statements,
    clippy::assigning_clones
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;

use rho_agent::clock::{Clock, IdGen, system_clock, uuid_id_gen};
use rho_agent::events::AgentEvent;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig, QueuedMessages};
use rho_agent::messages::{
    AgentMessage, AssistantMessage, StopReason, TextContent, ToolResultContent, ToolResultMessage,
    UserMessage,
};
use rho_agent::model_limits::RuntimeModelLimits;
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::session::entries::{
    BranchSummaryEntry, CompactionEntry, LeafEntry, MessageEntry, ModelChangeEntry, SessionEntry,
    SessionInfoEntry, ThinkingLevelChangeEntry,
};
use rho_agent::session::memory::SessionState;
use rho_agent::session::storage::{JsonlSessionStorage, SessionStorage};
use rho_agent::session::tree::{SessionTreeError, path_to_entry};
use rho_agent::tools::AgentTool;

use crate::branch_summary::summarize_branch_messages_with_model;
use crate::commands::{CommandResult, CommandSession, create_default_command_registry};
use crate::context::discover_project_context_with_diagnostics;
use crate::context_window::{
    ContextUsageEstimate, DEFAULT_COMPACTION_KEEP_RECENT_TOKENS, DEFAULT_CONTEXT_WINDOW_TOKENS,
    SUMMARIZATION_SYSTEM_PROMPT, auto_compaction_threshold_for_context_window,
    build_compaction_summary_prompt, estimate_context_usage, estimate_message_tokens,
    summarize_messages_for_compaction,
};
use crate::credentials::{FileCredentialStore, credentials_path};
use crate::diagnostics::{
    AgentCallDiagnosticContext, AgentCallDiagnosticLogger, new_agent_call_run_id,
};
use crate::events::{
    AgentSettledEvent, AutoRetryEndEvent, AutoRetryStartEvent, CodingSessionEvent,
    CompactionEndEvent, CompactionReason, CompactionStartEvent, QueueUpdateEvent,
    SessionAgentEndEvent,
};
use crate::extensions::ExtensionRuntime;
use crate::extensions::{
    SessionContext as ExtSessionContext, SessionContextBridge as ExtSessionContextBridge,
};
use crate::paths::RhoPaths;
use crate::prompt_templates::{
    PromptTemplate, expand_prompt_template_command, load_prompt_templates_with_diagnostics,
};
use crate::provider_config::{
    ProviderConfig, ProviderConfigError, ProviderSettings, load_provider_settings,
    provider_default_thinking_level, provider_has_usable_credentials, provider_thinking_levels,
    provider_thinking_unavailable_reason, save_default_provider_model,
    save_provider_thinking_level, toggle_saved_scoped_model, validate_provider_model,
};
use crate::provider_runtime::create_model_provider;
use crate::reload::{CodingReloadSummary, ReloadCategorySummary};
use crate::resources::{
    ResourceDiagnostic, ResourceError, RhoResourcePaths, resource_paths_with_cwd,
};
use crate::session_export::{
    default_session_export_artifact_path, export_session_artifact, normalize_export_format,
};
use crate::session_manager::SessionManager;
use crate::session_stats::{SessionStats, calculate_session_stats};
use crate::skills::{Skill, expand_skill_command, load_skills_with_diagnostics};
use crate::system_prompt::{BuildSystemPromptOptions, ProjectContextFile, build_system_prompt};
use crate::thinking::{
    DEFAULT_THINKING_LEVEL, THINKING_LEVELS, next_thinking_level, normalize_thinking_level,
};
use crate::tools::{create_bash_tool, create_coding_tools};

/// tau `SESSION_NAME_SYSTEM_PROMPT`.
const SESSION_NAME_SYSTEM_PROMPT: &str = "You write concise coding-agent session names. Reply with only a short title, \
maximum four words, no quotes, no punctuation-only output.";

/// How a queued message should be applied while the agent is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingBehavior {
    /// Steer the current run.
    Steer,
    /// Queue as a follow-up turn.
    FollowUp,
}

/// Errors raised by the coding session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Session storage failure.
    #[error(transparent)]
    Storage(#[from] rho_agent::session::storage::SessionStorageError),
    /// Session-tree navigation failure.
    #[error(transparent)]
    Tree(#[from] SessionTreeError),
    /// A user-facing error (bad argument, unavailable feature).
    #[error("{0}")]
    Value(String),
    /// A session-export failure (bad format or filesystem error).
    #[error("{0}")]
    Export(String),
}

impl SessionError {
    /// A tau-style exception class name for diagnostic-log entries. Not covered
    /// by any golden (diagnostic files are runtime-only), so this is a faithful
    /// approximation of `type(exc).__name__`, not a byte-checked mapping.
    #[must_use]
    fn error_type(&self) -> &'static str {
        match self {
            SessionError::Storage(_) => "StorageError",
            SessionError::Tree(_) => "SessionTreeError",
            SessionError::Value(_) | SessionError::Export(_) => "ValueError",
        }
    }
}

impl From<ProviderConfigError> for SessionError {
    /// Preserve tau's `ProviderConfigError` message verbatim (tau catches these
    /// as user-facing `ValueError`s at the command boundary).
    fn from(err: ProviderConfigError) -> Self {
        SessionError::Value(err.0)
    }
}

/// A selectable model and the provider that serves it (tau `ModelChoice`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelChoice {
    /// The provider name that serves the model.
    pub provider_name: String,
    /// The model id.
    pub model: String,
}

impl ModelChoice {
    /// Build a provider/model choice.
    #[must_use]
    pub fn new(provider_name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: provider_name.into(),
            model: model.into(),
        }
    }
}

/// Result of an input-bar terminal command.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalCommandResult {
    /// The normalized command.
    pub command: String,
    /// Combined stdout/stderr output.
    pub output: String,
    /// Exit code, if known.
    pub exit_code: Option<i64>,
    /// Whether the command succeeded (exit code 0).
    pub ok: bool,
    /// Whether the output was added to the transcript.
    pub added_to_context: bool,
}

/// One branchable entry in the active session tree.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeChoice {
    /// Entry id to branch from.
    pub entry_id: String,
    /// Human-readable label.
    pub label: String,
    /// Whether this is the active leaf.
    pub active: bool,
    /// Whether the entry is a tool call.
    pub is_tool_call: bool,
}

/// Result of moving the active session tree leaf.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeBranchResult {
    /// Status message.
    pub message: String,
    /// Optional input prefill (when branching before a user message).
    pub input_prefill: Option<String>,
}

/// Parsed input-bar terminal command request.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalCommandRequest {
    /// The shell command.
    pub command: String,
    /// Whether to add output to context.
    pub add_to_context: bool,
}

/// Prepared active-context entries for a compaction run.
#[derive(Debug, Clone, PartialEq)]
struct CompactionPlan {
    replace_entry_ids: Vec<String>,
    messages_to_summarize: Vec<AgentMessage>,
}

/// Configuration for a persistent coding session (dispatch-1 subset).
// The many independent feature toggles mirror tau's config dataclass
// field-for-field; each names a distinct knob, so they are not consolidated.
#[allow(clippy::struct_excessive_bools)]
pub struct CodingSessionConfig {
    /// The model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// The requested model id.
    pub model: String,
    /// Append-only session storage.
    pub storage: Arc<dyn SessionStorage>,
    /// Session working directory.
    pub cwd: PathBuf,
    /// Explicit system prompt (skips rebuild when set).
    pub system: Option<String>,
    /// Custom system-prompt override forwarded to the builder.
    pub custom_system_prompt: Option<String>,
    /// Text appended after the built system prompt.
    pub append_system_prompt: Option<String>,
    /// Explicit project-context files.
    pub context_files: Vec<ProjectContextFile>,
    /// Explicit tool set (defaults to the built-in coding tools).
    pub tools: Option<Vec<AgentTool>>,
    /// Resource paths override (skills/prompts/context discovery).
    pub resource_paths: Option<RhoResourcePaths>,
    /// Session id for the resume index.
    pub session_id: Option<String>,
    /// Session manager for indexing/resume.
    pub session_manager: Option<SessionManager>,
    /// Active provider name.
    pub provider_name: String,
    /// Durable provider settings (the provider catalog + saved preferences).
    ///
    /// When `None` the whole provider-catalog surface collapses to its fixed
    /// defaults (model pinned by config, all thinking levels available, the
    /// fallback context window).
    pub provider_settings: Option<ProviderSettings>,
    /// The provider config backing the live runtime provider, if the session was
    /// constructed from a durable provider config (drives `set_model` /
    /// thinking-level provider refreshes).
    pub runtime_provider_config: Option<ProviderConfig>,
    /// Explicit auto-compaction threshold override.
    pub auto_compact_token_threshold: Option<i64>,
    /// Whether auto-compaction is enabled.
    pub auto_compact_enabled: bool,
    /// Initial thinking level.
    pub thinking_level: String,
    /// Index the session on its first durable write.
    pub index_on_first_persist: bool,
    /// Shell-command prefix for the bash tool.
    pub shell_command_prefix: Option<String>,
    /// Whether skill discovery is enabled (dispatch-2 loads them; here it only
    /// gates future wiring).
    pub skills_enabled: bool,
    /// Whether extension discovery scans the resource directories (tau
    /// `extensions_enabled`; default `true`).
    pub extensions_enabled: bool,
    /// Explicit extension component paths loaded regardless of
    /// `extensions_enabled` (tau `extension_paths`; the `-x/--extension` flag).
    pub extension_paths: Vec<PathBuf>,
    /// Whether project-local `.rho/extensions` are discovered (tau
    /// `project_extensions_enabled`; opt-in because they run at startup).
    pub project_extensions_enabled: bool,
    /// A pre-built extension runtime the session adopts instead of discovering
    /// its own (tau `extension_runtime`). Used to inject a host/test double; a
    /// fresh session builds a default [`ExtensionRuntime`] and loads it.
    pub extension_runtime: Option<ExtensionRuntime>,
    /// Clock for entry/message timestamps.
    pub clock: Arc<dyn Clock>,
    /// Id generator for session entries.
    pub id_gen: Arc<dyn IdGen>,
}

impl CodingSessionConfig {
    /// Build a config with the common defaults (real clock/ids, coding tools).
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
        storage: Arc<dyn SessionStorage>,
        cwd: PathBuf,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            storage,
            cwd,
            system: None,
            custom_system_prompt: None,
            append_system_prompt: None,
            context_files: Vec::new(),
            tools: None,
            resource_paths: None,
            session_id: None,
            session_manager: None,
            provider_name: "openai".to_string(),
            provider_settings: None,
            runtime_provider_config: None,
            auto_compact_token_threshold: None,
            auto_compact_enabled: true,
            thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
            index_on_first_persist: false,
            shell_command_prefix: None,
            skills_enabled: true,
            extensions_enabled: true,
            extension_paths: Vec::new(),
            project_extensions_enabled: false,
            extension_runtime: None,
            clock: system_clock(),
            id_gen: uuid_id_gen(),
        }
    }
}

/// Tau-owned resources loaded around a coding session (skills, prompt
/// templates, discovered project-context files, and non-fatal diagnostics).
struct SessionResources {
    skills: Vec<Skill>,
    prompt_templates: Vec<PromptTemplate>,
    context_files: Vec<ProjectContextFile>,
    diagnostics: Vec<ResourceDiagnostic>,
}

/// Tau's coding-agent environment wrapper.
pub struct CodingSession {
    config: CodingSessionConfig,
    state: SessionState,
    harness: AgentHarness,
    last_parent_id: Option<String>,
    pending_initial_entries: Vec<SessionEntry>,
    context_files: Vec<ProjectContextFile>,
    skills: Vec<Skill>,
    prompt_templates: Vec<PromptTemplate>,
    resource_diagnostics: Vec<ResourceDiagnostic>,
    thinking_level: String,
    auto_compact_token_threshold: Option<i64>,
    auto_compact_enabled: bool,
    /// Live model limits discovered from the active provider's authenticated
    /// catalog, keyed by the (provider, model) they were resolved for (tau
    /// `_runtime_model_limits` / `_runtime_model_limits_key`). Cleared on any
    /// model/provider swap; refreshed at load and before each turn.
    runtime_model_limits: Option<RuntimeModelLimits>,
    runtime_model_limits_key: Option<(String, String)>,
    /// The last non-fatal live model-limit discovery error, for `/status`
    /// diagnostics (tau `_model_limits_discovery_error`).
    model_limits_discovery_error: Option<String>,
    context_usage_cache: Option<ContextUsageEstimate>,
    diagnostic_logger: AgentCallDiagnosticLogger,
    last_diagnostic_log_path: Option<PathBuf>,
    run_error: Option<String>,
    provider_settings: Option<ProviderSettings>,
    runtime_provider_config: Option<ProviderConfig>,
    resource_paths: RhoResourcePaths,
    credential_store: Arc<FileCredentialStore>,
    /// Providers swapped in by `set_model`/`set_provider`; kept alive here
    /// (tau's `_owned_providers`) so the live harness reference stays valid.
    owned_providers: Vec<Arc<dyn ModelProvider>>,
    /// The long-lived extension runtime bound to this session (tau
    /// `_extension_runtime`). Defaults to a [`NoopExtensionHost`]-backed runtime
    /// so extension-free sessions carry zero WASM machinery and behave
    /// identically to a pre-extension build.
    ///
    /// Boxed to keep `CodingSession` small: it is held across the print-mode
    /// run future, and the runtime's registration tables would otherwise bloat
    /// that future past the `large_futures` budget.
    extension_runtime: Box<ExtensionRuntime>,
    /// The live session-context snapshot the extension host bridge reads. Shared
    /// (`Arc<Mutex<_>>`) so sync mutators (`set_model`/`set_provider`) can update
    /// it in place while a bound extension reads the current value.
    extension_context: Arc<Mutex<ExtSessionContext>>,
}

impl CodingSession {
    /// Load a coding session from append-only storage.
    pub async fn load(mut config: CodingSessionConfig) -> Result<Self, SessionError> {
        let mut entries = config.storage.read_all().await?;
        let mut pending_initial_entries: Vec<SessionEntry> = Vec::new();

        if entries.is_empty() {
            let mut info = SessionInfoEntry::new();
            info.id = config.id_gen.new_id();
            info.timestamp = config.clock.now_secs();
            info.created_at = config.clock.now_secs();
            info.cwd = Some(config.cwd.to_string_lossy().to_string());

            let initial_model = initial_model_for_config(&config);
            let mut model = ModelChangeEntry::new(initial_model.clone());
            model.id = config.id_gen.new_id();
            model.timestamp = config.clock.now_secs();
            model.parent_id = Some(info.id.clone());

            let initial_thinking = initial_thinking_level_for_config(&config, &initial_model);
            let mut thinking = ThinkingLevelChangeEntry::new(Some(initial_thinking));
            thinking.id = config.id_gen.new_id();
            thinking.timestamp = config.clock.now_secs();
            thinking.parent_id = Some(model.id.clone());

            let info_e = SessionEntry::SessionInfo(info);
            let model_e = SessionEntry::ModelChange(model);
            let thinking_e = SessionEntry::ThinkingLevelChange(thinking);
            entries = vec![info_e.clone(), model_e.clone(), thinking_e.clone()];
            pending_initial_entries = vec![info_e, model_e, thinking_e];
        } else {
            entries = detach_missing_parents(entries);
        }

        // tau `load`: with a latest leaf, replay its root-to-leaf path (an
        // explicit `entry_id: None` root leaf → the empty pre-root context, NOT
        // a linear replay); with no leaf at all, replay linearly.
        let latest_leaf = latest_leaf_entry(&entries);
        let state = match &latest_leaf {
            Some(entry_id) => state_at(&entries, entry_id.as_deref())?,
            None => SessionState::from_entries(&entries),
        };

        let resource_paths = resource_paths_with_cwd(config.resource_paths.clone(), &config.cwd);
        let resources = load_session_resources(
            &resource_paths,
            &config.context_files,
            config.skills_enabled,
        );

        let base_tools = config.tools.clone().unwrap_or_else(|| {
            create_coding_tools(&config.cwd, config.shell_command_prefix.as_deref())
        });

        // Adopt a provided extension runtime (a host/test double already loaded)
        // or build a fresh one and discover extensions (tau `CodingSession.load`:
        // `fresh_extension_runtime = extension_runtime is None`). A default
        // runtime is `NoopExtensionHost`-backed, so with no extensions this is a
        // pure pass-through (`compose_tools` returns the built-ins unwrapped and
        // there are no prompt guidelines).
        let extension_runtime = if let Some(runtime) = config.extension_runtime.take() {
            runtime
        } else {
            let mut runtime = ExtensionRuntime::for_session();
            if config.extensions_enabled || !config.extension_paths.is_empty() {
                runtime
                    .load(
                        &resource_paths,
                        &config.extension_paths,
                        config.extensions_enabled,
                        config.project_extensions_enabled,
                    )
                    .await;
            }
            runtime
        };

        let tools = extension_runtime.compose_tools(base_tools);
        let system = config.system.clone().unwrap_or_else(|| {
            build_system_prompt(&BuildSystemPromptOptions {
                cwd: config.cwd.clone(),
                tools: tools.clone(),
                custom_prompt: config.custom_system_prompt.clone(),
                append_system_prompt: config.append_system_prompt.clone(),
                context_files: resources.context_files.clone(),
                extra_guidelines: extension_runtime.prompt_guidelines(),
                ..Default::default()
            })
        });

        let runtime_model = runtime_model_for_state(&config, &state);
        let harness = AgentHarness::new(
            AgentHarnessConfig::new(config.provider.clone(), runtime_model.clone(), system)
                .with_tools(tools)
                .with_clock(config.clock.clone()),
            state.messages.clone(),
        );

        // tau `__init__`: the initial thinking level defaults to the active
        // provider's preferred level for the runtime model (falling back to the
        // config default when there are no provider settings).
        let default_thinking = match active_provider_config_from(
            config.provider_settings.as_ref(),
            &config.provider_name,
        ) {
            Some(provider) => preferred_thinking_level_for_model(
                &provider,
                &runtime_model,
                &config.thinking_level,
            ),
            None => config.thinking_level.clone(),
        };
        let thinking_level = state_thinking_level(&state, &default_thinking);
        let diagnostic_logger =
            AgentCallDiagnosticLogger::new(diagnostics_log_path(&resource_paths));
        let auto_compact_token_threshold = config.auto_compact_token_threshold;
        let auto_compact_enabled = config.auto_compact_enabled;
        let last_parent_id = last_parent_id_from_state(&state);
        let context_files = resources.context_files.clone();
        let provider_settings = config.provider_settings.clone();
        let runtime_provider_config = config.runtime_provider_config.clone();
        let credential_store = Arc::new(FileCredentialStore::new(credentials_path(
            resource_paths.paths.as_ref(),
        )));

        let mut session = Self {
            config,
            state,
            harness,
            last_parent_id,
            pending_initial_entries,
            context_files,
            skills: resources.skills,
            prompt_templates: resources.prompt_templates,
            resource_diagnostics: resources.diagnostics,
            thinking_level,
            auto_compact_token_threshold,
            auto_compact_enabled,
            runtime_model_limits: None,
            runtime_model_limits_key: None,
            model_limits_discovery_error: None,
            context_usage_cache: None,
            diagnostic_logger,
            last_diagnostic_log_path: None,
            run_error: None,
            provider_settings,
            runtime_provider_config,
            resource_paths,
            credential_store,
            owned_providers: Vec::new(),
            extension_runtime: Box::new(extension_runtime),
            extension_context: Arc::new(Mutex::new(ExtSessionContext::default())),
        };
        session.persist_loaded_interrupted_tool_repairs().await?;
        session.sync_thinking_level_to_active_model();
        session.refresh_runtime_provider()?;
        session.refresh_runtime_model_limits().await;
        // Bind a live session-context bridge so extension `context.*` reads
        // reflect this session (tau's `extension_runtime.bind(session)`), and
        // fire `session_start` for lifecycle subscribers. Cheap no-op when no
        // extensions are loaded.
        session.refresh_extension_context();
        if session.extension_runtime.has_extensions() {
            let bridge = Arc::new(ExtSessionContextBridge::new(
                session.extension_context.clone(),
            ));
            session.extension_runtime.set_bridge(bridge);
            session.extension_runtime.rebind().await;
            session
                .extension_runtime
                .emit_session_start("startup")
                .await;
        }
        Ok(session)
    }

    /// Refresh the shared session-context snapshot the extension bridge reads
    /// (cwd / model / provider / session id / system prompt). Called at load and
    /// after model/provider changes so extension `context.*` reads stay current.
    fn refresh_extension_context(&self) {
        let mut ctx = self.extension_context.lock().unwrap();
        ctx.cwd = self.config.cwd.display().to_string();
        ctx.model = self.harness.config().model.clone();
        ctx.provider_name = self.config.provider_name.clone();
        ctx.session_id = self.config.session_id.clone();
        ctx.system_prompt = self.harness.config().system.clone();
    }

    // ---- properties -------------------------------------------------------

    /// Session working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.config.cwd
    }

    /// The extension runtime bound to this session (tau `extension_runtime`).
    #[must_use]
    pub fn extension_runtime(&self) -> &ExtensionRuntime {
        &self.extension_runtime
    }

    /// Mutable access to the extension runtime (for the agent-event fan-out and
    /// lifecycle emit the harness/frontend drives).
    pub fn extension_runtime_mut(&mut self) -> &mut ExtensionRuntime {
        &mut self.extension_runtime
    }

    /// Active model.
    #[must_use]
    pub fn model(&self) -> String {
        self.harness.config().model.clone()
    }

    /// Active provider name.
    #[must_use]
    pub fn provider_name(&self) -> &str {
        &self.config.provider_name
    }

    /// The agent tools.
    #[must_use]
    pub fn tools(&self) -> Vec<AgentTool> {
        self.harness.config().tools.clone()
    }

    /// Current transcript.
    #[must_use]
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.harness.messages()
    }

    /// Last replayed durable session state.
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Active project context files.
    #[must_use]
    pub fn context_files(&self) -> &[ProjectContextFile] {
        &self.context_files
    }

    /// Loaded markdown skills.
    #[must_use]
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Loaded markdown prompt templates.
    #[must_use]
    pub fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.prompt_templates
    }

    /// Loaded extension names in load order (tau `extension_names`).
    #[must_use]
    pub fn extension_names(&self) -> Vec<String> {
        self.extension_runtime.extension_names()
    }

    /// Cumulative activity and billed usage for the active branch (tau
    /// `session_stats`). Iterates every original-branch message (including
    /// compaction-replaced messages) and estimates cost from catalog pricing.
    #[must_use]
    pub fn session_stats(&self) -> SessionStats {
        calculate_session_stats(
            &self.state.entries,
            &|provider_name, model, input_tokens| {
                self.pricing_for_response(provider_name, model, input_tokens)
            },
        )
    }

    /// Resolve per-million-token rates for one response from the configured
    /// provider's model metadata (tau `_pricing_for_response`).
    fn pricing_for_response(
        &self,
        provider_name: &str,
        model: &str,
        input_tokens: i64,
    ) -> Option<IndexMap<String, f64>> {
        let provider = provider_config_for_name(&self.config, provider_name)?;
        if provider.name() != provider_name {
            return None;
        }
        let metadata = provider.model_metadata()?.get(model)?;
        for tier in &metadata.cost_tiers {
            if tier.max_input_tokens.is_none() || input_tokens <= tier.max_input_tokens.unwrap() {
                return Some(tier.cost.clone());
            }
        }
        if metadata.cost.is_empty() {
            None
        } else {
            Some(metadata.cost.clone())
        }
    }

    /// Non-fatal resource diagnostics.
    #[must_use]
    pub fn resource_diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.resource_diagnostics
    }

    /// Effective system prompt.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        self.harness.config().system.clone()
    }

    /// Backing storage.
    #[must_use]
    pub fn storage(&self) -> Arc<dyn SessionStorage> {
        self.config.storage.clone()
    }

    /// Active thinking level.
    #[must_use]
    pub fn thinking_level(&self) -> &str {
        &self.thinking_level
    }

    /// Provider names rho can call with available credentials (tau
    /// `available_providers`).
    #[must_use]
    pub fn available_providers(&self) -> Vec<String> {
        let Some(_) = self.provider_settings.as_ref() else {
            return vec![self.provider_name().to_string()];
        };
        self.usable_provider_configs()
            .iter()
            .map(|provider| provider.name().to_string())
            .collect()
    }

    /// Model names for the active provider when it is usable (tau
    /// `available_models`).
    #[must_use]
    pub fn available_models(&self) -> Vec<String> {
        let Some(settings) = self.provider_settings.as_ref() else {
            return vec![self.model()];
        };
        let Ok(provider) = settings.get_provider(Some(self.provider_name())) else {
            return vec![self.model()];
        };
        if !self.provider_is_usable(provider) {
            return Vec::new();
        }
        provider.models().to_vec()
    }

    /// Provider/model choices rho can call with available credentials (tau
    /// `available_model_choices`).
    #[must_use]
    pub fn available_model_choices(&self) -> Vec<ModelChoice> {
        let Some(_) = self.provider_settings.as_ref() else {
            return vec![ModelChoice::new(self.provider_name(), self.model())];
        };
        self.usable_provider_configs()
            .iter()
            .flat_map(|provider| {
                provider
                    .models()
                    .iter()
                    .map(|model| ModelChoice::new(provider.name(), model.clone()))
            })
            .collect()
    }

    /// Configured quick-switch model choices that are currently usable (tau
    /// `scoped_model_choices`).
    #[must_use]
    pub fn scoped_model_choices(&self) -> Vec<ModelChoice> {
        let Some(settings) = self.provider_settings.as_ref() else {
            return Vec::new();
        };
        let available: std::collections::HashSet<ModelChoice> =
            self.available_model_choices().into_iter().collect();
        settings
            .scoped_models
            .iter()
            .map(|item| ModelChoice::new(item.provider.clone(), item.model.clone()))
            .filter(|choice| available.contains(choice))
            .collect()
    }

    /// Thinking modes supported by the active provider/model (tau
    /// `available_thinking_levels`).
    #[must_use]
    pub fn available_thinking_levels(&self) -> Vec<String> {
        if self.provider_settings.is_none() {
            return THINKING_LEVELS.iter().map(|s| (*s).to_string()).collect();
        }
        let Some(provider) = self.active_provider_config() else {
            return Vec::new();
        };
        provider_thinking_levels(&provider, Some(&self.model()))
    }

    /// Why thinking controls are unavailable for the active model, if they are
    /// (tau `thinking_unavailable_reason`).
    #[must_use]
    pub fn thinking_unavailable_reason(&self) -> Option<String> {
        if !self.available_thinking_levels().is_empty() {
            return None;
        }
        let Some(provider) = self.active_provider_config() else {
            return Some("Active provider settings are not available".to_string());
        };
        provider_thinking_unavailable_reason(&provider, Some(&self.model()))
    }

    /// Session id, if any.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.config.session_id.as_deref()
    }

    /// Whether the agent loop is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.harness.is_running()
    }

    /// Queued messages snapshot.
    #[must_use]
    pub fn queued_messages(&self) -> QueuedMessages {
        self.harness.queued_messages()
    }

    /// Queued steering message texts.
    #[must_use]
    pub fn queued_steering_messages(&self) -> Vec<String> {
        self.harness
            .queued_messages()
            .steering
            .iter()
            .map(AgentMessage::text)
            .collect()
    }

    /// Queued follow-up message texts.
    #[must_use]
    pub fn queued_follow_up_messages(&self) -> Vec<String> {
        self.harness
            .queued_messages()
            .follow_up
            .iter()
            .map(AgentMessage::text)
            .collect()
    }

    /// Path of the last diagnostic log written, if any.
    #[must_use]
    pub fn last_diagnostic_log_path(&self) -> Option<&Path> {
        self.last_diagnostic_log_path.as_deref()
    }

    /// Cancel the running agent loop.
    pub fn cancel(&self) {
        self.harness.cancel();
    }

    /// A cheap, cloneable control handle (cancel / steer / follow-up / queue
    /// snapshot) that works *while* a [`Self::prompt`]/[`Self::continue_`] stream
    /// is being drained. The stream borrows `&mut self`, so the interactive TUI
    /// drives cancellation and steering through this `Arc`-backed handle instead
    /// of calling `&self` methods on the borrowed session.
    #[must_use]
    pub fn control(&self) -> rho_agent::harness::HarnessControl {
        self.harness.control()
    }

    /// Build the `queue_update` snapshot event.
    #[must_use]
    pub fn queue_update_event(&self) -> QueueUpdateEvent {
        QueueUpdateEvent::new(
            self.queued_steering_messages(),
            self.queued_follow_up_messages(),
        )
    }

    /// Structured context accounting (cached until the transcript changes).
    pub fn context_usage(&mut self) -> ContextUsageEstimate {
        if self.context_usage_cache.is_none() {
            let usage = estimate_context_usage(
                &self.harness.config().system,
                &self.harness.messages(),
                &self.harness.config().tools,
            );
            self.context_usage_cache = Some(usage);
        }
        self.context_usage_cache.expect("cache populated")
    }

    /// Rough token estimate for the active provider context.
    pub fn context_token_estimate(&mut self) -> i64 {
        self.context_usage().total_tokens
    }

    /// Active model's discovered or configured context window (tau
    /// `context_window_tokens`). A live limit discovered for the active
    /// (provider, model) overrides the static catalog.
    #[must_use]
    pub fn context_window_tokens(&self) -> i64 {
        if let Some(limits) = self.active_runtime_model_limits() {
            return limits.context_window();
        }
        let Some(provider) = self.active_provider_config() else {
            return DEFAULT_CONTEXT_WINDOW_TOKENS;
        };
        provider
            .context_windows()
            .get(&self.model())
            .copied()
            .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
    }

    /// Where the active context-window limit came from (tau
    /// `context_window_source`): the provider's live catalog when a discovered
    /// limit is active, else the configured catalog.
    #[must_use]
    pub fn context_window_source(&self) -> &'static str {
        if self.active_runtime_model_limits().is_some() {
            "provider live catalog"
        } else {
            "configured catalog"
        }
    }

    /// The last non-fatal live model-limit discovery error, if any (tau
    /// `model_limits_discovery_error`).
    #[must_use]
    pub fn model_limits_discovery_error(&self) -> Option<&str> {
        self.model_limits_discovery_error.as_deref()
    }

    /// The live model limits discovered for the *currently active*
    /// (provider, model), or `None` when the cached key no longer matches or
    /// nothing was discovered (tau's `_runtime_model_limits_key` guard).
    fn active_runtime_model_limits(&self) -> Option<&RuntimeModelLimits> {
        let matches = self
            .runtime_model_limits_key
            .as_ref()
            .is_some_and(|(provider, model)| {
                provider == self.provider_name() && model == &self.model()
            });
        if matches {
            self.runtime_model_limits.as_ref()
        } else {
            None
        }
    }

    // ---- provider config internals ---------------------------------------

    /// The active provider's config from provider settings, if any (tau
    /// `_active_provider_config`).
    fn active_provider_config(&self) -> Option<ProviderConfig> {
        active_provider_config_from(self.provider_settings.as_ref(), self.provider_name())
    }

    /// Provider configs from settings that have usable credentials (tau
    /// `_usable_provider_configs`).
    fn usable_provider_configs(&self) -> Vec<ProviderConfig> {
        let Some(settings) = self.provider_settings.as_ref() else {
            return Vec::new();
        };
        settings
            .providers
            .iter()
            .filter(|provider| self.provider_is_usable(provider))
            .cloned()
            .collect()
    }

    /// Whether a provider has usable credentials (tau `_provider_is_usable`).
    fn provider_is_usable(&self, provider: &ProviderConfig) -> bool {
        provider_has_usable_credentials(provider, Some(self.credential_store.as_ref()))
    }

    /// Effective automatic compaction threshold, if any.
    #[must_use]
    pub fn auto_compact_token_threshold(&self) -> Option<i64> {
        if !self.auto_compact_enabled {
            return None;
        }
        if self.auto_compact_token_threshold.is_some() {
            return self.auto_compact_token_threshold;
        }
        if let Some(limits) = self.active_runtime_model_limits() {
            return Some(limits.effective_auto_compact_token_limit());
        }
        auto_compaction_threshold_for_context_window(self.context_window_tokens())
    }

    // ---- turn drivers -----------------------------------------------------

    /// Append a user prompt, run the agent, and persist new messages.
    ///
    /// The returned stream yields [`CodingSessionEvent`]s. Message-lifecycle
    /// events are the durable-message boundary: each completed message is
    /// appended immediately with a trailing leaf pointer.
    pub fn prompt(
        &mut self,
        content: String,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> impl futures::Stream<Item = CodingSessionEvent> + '_ {
        async_stream::stream! {
            self.run_error = None;
            let context = self.diagnostic_context();

            // Run `input` hooks on the RAW prompt, before expansion (tau
            // `prompt`): a `handled` outcome consumes the input (optionally
            // notifying) and starts no agent run; a `transform` replaces the
            // text. Guarded so a session with no extensions pays nothing.
            let mut content = content;
            if self.extension_runtime.has_extensions() {
                let behavior = match streaming_behavior {
                    Some(StreamingBehavior::Steer) => Some("steer".to_string()),
                    Some(StreamingBehavior::FollowUp) => Some("follow_up".to_string()),
                    None => None,
                };
                let outcome = self
                    .extension_runtime
                    .run_input_hooks(&content, "interactive", behavior)
                    .await;
                if outcome.handled {
                    if let Some(message) = outcome.message {
                        self.extension_runtime.notify(&message, "info").await;
                    }
                    return;
                }
                content = outcome.text;
            }

            // tau raises the `/skill:` `ResourceError` out of `prompt()`, aborting
            // the turn; rho records it on the session so the print-mode CLI exits
            // non-zero (mirroring the persistence-failure path).
            let expanded_content = match self.expand_prompt_text(&content) {
                Ok(expanded) => expanded,
                Err(err) => {
                    self.run_error = Some(err.0);
                    return;
                }
            };

            if self.harness.is_running() {
                match streaming_behavior {
                    Some(StreamingBehavior::Steer) => {
                        self.harness.steer(&expanded_content);
                        yield CodingSessionEvent::Session(self.queue_update_event().into());
                        return;
                    }
                    Some(StreamingBehavior::FollowUp) => {
                        self.harness.follow_up(&expanded_content);
                        yield CodingSessionEvent::Session(self.queue_update_event().into());
                        return;
                    }
                    None => return,
                }
            }

            self.refresh_runtime_model_limits().await;
            self.try_auto_compact(&context, "auto_compact_before_prompt")
                .await;
            let mut persisted_count = self.harness.messages().len();
            let mut auto_name_attempted = false;
            let mut overflow_message: Option<AssistantMessage> = None;

            let mut prompt_msg = UserMessage::new(expanded_content);
            prompt_msg.timestamp = self.config.clock.now_ms();
            let prompt_msg = AgentMessage::User(prompt_msg);

            let Ok(mut events) = self.harness.prompt_message(prompt_msg) else {
                return;
            };
            self.invalidate_context_usage_cache();
            while let Some(event) = events.next().await {
                // Fan each canonical agent event out to subscribed extensions
                // (tau's harness listener, dispatched inline here). Guarded so a
                // session with no extensions pays nothing.
                if self.extension_runtime.has_extensions() {
                    self.extension_runtime.on_agent_event(&event).await;
                }
                if let AgentEvent::MessageEnd(ref e) = event {
                    let Some(count) =
                        self.persist_or_log(persisted_count, &context, "agent_loop").await
                    else {
                        return;
                    };
                    persisted_count = count;
                    if !auto_name_attempted {
                        if let AgentMessage::User(ref u) = e.message {
                            auto_name_attempted = true;
                            let text = u.text();
                            self.try_auto_name_session(&text, &context).await;
                        }
                    }
                    if let AgentMessage::Assistant(ref a) = e.message {
                        if a.stop_reason == StopReason::Error {
                            let path = self
                                .diagnostic_logger
                                .log_assistant_error(&context, "agent_loop", a);
                            self.last_diagnostic_log_path = Some(path);
                            if is_context_overflow_error(a) {
                                overflow_message = Some(a.clone());
                            }
                        }
                    }
                }
                if matches!(event, AgentEvent::ToolExecutionEnd(_)) {
                    self.invalidate_context_usage_cache();
                }
                match event {
                    AgentEvent::AgentEnd(e) => {
                        yield CodingSessionEvent::Session(
                            SessionAgentEndEvent::new(e.messages, false).into(),
                        );
                    }
                    other => yield CodingSessionEvent::Agent(other),
                }
            }
            if self
                .persist_or_log(persisted_count, &context, "agent_loop")
                .await
                .is_none()
            {
                return;
            }

            if let Some(overflow) = overflow_message {
                yield CodingSessionEvent::Session(
                    CompactionStartEvent::new(CompactionReason::Overflow).into(),
                );
                let compacted = self.try_overflow_compact(&context).await;
                let mut end = CompactionEndEvent::new(CompactionReason::Overflow);
                end.aborted = !compacted;
                end.will_retry = compacted;
                end.error_message = if compacted {
                    None
                } else {
                    Some("Overflow compaction failed".to_string())
                };
                yield CodingSessionEvent::Session(end.into());
                if compacted {
                    let err_msg = overflow
                        .error_message
                        .clone()
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| "Context overflow".to_string());
                    yield CodingSessionEvent::Session(
                        AutoRetryStartEvent::new(1, 1, 0, err_msg).into(),
                    );
                    let mut retry_persisted = self.harness.messages().len();
                    if let Ok(mut retry_events) = self.harness.continue_() {
                        self.invalidate_context_usage_cache();
                        while let Some(event) = retry_events.next().await {
                            if self.extension_runtime.has_extensions() {
                                self.extension_runtime.on_agent_event(&event).await;
                            }
                            if let AgentEvent::MessageEnd(ref e) = event {
                                let Some(count) = self
                                    .persist_or_log(retry_persisted, &context, "agent_loop_retry")
                                    .await
                                else {
                                    return;
                                };
                                retry_persisted = count;
                                if let AgentMessage::Assistant(ref a) = e.message {
                                    if a.stop_reason == StopReason::Error {
                                        let path = self.diagnostic_logger.log_assistant_error(
                                            &context,
                                            "agent_loop_retry",
                                            a,
                                        );
                                        self.last_diagnostic_log_path = Some(path);
                                    }
                                }
                            }
                            if matches!(event, AgentEvent::ToolExecutionEnd(_)) {
                                self.invalidate_context_usage_cache();
                            }
                            match event {
                                AgentEvent::AgentEnd(e) => {
                                    yield CodingSessionEvent::Session(
                                        SessionAgentEndEvent::new(e.messages, false).into(),
                                    );
                                }
                                other => yield CodingSessionEvent::Agent(other),
                            }
                        }
                        if self
                            .persist_or_log(retry_persisted, &context, "agent_loop_retry")
                            .await
                            .is_none()
                        {
                            return;
                        }
                    }
                    yield CodingSessionEvent::Session(AutoRetryEndEvent::new(true, 1, None).into());
                }
                yield CodingSessionEvent::Session(AgentSettledEvent::new().into());
                return;
            }

            self.try_auto_compact(&context, "auto_compact_after_prompt")
                .await;
            yield CodingSessionEvent::Session(AgentSettledEvent::new().into());
        }
    }

    /// Continue the agent from restored state, persisting new messages.
    pub fn continue_(&mut self) -> impl futures::Stream<Item = CodingSessionEvent> + '_ {
        async_stream::stream! {
            self.run_error = None;
            let context = self.diagnostic_context();
            self.refresh_runtime_model_limits().await;
            let mut persisted_count = self.harness.messages().len();
            let Ok(mut events) = self.harness.continue_() else {
                return;
            };
            self.invalidate_context_usage_cache();
            while let Some(event) = events.next().await {
                // Fan each canonical agent event out to subscribed extensions
                // (tau's harness listener, dispatched inline here). Guarded so a
                // session with no extensions pays nothing.
                if self.extension_runtime.has_extensions() {
                    self.extension_runtime.on_agent_event(&event).await;
                }
                if let AgentEvent::MessageEnd(ref e) = event {
                    let Some(count) =
                        self.persist_or_log(persisted_count, &context, "agent_loop").await
                    else {
                        return;
                    };
                    persisted_count = count;
                    if let AgentMessage::Assistant(ref a) = e.message {
                        if a.stop_reason == StopReason::Error {
                            let path = self
                                .diagnostic_logger
                                .log_assistant_error(&context, "agent_loop", a);
                            self.last_diagnostic_log_path = Some(path);
                        }
                    }
                }
                if matches!(event, AgentEvent::ToolExecutionEnd(_)) {
                    self.invalidate_context_usage_cache();
                }
                match event {
                    AgentEvent::AgentEnd(e) => {
                        yield CodingSessionEvent::Session(
                            SessionAgentEndEvent::new(e.messages, false).into(),
                        );
                    }
                    other => yield CodingSessionEvent::Agent(other),
                }
            }
            if self
                .persist_or_log(persisted_count, &context, "agent_loop")
                .await
                .is_none()
            {
                return;
            }
            self.try_auto_compact(&context, "auto_compact_after_continue")
                .await;
            yield CodingSessionEvent::Session(AgentSettledEvent::new().into());
        }
    }

    // ---- thinking ---------------------------------------------------------

    /// Persist and activate a thinking mode for future turns (tau
    /// `set_thinking_level`).
    pub async fn set_thinking_level(&mut self, level: &str) -> Result<String, SessionError> {
        let normalized = normalize_thinking_level(Some(level)).map_err(SessionError::Value)?;
        let available = self.available_thinking_levels();
        if available.is_empty() {
            return Err(SessionError::Value(unavailable_thinking_message(self)));
        }
        if !available.contains(&normalized) {
            let modes = available.join(", ");
            return Err(SessionError::Value(format!(
                "Thinking mode {normalized} is not available for {}:{}. Available modes: {modes}",
                self.provider_name(),
                self.model(),
            )));
        }
        if normalized == self.thinking_level {
            return Ok(format!("Thinking mode: {normalized}"));
        }

        let previous = self.thinking_level.clone();
        self.thinking_level = normalized.clone();
        if let Err(err) = self.refresh_runtime_provider() {
            self.thinking_level = previous;
            return Err(err);
        }

        let mut entry = ThinkingLevelChangeEntry::new(Some(normalized.clone()));
        entry.id = self.config.id_gen.new_id();
        entry.timestamp = self.config.clock.now_secs();
        entry.parent_id = self.last_parent_id.clone();
        let entry_id = entry.id.clone();
        self.append_session_entry(SessionEntry::ThinkingLevelChange(entry))
            .await?;
        self.append_leaf(Some(entry_id.clone())).await?;
        self.last_parent_id = Some(entry_id.clone());
        self.persist_thinking_level_choice();
        self.refresh_persisted_state(Some(&entry_id)).await?;
        Ok(format!("Thinking mode: {normalized}"))
    }

    /// Cycle to the next supported thinking mode and persist it (tau
    /// `cycle_thinking_level`).
    pub async fn cycle_thinking_level(&mut self) -> Result<String, SessionError> {
        let available = self.available_thinking_levels();
        let available_refs: Vec<&str> = available.iter().map(String::as_str).collect();
        let next = next_thinking_level(Some(&self.thinking_level), &available_refs);
        self.set_thinking_level(&next).await
    }

    // ---- provider / model mutators ---------------------------------------

    /// Switch the active model for future turns and make it the default (tau
    /// `set_model`).
    pub fn set_model(&mut self, model: &str) -> Result<(), SessionError> {
        if let Some(provider) = self.active_provider_config() {
            validate_provider_model(&provider, model)?;
        }
        self.set_harness_model(model);
        self.sync_thinking_level_to_active_model();
        self.refresh_runtime_provider()?;
        self.persist_default_model_choice();
        if let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) {
            let _ =
                manager.touch_session(session_id, Some(model), Some(self.provider_name()), None);
        }
        self.refresh_extension_context();
        Ok(())
    }

    /// Switch provider/model as one operation (tau `set_model_choice`).
    pub fn set_model_choice(&mut self, choice: &ModelChoice) -> Result<(), SessionError> {
        if choice.provider_name == self.provider_name() {
            return self.set_model(&choice.model);
        }
        self.set_provider_model(&choice.provider_name, &choice.model, true)
    }

    /// Whether a provider/model pair is in the scoped model list (tau
    /// `is_scoped_model`).
    #[must_use]
    pub fn is_scoped_model(&self, choice: &ModelChoice) -> bool {
        self.scoped_model_choices().contains(choice)
    }

    /// Add or remove a model from the persisted scoped model list (tau
    /// `toggle_scoped_model`).
    pub fn toggle_scoped_model(
        &mut self,
        choice: &ModelChoice,
    ) -> Result<Vec<ModelChoice>, SessionError> {
        if self.provider_settings.is_none() {
            return Err(SessionError::Value(
                "Provider settings are not available for this session".to_string(),
            ));
        }
        let available: std::collections::HashSet<ModelChoice> =
            self.available_model_choices().into_iter().collect();
        if !available.contains(choice) {
            return Err(SessionError::Value(format!(
                "Model is not available: {}:{}",
                choice.provider_name, choice.model
            )));
        }
        let updated = toggle_saved_scoped_model(
            &choice.provider_name,
            &choice.model,
            self.resource_paths.paths.as_ref(),
            self.provider_settings.as_ref(),
            Some(self.credential_store.as_ref()),
        )?;
        self.provider_settings = Some(updated);
        self.sync_thinking_level_to_active_model();
        Ok(self.scoped_model_choices())
    }

    /// Switch to the next configured scoped model (tau `cycle_scoped_model`).
    pub fn cycle_scoped_model(&mut self, reverse: bool) -> Result<ModelChoice, SessionError> {
        let scoped = self.scoped_model_choices();
        if scoped.is_empty() {
            return Err(SessionError::Value(
                "No scoped models configured.".to_string(),
            ));
        }
        let current = ModelChoice::new(self.provider_name(), self.model());
        let current_index = scoped.iter().position(|choice| *choice == current);
        // tau: a missing current maps to -1 forward / 0 reverse, so the next
        // step lands on the first / last configured scoped model.
        let base = match current_index {
            Some(index) => index as isize,
            None => {
                if reverse {
                    0
                } else {
                    -1
                }
            }
        };
        let delta: isize = if reverse { -1 } else { 1 };
        let len = scoped.len() as isize;
        let index = ((base + delta) % len + len) % len;
        let choice = scoped[index as usize].clone();
        self.set_model_choice(&choice)?;
        Ok(choice)
    }

    /// Switch the active provider and reset to its default model (tau
    /// `set_provider`).
    pub fn set_provider(
        &mut self,
        provider_name: &str,
        persist_default: bool,
    ) -> Result<(), SessionError> {
        let Some(settings) = self.provider_settings.as_ref() else {
            return Err(SessionError::Value(
                "Provider settings are not available for this session".to_string(),
            ));
        };
        let provider = settings.get_provider(Some(provider_name))?;
        let default_model = provider.default_model().to_string();
        self.set_provider_model(provider_name, &default_model, persist_default)
    }

    /// Switch active provider/model, building a fresh runtime provider (tau
    /// `_set_provider_model`).
    fn set_provider_model(
        &mut self,
        provider_name: &str,
        model: &str,
        persist_default: bool,
    ) -> Result<(), SessionError> {
        let Some(settings) = self.provider_settings.as_ref() else {
            return Err(SessionError::Value(
                "Provider settings are not available for this session".to_string(),
            ));
        };
        let provider_config = settings.get_provider(Some(provider_name))?.clone();
        if !provider_config.models().iter().any(|m| m == model) {
            return Err(SessionError::Value(format!(
                "Model is not configured: {provider_name}:{model}"
            )));
        }
        let thinking_level =
            coerced_thinking_level(&provider_config, model, &self.thinking_level, None);
        let provider = create_model_provider(
            &provider_config,
            Some(self.credential_store.clone()),
            Some(model),
            Some(&thinking_level),
        )?;
        self.owned_providers.push(provider.clone());
        self.set_harness_provider_and_model(provider, model);
        self.config.provider_name = provider_config.name().to_string();
        self.runtime_provider_config = Some(provider_config);
        self.thinking_level = thinking_level;
        if persist_default {
            self.persist_default_model_choice();
        }
        if let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) {
            let _ =
                manager.touch_session(session_id, Some(model), Some(self.provider_name()), None);
        }
        self.refresh_extension_context();
        Ok(())
    }

    /// Reload provider settings for login and model-selection flows (tau
    /// `reload_provider_settings`).
    pub fn reload_provider_settings(&mut self) -> Result<(), SessionError> {
        if self.provider_settings.is_none() {
            return Ok(());
        }
        let previous_settings = self.provider_settings.clone();
        let previous_thinking_level = self.thinking_level.clone();
        self.provider_settings = Some(load_provider_settings(
            self.resource_paths.paths.as_ref(),
            Some(self.credential_store.as_ref()),
        )?);
        self.sync_thinking_level_to_active_model();
        if let Err(err) = self.refresh_runtime_provider() {
            self.provider_settings = previous_settings;
            self.thinking_level = previous_thinking_level;
            return Err(err);
        }
        Ok(())
    }

    fn persist_default_model_choice(&mut self) {
        if self.provider_settings.is_none() {
            return;
        }
        if let Ok(updated) = save_default_provider_model(
            self.provider_name(),
            &self.model(),
            self.resource_paths.paths.as_ref(),
            self.provider_settings.as_ref(),
            Some(self.credential_store.as_ref()),
        ) {
            self.provider_settings = Some(updated);
        }
        self.sync_thinking_level_to_active_model();
    }

    fn persist_thinking_level_choice(&mut self) {
        if self.provider_settings.is_none() {
            return;
        }
        let Some(provider) = self.active_provider_config() else {
            return;
        };
        if !provider_thinking_levels(&provider, Some(&self.model())).contains(&self.thinking_level)
        {
            return;
        }
        if let Ok(updated) = save_provider_thinking_level(
            self.provider_name(),
            &self.model(),
            &self.thinking_level,
            self.resource_paths.paths.as_ref(),
            self.provider_settings.as_ref(),
            Some(self.credential_store.as_ref()),
        ) {
            self.provider_settings = Some(updated);
        }
    }

    fn sync_thinking_level_to_active_model(&mut self) {
        let Some(provider) = self.active_provider_config() else {
            return;
        };
        let model = self.model();
        let preferred = provider.thinking_defaults().get(&model).cloned();
        self.thinking_level = coerced_thinking_level(
            &provider,
            &model,
            &self.thinking_level,
            preferred.as_deref(),
        );
    }

    fn refresh_runtime_provider(&mut self) -> Result<(), SessionError> {
        let Some(runtime_config) = self.runtime_provider_config.clone() else {
            return Ok(());
        };
        let provider_config = self.active_provider_config().unwrap_or(runtime_config);
        let model = self.model();
        validate_provider_model(&provider_config, &model)?;
        let provider = create_model_provider(
            &provider_config,
            Some(self.credential_store.clone()),
            Some(&model),
            Some(&self.thinking_level),
        )?;
        self.owned_providers.push(provider.clone());
        self.set_harness_provider(provider);
        self.runtime_provider_config = Some(provider_config);
        Ok(())
    }

    /// Rebuild the live harness with a new model, preserving the transcript.
    ///
    /// rho's `AgentHarness` exposes no in-place model/provider setter (tau
    /// mutates `harness.config.model` directly); the port instead rebuilds the
    /// harness from a cloned config, which is safe because model/provider swaps
    /// happen between turns.
    fn set_harness_model(&mut self, model: &str) {
        let mut config = self.harness.config().clone();
        config.model = model.to_string();
        self.harness = AgentHarness::new(config, self.harness.messages());
        self.invalidate_runtime_model_limits();
    }

    fn set_harness_provider(&mut self, provider: Arc<dyn ModelProvider>) {
        let mut config = self.harness.config().clone();
        config.provider = provider;
        self.harness = AgentHarness::new(config, self.harness.messages());
        self.invalidate_runtime_model_limits();
    }

    fn set_harness_provider_and_model(&mut self, provider: Arc<dyn ModelProvider>, model: &str) {
        let mut config = self.harness.config().clone();
        config.provider = provider;
        config.model = model.to_string();
        self.harness = AgentHarness::new(config, self.harness.messages());
        self.invalidate_runtime_model_limits();
    }

    /// Drop any discovered model limits after a model/provider swap (tau
    /// `_invalidate_runtime_model_limits`). The next turn re-discovers them.
    fn invalidate_runtime_model_limits(&mut self) {
        self.runtime_model_limits = None;
        self.runtime_model_limits_key = None;
        self.model_limits_discovery_error = None;
    }

    /// Discover live model limits for the active (provider, model) if not already
    /// cached for that pair (tau `_refresh_runtime_model_limits`). A provider that
    /// exposes no live catalog returns `None` from the default trait hook, so the
    /// session simply falls back to the static catalog. Discovery failures are
    /// recorded (not fatal) for `/status`.
    async fn refresh_runtime_model_limits(&mut self) {
        let key = (self.provider_name().to_string(), self.model());
        if self.runtime_model_limits_key.as_ref() == Some(&key) {
            return;
        }
        self.runtime_model_limits = None;
        self.runtime_model_limits_key = Some(key.clone());
        self.model_limits_discovery_error = None;
        let provider = self.harness.config().provider.clone();
        match provider.discover_model_limits(&key.1).await {
            Ok(limits) => self.runtime_model_limits = limits,
            Err(err) => self.model_limits_discovery_error = Some(err),
        }
    }

    // ---- compaction -------------------------------------------------------

    /// Generate a manual compaction summary and rebuild active context.
    pub async fn compact(&mut self, instructions: Option<&str>) -> Result<String, SessionError> {
        let plan = self.manual_compaction_plan()?;
        let summary = self
            .generate_compaction_summary(&plan.messages_to_summarize, instructions)
            .await?;
        let replaced = plan.replace_entry_ids.len();
        self.append_compaction(&summary, plan.replace_entry_ids)
            .await?;
        Ok(format!("Compacted {replaced} context entries."))
    }

    async fn try_auto_compact(
        &mut self,
        context: &AgentCallDiagnosticContext,
        phase: &str,
    ) -> bool {
        // Automatic compaction must never lose a turn (tau `_try_auto_compact`):
        // on failure, log a diagnostic (remembering its path) and carry on.
        match self.maybe_auto_compact().await {
            Ok(compacted) => compacted,
            Err(err) => self.log_compaction_failure(context, phase, &err),
        }
    }

    async fn try_overflow_compact(&mut self, context: &AgentCallDiagnosticContext) -> bool {
        // A `None` plan is not a failure (nothing to compact); only a raised
        // error is logged, so the original overflow stays visible (tau
        // `_try_overflow_compact`, phase `overflow_compact`).
        let Some(plan) = self.recent_preserving_compaction_plan() else {
            return false;
        };
        let summary = match self
            .generate_compaction_summary(&plan.messages_to_summarize, None)
            .await
        {
            Ok(summary) => summary,
            Err(err) => return self.log_compaction_failure(context, "overflow_compact", &err),
        };
        match self
            .append_compaction(&summary, plan.replace_entry_ids)
            .await
        {
            Ok(()) => true,
            Err(err) => self.log_compaction_failure(context, "overflow_compact", &err),
        }
    }

    /// Record a compaction-failure diagnostic and return `false` (no compaction
    /// happened). Mirrors tau's `log_exception` + `_last_diagnostic_log_path`
    /// stash so the failure is surfaced without aborting the turn.
    fn log_compaction_failure(
        &mut self,
        context: &AgentCallDiagnosticContext,
        phase: &str,
        err: &SessionError,
    ) -> bool {
        let path = self.diagnostic_logger.log_exception(
            context,
            phase,
            err.error_type(),
            &err.to_string(),
        );
        self.last_diagnostic_log_path = Some(path);
        false
    }

    async fn maybe_auto_compact(&mut self) -> Result<bool, SessionError> {
        let Some(threshold) = self.auto_compact_token_threshold() else {
            return Ok(false);
        };
        if threshold <= 0 {
            return Ok(false);
        }
        if self.state.context_entry_ids.len() < 2 {
            return Ok(false);
        }
        if self.context_token_estimate() <= threshold {
            return Ok(false);
        }
        let Some(plan) = self.recent_preserving_compaction_plan() else {
            return Ok(false);
        };
        let summary = self
            .generate_compaction_summary(&plan.messages_to_summarize, None)
            .await?;
        self.append_compaction(&summary, plan.replace_entry_ids)
            .await?;
        Ok(true)
    }

    async fn generate_compaction_summary(
        &self,
        messages: &[AgentMessage],
        custom_instructions: Option<&str>,
    ) -> Result<String, SessionError> {
        let prompt = build_compaction_summary_prompt(messages, custom_instructions);
        let mut user = UserMessage::new(prompt);
        user.timestamp = self.config.clock.now_ms();
        let summary_messages = vec![AgentMessage::User(user)];
        let (text_parts, final_text, error) =
            drive_text_stream(self, SUMMARIZATION_SYSTEM_PROMPT, &summary_messages).await;
        if let Some(err) = error {
            return Err(SessionError::Value(format!(
                "Compaction summarization failed: {err}"
            )));
        }
        let summary = final_text.unwrap_or(text_parts).trim().to_string();
        if summary.is_empty() {
            return Err(SessionError::Value(
                "Compaction summarization returned an empty summary".to_string(),
            ));
        }
        Ok(summary)
    }

    async fn summarize_branch_messages(
        &self,
        messages: &[AgentMessage],
        custom_instructions: Option<&str>,
        replace_instructions: bool,
    ) -> String {
        let summary = summarize_branch_messages_with_model(
            // Read the live provider from the harness (tau
            // `self._harness.config.provider`, session.py:1921), which is
            // rebuilt on every model/provider switch — not the session-config
            // provider, which is only the initial one and would be stale after
            // a cross-provider `/model`.
            self.harness.config().provider.as_ref(),
            &self.model(),
            messages,
            custom_instructions,
            replace_instructions,
        )
        .await;
        summary.unwrap_or_else(|| summarize_messages_for_compaction(messages))
    }

    fn manual_compaction_plan(&self) -> Result<CompactionPlan, SessionError> {
        let rows = self.active_context_rows();
        if rows.is_empty() {
            return Err(SessionError::Value(
                "No active context messages to compact".to_string(),
            ));
        }
        Ok(CompactionPlan {
            replace_entry_ids: rows.iter().map(|(id, _)| id.clone()).collect(),
            messages_to_summarize: rows.iter().map(|(_, m)| m.clone()).collect(),
        })
    }

    fn recent_preserving_compaction_plan(&self) -> Option<CompactionPlan> {
        let rows = self.active_context_rows();
        if rows.len() < 2 {
            return None;
        }
        let first_kept = first_recent_context_index(&rows, DEFAULT_COMPACTION_KEEP_RECENT_TOKENS);
        if first_kept <= 0 {
            return None;
        }
        let first_kept = first_kept as usize;
        let replaced = &rows[..first_kept];
        if replaced.is_empty() {
            return None;
        }
        Some(CompactionPlan {
            replace_entry_ids: replaced.iter().map(|(id, _)| id.clone()).collect(),
            messages_to_summarize: replaced.iter().map(|(_, m)| m.clone()).collect(),
        })
    }

    fn active_context_rows(&self) -> Vec<(String, AgentMessage)> {
        self.state
            .context_entry_ids
            .iter()
            .cloned()
            .zip(self.state.messages.iter().cloned())
            .collect()
    }

    async fn append_compaction(
        &mut self,
        summary: &str,
        replace_entry_ids: Vec<String>,
    ) -> Result<(), SessionError> {
        if replace_entry_ids.is_empty() {
            return Err(SessionError::Value(
                "No active context messages to compact".to_string(),
            ));
        }
        let mut compaction = CompactionEntry::new(summary.to_string(), replace_entry_ids);
        compaction.id = self.config.id_gen.new_id();
        compaction.timestamp = self.config.clock.now_secs();
        compaction.parent_id = self.last_parent_id.clone();
        let compaction_id = compaction.id.clone();
        self.append_session_entry(SessionEntry::Compaction(compaction))
            .await?;
        self.append_leaf(Some(compaction_id.clone())).await?;
        self.last_parent_id = Some(compaction_id.clone());
        self.refresh_persisted_state(Some(&compaction_id)).await?;
        self.harness.replace_messages(self.state.messages.clone());
        self.invalidate_context_usage_cache();
        Ok(())
    }

    // ---- branch -----------------------------------------------------------

    /// Return branchable session entries for a tree picker.
    pub async fn tree_choices(&self) -> Result<Vec<SessionTreeChoice>, SessionError> {
        let entries = self.read_session_entries().await?;
        let indents = tree_branch_indents(&entries);
        let active = self.state.active_leaf_id.clone();
        let mut out = Vec::new();
        for entry in ordered_tree_entries(&entries) {
            if !is_branchable_tree_entry(&entry) {
                continue;
            }
            let id = entry.id().to_string();
            out.push(SessionTreeChoice {
                label: tree_choice_label(&entry, *indents.get(&id).unwrap_or(&0)),
                active: Some(id.clone()) == active,
                is_tool_call: is_tool_call_tree_entry(&entry),
                entry_id: id,
            });
        }
        Ok(out)
    }

    /// Move the active leaf to a previous entry, preserving existing history.
    pub async fn branch_to_entry(
        &mut self,
        entry_id: &str,
        summarize: bool,
        custom_instructions: Option<&str>,
        replace_instructions: bool,
    ) -> Result<SessionTreeBranchResult, SessionError> {
        if self.harness.is_running() {
            return Err(SessionError::Value(
                "rho is still working. Interrupt before branching.".to_string(),
            ));
        }
        let entries = self.read_session_entries().await?;
        let Some(selected) = entries.iter().find(|e| e.id() == entry_id).cloned() else {
            return Err(SessionError::Value(format!(
                "Unknown session entry: {entry_id}"
            )));
        };
        if !is_branchable_tree_entry(&selected) {
            return Err(SessionError::Value(format!(
                "Session entry cannot be branched from: {entry_id}"
            )));
        }

        let mut target_id: Option<String> = Some(entry_id.to_string());
        let mut input_prefill: Option<String> = None;
        let mut summarized = false;
        if summarize {
            let abandoned = messages_after_entry_on_active_path(
                &entries,
                entry_id,
                self.last_parent_id.as_deref(),
            );
            if !abandoned.is_empty() {
                let summary = self
                    .summarize_branch_messages(
                        &abandoned,
                        custom_instructions,
                        replace_instructions,
                    )
                    .await;
                let mut summary_entry = BranchSummaryEntry::new(summary);
                summary_entry.id = self.config.id_gen.new_id();
                summary_entry.timestamp = self.config.clock.now_secs();
                summary_entry.parent_id = Some(entry_id.to_string());
                summary_entry.branch_root_id = Some(entry_id.to_string());
                let sid = summary_entry.id.clone();
                self.append_session_entry(SessionEntry::BranchSummary(summary_entry))
                    .await?;
                target_id = Some(sid);
                summarized = true;
            }
        } else if let SessionEntry::Message(ref m) = selected {
            if let AgentMessage::User(ref u) = m.message {
                target_id = m.parent_id.clone();
                input_prefill = Some(u.text());
            }
        }

        self.append_leaf(target_id.clone()).await?;
        self.last_parent_id = target_id.clone();
        self.refresh_persisted_state(target_id.as_deref()).await?;
        self.harness.replace_messages(self.state.messages.clone());
        self.invalidate_context_usage_cache();
        let default_thinking = self.default_thinking_level_for_active_model();
        self.thinking_level = state_thinking_level(&self.state, &default_thinking);
        self.sync_thinking_level_to_active_model();
        self.refresh_runtime_provider()?;

        if let Some(prefill) = input_prefill {
            return Ok(SessionTreeBranchResult {
                message: format!("Branched session before {entry_id}."),
                input_prefill: Some(prefill),
            });
        }
        let suffix = if summarized {
            " with branch summary"
        } else {
            ""
        };
        let target = target_id.unwrap_or_default();
        Ok(SessionTreeBranchResult {
            message: format!("Branched session at {target}{suffix}."),
            input_prefill: None,
        })
    }

    // ---- terminal command -------------------------------------------------

    /// Run a shell command in the session cwd, optionally adding output to context.
    pub async fn run_terminal_command(
        &mut self,
        command: &str,
        add_to_context: bool,
    ) -> Result<TerminalCommandResult, SessionError> {
        let normalized = command.trim().to_string();
        if normalized.is_empty() {
            return Err(SessionError::Value(
                "Terminal command cannot be empty".to_string(),
            ));
        }
        let bash_tool = create_bash_tool(
            &self.config.cwd,
            self.config.shell_command_prefix.as_deref(),
        );
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::Value::String(normalized.clone()),
        );
        let result = bash_tool
            .execute("terminal-command".to_string(), args, None, Arc::new(|_| {}))
            .await
            .map_err(|e| SessionError::Value(e.to_string()))?;
        let exit_code = result
            .details
            .as_ref()
            .and_then(|d| d.get("exit_code"))
            .and_then(serde_json::Value::as_i64);

        if add_to_context {
            let before = self.harness.messages().len();
            let mut msg = UserMessage::new(terminal_command_context_message(
                &normalized,
                &result.text(),
            ));
            msg.timestamp = self.config.clock.now_ms();
            self.harness.append_message(AgentMessage::User(msg));
            self.invalidate_context_usage_cache();
            self.persist_messages_since(before).await?;
        }

        Ok(TerminalCommandResult {
            command: normalized,
            output: result.text(),
            exit_code,
            ok: exit_code == Some(0),
            added_to_context: add_to_context,
        })
    }

    /// Handle a coding-session slash command (tau `handle_command`).
    ///
    /// A `/name [args]` prompt-template command is an *expansion directive*, not
    /// a slash command, so it stays unhandled here and flows through `prompt()`
    /// for on-the-fly replacement; everything else dispatches to the registry.
    pub fn handle_command(&mut self, text: &str) -> CommandResult {
        if expand_prompt_template_command(text, &self.prompt_templates).is_some() {
            return CommandResult::default();
        }
        // The registry is rebuilt per call (stateless) to avoid a borrow
        // conflict (`&registry` + `&mut self`). When extensions are loaded it
        // layers their commands over the defaults (tau
        // `build_command_registry`); otherwise it is the plain default registry
        // — byte-identical to before for extension-free sessions.
        let registry = if self.extension_runtime.has_extensions() {
            self.extension_runtime.build_command_registry()
        } else {
            create_default_command_registry()
        };
        registry.execute(self, text)
    }

    /// Expand prompt text using loaded markdown resources.
    ///
    /// tau `expand_prompt_text`: a `/name [args]` prompt-template command wins
    /// first (it never errors), then a `/skill:name` command (which raises
    /// `ResourceError` — a `ValueError` in tau — for an unknown/empty skill).
    /// Otherwise the text passes through unchanged.
    pub fn expand_prompt_text(&self, text: &str) -> Result<String, ResourceError> {
        if let Some(expanded) = expand_prompt_template_command(text, &self.prompt_templates) {
            return Ok(expanded);
        }
        match expand_skill_command(text, &self.skills)? {
            Some(expanded) => Ok(expanded),
            None => Ok(text.to_string()),
        }
    }

    // ---- export / reload --------------------------------------------------

    /// Export the current session to a user-facing artifact (tau `export`).
    pub async fn export(
        &self,
        destination: Option<PathBuf>,
        format: Option<&str>,
    ) -> Result<PathBuf, SessionError> {
        let entries = self.read_session_entries().await?;
        let session_path = self.storage().storage_path();
        // tau: an explicit `format`, else the destination's suffix, else html.
        let inferred_format = format.map(str::to_string).or_else(|| {
            destination
                .as_ref()
                .and_then(|d| d.extension())
                .map(|ext| ext.to_string_lossy().into_owned())
        });
        let export_format = normalize_export_format(inferred_format.as_deref())
            .map_err(|err| SessionError::Export(err.to_string()))?;
        let output_path = resolve_export_destination(
            destination.as_deref(),
            &self.config.cwd,
            session_path.as_deref(),
            &export_format,
        );
        let title = self.session_export_title();
        let source = session_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| self.session_id().map(str::to_string));
        export_session_artifact(
            &entries,
            &output_path,
            &title,
            source.as_deref(),
            Some(&export_format),
        )
        .map_err(|err| SessionError::Export(err.to_string()))
    }

    /// Reload local coding resources and report the before/after delta (tau
    /// `reload`, with the extension runtime elided — rho has no extensions).
    ///
    /// Kept `async` for parity with tau (whose reload awaits the extension
    /// lifecycle) and with the command surface that awaits it; the elided body
    /// has nothing to await here.
    pub async fn reload(&mut self) -> Result<CodingReloadSummary, SessionError> {
        let before_skills = skill_signatures(&self.skills);
        let before_prompt_templates = prompt_template_signatures(&self.prompt_templates);
        let before_context_files = context_file_signatures(&self.context_files);
        let before_diagnostics = diagnostic_signatures(self.resource_diagnostics());
        let before_system_prompt_inputs =
            system_prompt_resource_signatures(&self.skills, &self.context_files);
        // Extension registration set + guidelines before the reload (tau tracks
        // live extension names + guidelines here).
        let before_extensions = self.extension_runtime.extension_names().len();
        let before_guidelines = self.extension_runtime.prompt_guidelines();

        // Re-discover extensions: fire `session_shutdown` for the outgoing
        // generation, tear it down, reload, then `session_start` for the new one
        // (tau's `_reload_extensions` lifecycle). A `NoopExtensionHost`-backed
        // runtime with no extensions reloads to the same empty set.
        if self.extension_runtime.has_extensions() {
            self.extension_runtime.emit_session_shutdown("reload").await;
        }
        self.extension_runtime.reset_for_reload().await;
        if self.config.extensions_enabled || !self.config.extension_paths.is_empty() {
            self.extension_runtime
                .load(
                    &self.resource_paths,
                    &self.config.extension_paths,
                    self.config.extensions_enabled,
                    self.config.project_extensions_enabled,
                )
                .await;
        }
        if self.extension_runtime.has_extensions() {
            let bridge = Arc::new(ExtSessionContextBridge::new(self.extension_context.clone()));
            self.extension_runtime.set_bridge(bridge);
            self.extension_runtime.rebind().await;
            self.extension_runtime.emit_session_start("reload").await;
        }
        let after_extensions = self.extension_runtime.extension_names().len();
        let after_guidelines = self.extension_runtime.prompt_guidelines();

        let resources = load_session_resources(
            &self.resource_paths,
            &self.config.context_files,
            self.config.skills_enabled,
        );

        let after_skills = skill_signatures(&resources.skills);
        let after_prompt_templates = prompt_template_signatures(&resources.prompt_templates);
        let after_context_files = context_file_signatures(&resources.context_files);
        let after_system_prompt_inputs =
            system_prompt_resource_signatures(&resources.skills, &resources.context_files);

        // Extensions changing means the composed tool set + prompt guidelines
        // may have changed, so both the harness tools and the system prompt need
        // rebuilding (tau folds the extension tool names + guidelines into the
        // prompt-input signature).
        let extensions_changed =
            before_extensions != after_extensions || before_guidelines != after_guidelines;

        // Recompose the harness tools when extensions changed (the base built-in
        // set is stable). With no extensions this branch never runs, so the
        // harness — and its subscribers — are left untouched.
        let recomposed_tools = if extensions_changed {
            let base_tools = self.config.tools.clone().unwrap_or_else(|| {
                create_coding_tools(
                    &self.config.cwd,
                    self.config.shell_command_prefix.as_deref(),
                )
            });
            Some(self.extension_runtime.compose_tools(base_tools))
        } else {
            None
        };

        let mut rebuilt_system_prompt: Option<String> = None;
        if self.config.system.is_none()
            && (before_system_prompt_inputs != after_system_prompt_inputs || extensions_changed)
        {
            let tools = recomposed_tools
                .clone()
                .unwrap_or_else(|| self.harness.config().tools.clone());
            rebuilt_system_prompt = Some(build_system_prompt(&BuildSystemPromptOptions {
                cwd: self.config.cwd.clone(),
                tools,
                custom_prompt: self.config.custom_system_prompt.clone(),
                append_system_prompt: self.config.append_system_prompt.clone(),
                context_files: resources.context_files.clone(),
                extra_guidelines: after_guidelines.clone(),
                ..Default::default()
            }));
        }
        let system_prompt_rebuilt = rebuilt_system_prompt.is_some();

        self.skills = resources.skills;
        self.prompt_templates = resources.prompt_templates;
        self.context_files = resources.context_files;
        self.resource_diagnostics = resources.diagnostics;
        let after_diagnostics = diagnostic_signatures(self.resource_diagnostics());
        if rebuilt_system_prompt.is_some() || recomposed_tools.is_some() {
            let mut config = self.harness.config().clone();
            if let Some(system) = rebuilt_system_prompt {
                config.system = system;
            }
            if let Some(tools) = recomposed_tools {
                config.tools = tools;
            }
            self.harness = AgentHarness::new(config, self.harness.messages());
            self.invalidate_context_usage_cache();
        }

        Ok(CodingReloadSummary {
            skills: category_summary_from(&before_skills, &after_skills),
            prompt_templates: category_summary_from(
                &before_prompt_templates,
                &after_prompt_templates,
            ),
            context_files: category_summary_from(&before_context_files, &after_context_files),
            extensions: ReloadCategorySummary::new(
                before_extensions,
                after_extensions,
                extensions_changed,
            ),
            diagnostics: category_summary_from(&before_diagnostics, &after_diagnostics),
            system_prompt_rebuilt,
        })
    }

    /// The active provider's preferred thinking level for the current model,
    /// falling back to the config default (tau
    /// `_default_thinking_level_for_active_model`).
    fn default_thinking_level_for_active_model(&self) -> String {
        match self.active_provider_config() {
            Some(provider) => preferred_thinking_level_for_model(
                &provider,
                &self.model(),
                &self.config.thinking_level,
            ),
            None => self.config.thinking_level.clone(),
        }
    }

    /// The export artifact title (tau `_session_export_title`).
    fn session_export_title(&self) -> String {
        if let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) {
            if let Some(record) = manager.get_session(session_id).ok().flatten() {
                if let Some(title) = record.title.filter(|t| !t.is_empty()) {
                    return title;
                }
            }
        }
        match self.session_id() {
            Some(session_id) => format!("Rho session {session_id}"),
            None => "Rho Session Export".to_string(),
        }
    }

    /// Persist pending session metadata and add this session to the resume index.
    pub async fn ensure_session_indexed(&mut self) -> Result<(), SessionError> {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.clone(),
            self.config.session_manager.clone(),
        ) else {
            return Ok(());
        };
        if manager.get_session(&session_id).ok().flatten().is_none() {
            let _ = manager.create_session(
                &self.config.cwd,
                &self.model(),
                Some(&self.config.provider_name),
                None,
                Some(&session_id),
            );
        }
        self.config.index_on_first_persist = false;
        self.write_pending_initial_entries().await?;
        Ok(())
    }

    // ---- persistence internals -------------------------------------------

    fn diagnostic_context(&self) -> AgentCallDiagnosticContext {
        AgentCallDiagnosticContext {
            provider_name: self.config.provider_name.clone(),
            model: self.model(),
            cwd: self.config.cwd.clone(),
            session_id: self.config.session_id.clone(),
            run_id: new_agent_call_run_id(),
        }
    }

    async fn persist_loaded_interrupted_tool_repairs(&mut self) -> Result<(), SessionError> {
        let Some((parent_id, suffix)) = interrupted_tool_repair_plan(
            &self.state.messages,
            &self.state.context_entry_ids,
            &self.config,
        ) else {
            return Ok(());
        };
        let mut parent = Some(parent_id);
        for message in suffix {
            let mut entry = MessageEntry::new(message);
            entry.id = self.config.id_gen.new_id();
            entry.timestamp = self.config.clock.now_secs();
            entry.parent_id = parent.clone();
            let id = entry.id.clone();
            self.append_session_entry(SessionEntry::Message(entry))
                .await?;
            parent = Some(id);
        }
        self.append_leaf(parent.clone()).await?;
        self.last_parent_id = parent.clone();
        self.refresh_persisted_state(parent.as_deref()).await?;
        self.harness =
            AgentHarness::new(self.harness.config().clone(), self.state.messages.clone());
        Ok(())
    }

    /// Persist completed harness messages after `persisted_count`, returning the
    /// new persisted count.
    ///
    /// A storage failure is **propagated**, never swallowed: tau's
    /// `_persist_messages_since` lets an append raise, aborting the turn, and the
    /// callers here do the same (they stop the stream rather than continuing with
    /// a stale count, which would re-append the already-durable message with a
    /// fresh id and corrupt the transcript — the failure Codex flagged). Each
    /// message counts as persisted the moment its `MessageEntry` is durably
    /// appended, so a later leaf-append failure cannot cause a re-append either.
    async fn persist_messages_since(
        &mut self,
        persisted_count: usize,
    ) -> Result<usize, SessionError> {
        let messages = self.harness.messages();
        if persisted_count >= messages.len() {
            return Ok(persisted_count);
        }
        let new_messages = messages[persisted_count..].to_vec();
        let total = messages.len();
        for message in new_messages {
            let mut entry = MessageEntry::new(message);
            entry.id = self.config.id_gen.new_id();
            entry.timestamp = self.config.clock.now_secs();
            entry.parent_id = self.last_parent_id.clone();
            let id = entry.id.clone();
            self.append_session_entry(SessionEntry::Message(entry))
                .await?;
            self.last_parent_id = Some(id.clone());
            self.append_leaf(Some(id)).await?;
        }
        let leaf = self.last_parent_id.clone();
        self.refresh_persisted_state(leaf.as_deref()).await?;
        self.invalidate_context_usage_cache();
        Ok(total)
    }

    /// Persist and, on a storage failure, log it, **record it on the session**,
    /// and return `None` so the caller (a turn stream) aborts without emitting
    /// `agent_settled`. This mirrors tau's `prompt()`, which logs and then
    /// *re-raises* — the error reaches the caller (a non-zero CLI exit); it is
    /// not swallowed. The print-mode caller reads it via [`Self::take_run_error`].
    async fn persist_or_log(
        &mut self,
        persisted_count: usize,
        context: &AgentCallDiagnosticContext,
        phase: &str,
    ) -> Option<usize> {
        match self.persist_messages_since(persisted_count).await {
            Ok(count) => Some(count),
            Err(err) => {
                let message = err.to_string();
                let path =
                    self.diagnostic_logger
                        .log_exception(context, phase, "StorageError", &message);
                self.last_diagnostic_log_path = Some(path);
                self.run_error = Some(message);
                None
            }
        }
    }

    /// Take (and clear) the error from the most recent turn, if it aborted on a
    /// persistence failure. The print-mode CLI uses this to exit non-zero, the
    /// way tau's re-raised exception fails a non-interactive run.
    pub fn take_run_error(&mut self) -> Option<String> {
        self.run_error.take()
    }

    fn invalidate_context_usage_cache(&mut self) {
        self.context_usage_cache = None;
    }

    async fn refresh_persisted_state(&mut self, leaf_id: Option<&str>) -> Result<(), SessionError> {
        let entries = self.read_session_entries().await?;
        self.state = state_at(&entries, leaf_id)?;
        if let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) {
            let _ = manager.touch_session(
                session_id,
                Some(&self.harness.config().model),
                Some(&self.config.provider_name),
                None,
            );
        }
        Ok(())
    }

    async fn read_session_entries(&self) -> Result<Vec<SessionEntry>, SessionError> {
        Ok(detach_missing_parents(
            self.config.storage.read_all().await?,
        ))
    }

    async fn append_leaf(&mut self, entry_id: Option<String>) -> Result<(), SessionError> {
        let mut leaf = LeafEntry::new(entry_id.clone());
        leaf.id = self.config.id_gen.new_id();
        leaf.timestamp = self.config.clock.now_secs();
        leaf.parent_id = entry_id;
        self.append_session_entry(SessionEntry::Leaf(leaf)).await
    }

    async fn append_session_entry(&mut self, entry: SessionEntry) -> Result<(), SessionError> {
        self.ensure_session_initialized().await?;
        self.config.storage.append(&entry).await?;
        Ok(())
    }

    async fn ensure_session_initialized(&mut self) -> Result<(), SessionError> {
        if self.pending_initial_entries.is_empty() {
            return Ok(());
        }
        self.write_pending_initial_entries().await?;
        if self.config.index_on_first_persist {
            self.index_current_session();
        }
        Ok(())
    }

    async fn write_pending_initial_entries(&mut self) -> Result<(), SessionError> {
        let pending = std::mem::take(&mut self.pending_initial_entries);
        for entry in pending {
            self.config.storage.append(&entry).await?;
        }
        Ok(())
    }

    fn index_current_session(&self) {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) else {
            return;
        };
        if manager.get_session(session_id).ok().flatten().is_some() {
            return;
        }
        let _ = manager.create_session(
            &self.config.cwd,
            &self.harness.config().model,
            Some(&self.config.provider_name),
            None,
            Some(session_id),
        );
    }

    async fn try_auto_name_session(
        &mut self,
        first_message: &str,
        context: &AgentCallDiagnosticContext,
    ) {
        if !self.should_auto_name_session() {
            return;
        }
        let title = match self.generate_session_name(first_message).await {
            Ok(title) => title,
            Err(msg) => {
                let path = self.diagnostic_logger.log_exception(
                    context,
                    "auto_name_session",
                    "RuntimeError",
                    &msg,
                );
                self.last_diagnostic_log_path = Some(path);
                fallback_session_name(first_message)
            }
        };
        let title = title.or_else(|| fallback_session_name(first_message));
        if let Some(title) = title {
            self.set_auto_session_title(&title);
        }
    }

    fn should_auto_name_session(&self) -> bool {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) else {
            return false;
        };
        if let Some(record) = manager.get_session(session_id).ok().flatten() {
            if record.title.as_ref().is_some_and(|t| !t.is_empty()) {
                return false;
            }
        }
        self.harness
            .messages()
            .iter()
            .filter(|m| matches!(m, AgentMessage::User(_)))
            .count()
            == 1
    }

    async fn generate_session_name(&self, first_message: &str) -> Result<Option<String>, String> {
        let prompt = format!(
            "Create a concise session name for this first user message. Use at most four words.\n\nUser message:\n{first_message}"
        );
        let mut user = UserMessage::new(prompt);
        user.timestamp = self.config.clock.now_ms();
        let messages = vec![AgentMessage::User(user)];
        let (text_parts, final_text, error) =
            drive_text_stream(self, SESSION_NAME_SYSTEM_PROMPT, &messages).await;
        if let Some(err) = error {
            return Err(format!("Session naming failed: {err}"));
        }
        Ok(sanitize_session_name(&final_text.unwrap_or(text_parts)))
    }

    fn set_auto_session_title(&self, title: &str) {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.as_ref(),
            self.config.session_manager.as_ref(),
        ) else {
            return;
        };
        if let Some(record) = manager.get_session(session_id).ok().flatten() {
            if record.title.as_ref().is_some_and(|t| !t.is_empty()) {
                return;
            }
        }
        let _ = manager.touch_session(
            session_id,
            Some(&self.harness.config().model),
            Some(&self.config.provider_name),
            Some(title),
        );
    }
}

/// Drive a one-shot provider stream, collecting text deltas / the final text /
/// an error message. Shared by session-naming and compaction summarization.
async fn drive_text_stream(
    session: &CodingSession,
    system: &str,
    messages: &[AgentMessage],
) -> (String, Option<String>, Option<String>) {
    let mut text_parts = String::new();
    let mut final_text: Option<String> = None;
    let mut stream =
        session
            .config
            .provider
            .stream_response(&session.model(), system, messages, &[], None);
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextDelta(d) => text_parts.push_str(&d.delta),
            AssistantMessageEvent::Done(d) => final_text = Some(d.message.text()),
            AssistantMessageEvent::Error(e) => {
                let msg = e
                    .error
                    .error_message
                    .clone()
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| format!("{:?}", e.reason));
                return (text_parts, final_text, Some(msg));
            }
            _ => {}
        }
    }
    (text_parts, final_text, None)
}

// ---- module-level helpers -------------------------------------------------

/// Convenience factory for local JSONL coding-session storage.
#[must_use]
pub fn jsonl_session_storage(path: impl AsRef<Path>) -> Arc<dyn SessionStorage> {
    Arc::new(JsonlSessionStorage::new(path))
}

/// Return rho's default user-home session path for a project cwd.
#[must_use]
pub fn default_session_path(cwd: &Path) -> PathBuf {
    RhoPaths::default().default_session_path(cwd)
}

/// Parse input-bar terminal command syntax (`!cmd` / `!!cmd`).
#[must_use]
pub fn parse_terminal_command(text: &str) -> Option<TerminalCommandRequest> {
    let stripped = text.trim();
    if let Some(rest) = stripped.strip_prefix("!!") {
        let command = rest.trim();
        if command.is_empty() {
            return None;
        }
        return Some(TerminalCommandRequest {
            command: command.to_string(),
            add_to_context: false,
        });
    }
    if let Some(rest) = stripped.strip_prefix('!') {
        let command = rest.trim();
        if command.is_empty() {
            return None;
        }
        return Some(TerminalCommandRequest {
            command: command.to_string(),
            add_to_context: true,
        });
    }
    None
}

fn terminal_command_context_message(command: &str, output: &str) -> String {
    format!(
        "Terminal command executed by the user.\n\nCommand:\n```bash\n{command}\n```\n\nOutput:\n```text\n{output}\n```"
    )
}

fn diagnostics_log_path(resource_paths: &RhoResourcePaths) -> PathBuf {
    resource_paths.paths.as_ref().map_or_else(
        || resource_paths.root.join("logs").join("agent-calls.jsonl"),
        RhoPaths::agent_calls_log_path,
    )
}

fn detach_missing_parents(entries: Vec<SessionEntry>) -> Vec<SessionEntry> {
    let ids: std::collections::HashSet<String> =
        entries.iter().map(|e| e.id().to_string()).collect();
    entries
        .into_iter()
        .map(|mut entry| {
            let detach = entry.parent_id().is_some_and(|p| !ids.contains(p));
            if detach {
                set_parent_id(&mut entry, None);
            }
            entry
        })
        .collect()
}

fn last_parent_id_from_state(state: &SessionState) -> Option<String> {
    if let Some(id) = &state.active_leaf_id {
        return Some(id.clone());
    }
    state.entries.last().map(|e| e.id().to_string())
}

fn latest_leaf_entry(entries: &[SessionEntry]) -> Option<Option<String>> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Leaf(leaf) = entry {
            return Some(leaf.entry_id.clone());
        }
    }
    None
}

/// Replay the root-to-leaf path for an explicitly-provided leaf id. `leaf_id`
/// is always *provided* here (this is tau's `from_entries(entries, leaf_id=…)`,
/// never the unset/linear form), so `None` means the empty pre-root context —
/// distinct from a linear replay. Load's no-leaf case calls `from_entries`
/// directly.
fn state_at(entries: &[SessionEntry], leaf_id: Option<&str>) -> Result<SessionState, SessionError> {
    Ok(SessionState::from_entries_at_leaf(entries, leaf_id)?)
}

fn state_thinking_level(state: &SessionState, default: &str) -> String {
    match &state.thinking_level {
        Some(level) => {
            normalize_thinking_level(Some(level)).unwrap_or_else(|_| default.to_string())
        }
        None => default.to_string(),
    }
}

fn runtime_model_for_state(config: &CodingSessionConfig, state: &SessionState) -> String {
    let state_model = state.model.clone().unwrap_or_else(|| config.model.clone());
    if config.provider_settings.is_none() || config.runtime_provider_config.is_none() {
        return state_model;
    }
    let Some(provider) = provider_config_for_name(config, &config.provider_name) else {
        return state_model;
    };
    match validate_provider_model(&provider, &state_model) {
        Ok(()) => state_model,
        Err(_) => {
            if provider.models().contains(&config.model) {
                config.model.clone()
            } else {
                provider.default_model().to_string()
            }
        }
    }
}

/// tau `_initial_model_for_config`: the model recorded in a fresh session's
/// `ModelChangeEntry`, validated against the active provider.
fn initial_model_for_config(config: &CodingSessionConfig) -> String {
    if config.provider_settings.is_none() || config.runtime_provider_config.is_none() {
        return config.model.clone();
    }
    let Some(provider) = provider_config_for_name(config, &config.provider_name) else {
        return config.model.clone();
    };
    match validate_provider_model(&provider, &config.model) {
        Ok(()) => config.model.clone(),
        Err(_) => provider.default_model().to_string(),
    }
}

/// tau `_initial_thinking_level_for_config`: the thinking level recorded in a
/// fresh session, coerced to the active provider's preferred level.
fn initial_thinking_level_for_config(config: &CodingSessionConfig, model: &str) -> String {
    match provider_config_for_name(config, &config.provider_name) {
        Some(provider) => {
            preferred_thinking_level_for_model(&provider, model, &config.thinking_level)
        }
        None => config.thinking_level.clone(),
    }
}

/// tau `_provider_config_for_name`: the named provider from settings, else the
/// runtime provider config.
fn provider_config_for_name(
    config: &CodingSessionConfig,
    provider_name: &str,
) -> Option<ProviderConfig> {
    if let Some(settings) = config.provider_settings.as_ref() {
        if let Ok(provider) = settings.get_provider(Some(provider_name)) {
            return Some(provider.clone());
        }
    }
    config.runtime_provider_config.clone()
}

/// tau `_active_provider_config`: the named provider from settings only (no
/// runtime fallback).
fn active_provider_config_from(
    settings: Option<&ProviderSettings>,
    provider_name: &str,
) -> Option<ProviderConfig> {
    settings?.get_provider(Some(provider_name)).ok().cloned()
}

/// tau `_preferred_thinking_level_for_model`.
fn preferred_thinking_level_for_model(
    provider: &ProviderConfig,
    model: &str,
    fallback: &str,
) -> String {
    let levels = provider_thinking_levels(provider, Some(model));
    if let Some(preferred) = provider.thinking_defaults().get(model) {
        if levels.iter().any(|level| level == preferred) {
            return preferred.clone();
        }
    }
    if levels.iter().any(|level| level == fallback) || levels.is_empty() {
        return fallback.to_string();
    }
    provider_default_thinking_level(provider, Some(model)).unwrap_or_else(|| levels[0].clone())
}

/// tau `_coerced_thinking_level`.
fn coerced_thinking_level(
    provider: &ProviderConfig,
    model: &str,
    current: &str,
    preferred: Option<&str>,
) -> String {
    let levels = provider_thinking_levels(provider, Some(model));
    if levels.is_empty() || levels.iter().any(|level| level == current) {
        return current.to_string();
    }
    if let Some(preferred) = preferred {
        if levels.iter().any(|level| level == preferred) {
            return preferred.to_string();
        }
    }
    provider_default_thinking_level(provider, Some(model)).unwrap_or_else(|| levels[0].clone())
}

/// tau `_unavailable_thinking_message`.
fn unavailable_thinking_message(session: &CodingSession) -> String {
    let message = format!(
        "Thinking controls are unavailable for {}:{}",
        session.provider_name(),
        session.model()
    );
    match session.thinking_unavailable_reason() {
        Some(reason) => format!("{message}: {reason}"),
        None => message,
    }
}

/// tau `_resolve_export_destination`.
fn resolve_export_destination(
    destination: Option<&Path>,
    cwd: &Path,
    session_path: Option<&Path>,
    format: &str,
) -> PathBuf {
    let Some(destination) = destination else {
        return match session_path {
            Some(path) => default_session_export_artifact_path(path, cwd, format),
            None => cwd.join(format!("tau-session.{format}")),
        };
    };
    let resolved = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        cwd.join(destination)
    };
    if resolved.extension().is_some() {
        return resolved;
    }
    let name = session_path.and_then(Path::file_stem).map_or_else(
        || "tau-session".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    default_session_export_artifact_path(Path::new(&name), &resolved, format)
}

fn is_branchable_tree_entry(entry: &SessionEntry) -> bool {
    match entry {
        SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_) => true,
        SessionEntry::Message(m) => {
            matches!(
                m.message,
                AgentMessage::User(_) | AgentMessage::Assistant(_)
            )
        }
        _ => false,
    }
}

fn is_tool_call_tree_entry(entry: &SessionEntry) -> bool {
    matches!(entry, SessionEntry::Message(m)
        if matches!(&m.message, AgentMessage::Assistant(a) if !a.tool_calls().is_empty()))
}

fn tree_choice_label(entry: &SessionEntry, branch_indent: usize) -> String {
    let prefix = "  ".repeat(branch_indent);
    format!("{prefix}{}", tree_entry_title(entry))
}

fn tree_entry_title(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message(m) => {
            if let AgentMessage::Assistant(a) = &m.message {
                if !a.tool_calls().is_empty() && a.content.is_empty() {
                    let names: Vec<String> =
                        a.tool_calls().iter().map(|c| c.name.clone()).collect();
                    return format!("tool call: {}", names.join(", "));
                }
            }
            format!(
                "{}: {}",
                m.message.role(),
                short_preview(&m.message.text(), 72)
            )
        }
        SessionEntry::Compaction(c) => {
            format!("compaction summary: {}", short_preview(&c.summary, 72))
        }
        SessionEntry::BranchSummary(b) => {
            format!("branch summary: {}", short_preview(&b.summary, 72))
        }
        SessionEntry::ModelChange(_) => "model_change".to_string(),
        SessionEntry::ThinkingLevelChange(_) => "thinking_level_change".to_string(),
        SessionEntry::Label(_) => "label".to_string(),
        SessionEntry::Leaf(_) => "leaf".to_string(),
        SessionEntry::SessionInfo(_) => "session_info".to_string(),
        SessionEntry::Custom(_) => "custom".to_string(),
    }
}

fn short_preview(text: &str, limit: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= limit {
        return if normalized.is_empty() {
            "(empty)".to_string()
        } else {
            normalized
        };
    }
    let head: String = chars[..limit - 1].iter().collect();
    format!("{head}...")
}

fn tree_branch_indents(entries: &[SessionEntry]) -> std::collections::HashMap<String, usize> {
    use std::collections::HashMap;
    let mut children_by_parent: HashMap<Option<String>, Vec<String>> = HashMap::new();
    for entry in entries {
        if matches!(entry, SessionEntry::Leaf(_)) {
            continue;
        }
        children_by_parent
            .entry(entry.parent_id().map(str::to_string))
            .or_default()
            .push(entry.id().to_string());
    }
    let mut sibling_index: HashMap<String, usize> = HashMap::new();
    for children in children_by_parent.values() {
        for (index, child_id) in children.iter().enumerate() {
            sibling_index.insert(child_id.clone(), index);
        }
    }
    let mut indents: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        if matches!(entry, SessionEntry::Leaf(_)) {
            continue;
        }
        let parent_indent = entry
            .parent_id()
            .and_then(|p| indents.get(p).copied())
            .unwrap_or(0);
        let sib = sibling_index.get(entry.id()).copied().unwrap_or(0);
        indents.insert(entry.id().to_string(), parent_indent + usize::from(sib > 0));
    }
    indents
}

fn ordered_tree_entries(entries: &[SessionEntry]) -> Vec<SessionEntry> {
    use std::collections::{HashMap, HashSet};
    let mut children_by_parent: HashMap<Option<String>, Vec<SessionEntry>> = HashMap::new();
    for entry in entries {
        if matches!(entry, SessionEntry::Leaf(_)) {
            continue;
        }
        children_by_parent
            .entry(entry.parent_id().map(str::to_string))
            .or_default()
            .push(entry.clone());
    }
    let mut ordered: Vec<SessionEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut expanded: HashSet<Option<String>> = HashSet::new();

    append_descendants(
        None,
        &children_by_parent,
        &mut ordered,
        &mut seen,
        &mut expanded,
    );
    for entry in entries {
        if !matches!(entry, SessionEntry::Leaf(_)) && !seen.contains(entry.id()) {
            ordered.push(entry.clone());
            seen.insert(entry.id().to_string());
            append_descendants(
                Some(entry.id().to_string()),
                &children_by_parent,
                &mut ordered,
                &mut seen,
                &mut expanded,
            );
        }
    }
    ordered
}

fn append_descendants(
    root: Option<String>,
    children_by_parent: &std::collections::HashMap<Option<String>, Vec<SessionEntry>>,
    ordered: &mut Vec<SessionEntry>,
    seen: &mut std::collections::HashSet<String>,
    expanded: &mut std::collections::HashSet<Option<String>>,
) {
    let mut stack: Vec<Option<String>> = vec![root];
    while let Some(parent_id) = stack.pop() {
        if expanded.contains(&parent_id) {
            continue;
        }
        expanded.insert(parent_id.clone());
        let children = children_by_parent
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();
        for child in &children {
            if seen.insert(child.id().to_string()) {
                ordered.push(child.clone());
            }
        }
        for child in children.iter().rev() {
            stack.push(Some(child.id().to_string()));
        }
    }
}

fn messages_after_entry_on_active_path(
    entries: &[SessionEntry],
    entry_id: &str,
    active_leaf_id: Option<&str>,
) -> Vec<AgentMessage> {
    let Some(active_leaf_id) = active_leaf_id else {
        return Vec::new();
    };
    let Ok(active_path) = path_to_entry(entries, active_leaf_id) else {
        return Vec::new();
    };
    let Some(target_index) = active_path.iter().position(|e| e.id() == entry_id) else {
        return Vec::new();
    };
    active_path[target_index + 1..]
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => Some(m.message.clone()),
            _ => None,
        })
        .collect()
}

fn first_recent_context_index(rows: &[(String, AgentMessage)], keep_recent_tokens: i64) -> i64 {
    if keep_recent_tokens <= 0 {
        return rows.len() as i64;
    }
    let mut accumulated = 0i64;
    let mut candidate: Option<usize> = None;
    for index in (0..rows.len()).rev() {
        accumulated += estimate_message_tokens(&rows[index].1);
        if accumulated >= keep_recent_tokens {
            candidate = Some(index);
            break;
        }
    }
    let Some(candidate_index) = candidate else {
        return 0;
    };
    let candidate_role = rows[candidate_index].1.role();
    if candidate_role == "user" {
        if candidate_index > 0 {
            return candidate_index as i64;
        }
        return next_user_message_index(rows, 1).map_or(0, |i| i as i64);
    }
    if let Some(next) = next_user_message_index(rows, candidate_index + 1) {
        return next as i64;
    }
    for (index, row) in rows.iter().enumerate().skip(candidate_index) {
        if row.1.role() != "toolResult" {
            return index as i64;
        }
    }
    rows.len() as i64
}

fn next_user_message_index(rows: &[(String, AgentMessage)], start: usize) -> Option<usize> {
    (start..rows.len()).find(|&index| rows[index].1.role() == "user")
}

fn is_context_overflow_error(message: &AssistantMessage) -> bool {
    let text = message
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    const MARKERS: [&str; 12] = [
        "context length",
        "context window",
        "context limit",
        "maximum context",
        "max context",
        "input is too long",
        "input length",
        "prompt is too long",
        "too many tokens",
        "token limit",
        "exceeds the limit",
        "exceeded the limit",
    ];
    MARKERS.iter().any(|m| text.contains(m))
}

/// Python `string.punctuation`.
const PY_PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

fn sanitize_session_name(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let quote_set: &[char] = &[
        '"', '\'', '`', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}',
    ];
    let cleaned = collapsed.trim_matches(quote_set);
    let punct: Vec<char> = PY_PUNCTUATION.chars().chain(std::iter::once(' ')).collect();
    let cleaned = cleaned.trim_matches(|c| punct.contains(&c));
    let strip: Vec<char> = PY_PUNCTUATION
        .chars()
        .chain(quote_set.iter().copied())
        .collect();
    let words: Vec<String> = cleaned
        .split_whitespace()
        .map(|word| word.trim_matches(|c| strip.contains(&c)).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(words.into_iter().take(4).collect::<Vec<_>>().join(" "))
}

fn fallback_session_name(first_message: &str) -> Option<String> {
    sanitize_session_name(first_message)
}

fn interrupted_tool_repair_plan(
    messages: &[AgentMessage],
    context_entry_ids: &[String],
    config: &CodingSessionConfig,
) -> Option<(String, Vec<AgentMessage>)> {
    let mut returned_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let mut repaired: Vec<AgentMessage> = Vec::new();
    for message in messages {
        repaired.push(message.clone());
        if let AgentMessage::Assistant(a) = message {
            for call in a.tool_calls() {
                if returned_ids.contains(&call.id) {
                    continue;
                }
                returned_ids.insert(call.id.clone());
                let mut result = ToolResultMessage::new(
                    call.id.clone(),
                    call.name.clone(),
                    vec![ToolResultContent::Text(TextContent::new(
                        "Tool call interrupted by user",
                    ))],
                );
                result.is_error = true;
                result.timestamp = config.clock.now_ms();
                repaired.push(AgentMessage::ToolResult(result));
            }
        }
    }
    if repaired == messages {
        return None;
    }
    let mut common_prefix = 0usize;
    for (old, new) in messages.iter().zip(repaired.iter()) {
        if old != new {
            break;
        }
        common_prefix += 1;
    }
    if common_prefix == 0 {
        return None;
    }
    Some((
        context_entry_ids[common_prefix - 1].clone(),
        repaired[common_prefix..].to_vec(),
    ))
}

/// The slash-command registry's view of a coding session (tau `CommandSession`).
///
/// `model`/`system_prompt` borrow the harness config directly (the inherent
/// accessors return owned `String`s). `context_token_estimate` and
/// `context_usage_breakdown` recompute the estimate on `&self` rather than using
/// the `&mut self` cache. `set_model` is only reached after `_model_command`
/// validates the model against `available_models`, so the (rare) provider-refresh
/// error is dropped here; tau would propagate it. `ensure_session_indexed` does
/// the synchronous index-record creation only — the async pending-entry flush is
/// deferred to the next durable write (tau flushes it eagerly).
impl CommandSession for CodingSession {
    fn cwd(&self) -> &Path {
        &self.config.cwd
    }

    fn model(&self) -> &str {
        self.harness.config().model.as_str()
    }

    fn provider_name(&self) -> &str {
        &self.config.provider_name
    }

    fn available_models(&self) -> Vec<String> {
        CodingSession::available_models(self)
    }

    fn available_providers(&self) -> Vec<String> {
        CodingSession::available_providers(self)
    }

    fn tools_len(&self) -> usize {
        self.harness.config().tools.len()
    }

    fn skills(&self) -> &[Skill] {
        &self.skills
    }

    fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.prompt_templates
    }

    fn context_files(&self) -> &[ProjectContextFile] {
        &self.context_files
    }

    fn context_token_estimate(&self) -> i64 {
        self.context_usage_estimate().total_tokens
    }

    fn auto_compact_token_threshold(&self) -> Option<i64> {
        CodingSession::auto_compact_token_threshold(self)
    }

    fn context_window_tokens(&self) -> i64 {
        CodingSession::context_window_tokens(self)
    }

    fn context_window_source(&self) -> &'static str {
        CodingSession::context_window_source(self)
    }

    fn model_limits_discovery_error(&self) -> Option<&str> {
        CodingSession::model_limits_discovery_error(self)
    }

    fn thinking_level(&self) -> &str {
        &self.thinking_level
    }

    fn available_thinking_levels(&self) -> Vec<String> {
        CodingSession::available_thinking_levels(self)
    }

    fn resource_diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.resource_diagnostics
    }

    fn system_prompt(&self) -> &str {
        self.harness.config().system.as_str()
    }

    fn session_id(&self) -> Option<&str> {
        self.config.session_id.as_deref()
    }

    fn session_title(&self) -> Option<String> {
        // Mirror tau `CodingSession.session_title`: the human-friendly title
        // lives in the manager's index record, looked up live by session id.
        let session_id = self.config.session_id.as_deref()?;
        let manager = self.config.session_manager.as_ref()?;
        // Best-effort like tau's try/except-wrapped internal reads: a corrupt
        // index shows no title rather than aborting `/session` rendering.
        manager.get_session(session_id).ok().flatten()?.title
    }

    fn session_manager(&self) -> Option<&SessionManager> {
        self.config.session_manager.as_ref()
    }

    fn thinking_unavailable_reason(&self) -> Option<String> {
        CodingSession::thinking_unavailable_reason(self)
    }

    fn context_usage_breakdown(&self) -> Option<(i64, i64, i64)> {
        let usage = self.context_usage_estimate();
        Some((usage.system_tokens, usage.message_tokens, usage.tool_tokens))
    }

    fn set_model(&mut self, model: &str) {
        let _ = CodingSession::set_model(self, model);
    }

    fn reload_provider_settings(&mut self) -> Result<(), String> {
        CodingSession::reload_provider_settings(self).map_err(|err| err.to_string())
    }

    fn ensure_session_indexed(&mut self) {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.clone(),
            self.config.session_manager.clone(),
        ) else {
            return;
        };
        if manager.get_session(&session_id).ok().flatten().is_none() {
            let _ = manager.create_session(
                &self.config.cwd,
                &CodingSession::model(self),
                Some(&self.config.provider_name),
                None,
                Some(&session_id),
            );
        }
    }
}

impl CodingSession {
    /// Recompute the context-usage estimate on `&self` (the `&mut self`
    /// accessor caches; the command view needs a shared borrow).
    fn context_usage_estimate(&self) -> ContextUsageEstimate {
        if let Some(cached) = self.context_usage_cache {
            return cached;
        }
        estimate_context_usage(
            &self.harness.config().system,
            &self.harness.messages(),
            &self.harness.config().tools,
        )
    }
}

fn load_session_resources(
    resource_paths: &RhoResourcePaths,
    explicit_context_files: &[ProjectContextFile],
    skills_enabled: bool,
) -> SessionResources {
    // tau `_load_session_resources`: skill loading is gated on `skills_enabled`;
    // diagnostics are concatenated in skill → prompt → context order.
    let (skills, skill_diagnostics) = if skills_enabled {
        load_skills_with_diagnostics(Some(resource_paths))
    } else {
        (Vec::new(), Vec::new())
    };
    let (prompt_templates, prompt_diagnostics) =
        load_prompt_templates_with_diagnostics(Some(resource_paths));
    let (discovered, context_diagnostics) =
        discover_project_context_with_diagnostics(Some(resource_paths.clone()));
    let mut diagnostics = skill_diagnostics;
    diagnostics.extend(prompt_diagnostics);
    diagnostics.extend(context_diagnostics);
    SessionResources {
        skills,
        prompt_templates,
        context_files: merge_context_files(explicit_context_files, &discovered),
        diagnostics,
    }
}

fn merge_context_files(
    explicit: &[ProjectContextFile],
    discovered: &[ProjectContextFile],
) -> Vec<ProjectContextFile> {
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for file in explicit.iter().chain(discovered.iter()) {
        if seen.insert(file.path.clone()) {
            merged.push(file.clone());
        }
    }
    merged
}

// ---- reload signatures ----------------------------------------------------

/// Resource-set signature type. Two reloads with equal signatures produce the
/// same rendered resources (tau compares these tuples for `changed`).
type ResourceSignature = (String, String, Option<String>, String);

/// Diagnostic signature: `(kind, message, path, name, severity)`.
type DiagnosticSignature = (String, String, Option<String>, Option<String>, String);

fn skill_signatures(skills: &[Skill]) -> Vec<ResourceSignature> {
    skills
        .iter()
        .map(|skill| {
            (
                skill.name.clone(),
                skill.path.to_string_lossy().into_owned(),
                skill.description.clone(),
                skill.content.clone(),
            )
        })
        .collect()
}

fn prompt_template_signatures(templates: &[PromptTemplate]) -> Vec<ResourceSignature> {
    templates
        .iter()
        .map(|template| {
            (
                template.name.clone(),
                template.path.to_string_lossy().into_owned(),
                template.description.clone(),
                template.content.clone(),
            )
        })
        .collect()
}

fn context_file_signatures(context_files: &[ProjectContextFile]) -> Vec<(String, String)> {
    context_files
        .iter()
        .map(|file| (file.path.clone(), file.content.clone()))
        .collect()
}

fn diagnostic_signatures(diagnostics: &[ResourceDiagnostic]) -> Vec<DiagnosticSignature> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.kind.clone(),
                diagnostic.message.clone(),
                diagnostic
                    .path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                diagnostic.name.clone(),
                diagnostic.severity.clone(),
            )
        })
        .collect()
}

/// tau `_system_prompt_resource_signatures`, minus the extension tool/guideline
/// terms rho does not have: the skill index (sorted by name, without content)
/// and the context-file signatures that drive the next-turn system prompt.
#[allow(clippy::type_complexity)]
fn system_prompt_resource_signatures(
    skills: &[Skill],
    context_files: &[ProjectContextFile],
) -> (Vec<(String, String, Option<String>)>, Vec<(String, String)>) {
    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let prompt_skills = sorted
        .iter()
        .map(|skill| {
            (
                skill.name.clone(),
                skill.path.to_string_lossy().into_owned(),
                skill.description.clone(),
            )
        })
        .collect();
    (prompt_skills, context_file_signatures(context_files))
}

fn category_summary_from<T: PartialEq>(before: &[T], after: &[T]) -> ReloadCategorySummary {
    ReloadCategorySummary::new(before.len(), after.len(), before != after)
}

fn set_parent_id(entry: &mut SessionEntry, parent: Option<String>) {
    match entry {
        SessionEntry::Message(e) => e.parent_id = parent,
        SessionEntry::ModelChange(e) => e.parent_id = parent,
        SessionEntry::ThinkingLevelChange(e) => e.parent_id = parent,
        SessionEntry::Compaction(e) => e.parent_id = parent,
        SessionEntry::BranchSummary(e) => e.parent_id = parent,
        SessionEntry::Label(e) => e.parent_id = parent,
        SessionEntry::Leaf(e) => e.parent_id = parent,
        SessionEntry::SessionInfo(e) => e.parent_id = parent,
        SessionEntry::Custom(e) => e.parent_id = parent,
    }
}
