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
use std::sync::Arc;

use futures::StreamExt;

use rho_agent::clock::{Clock, IdGen, system_clock, uuid_id_gen};
use rho_agent::events::AgentEvent;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig, QueuedMessages};
use rho_agent::messages::{
    AgentMessage, AssistantMessage, StopReason, TextContent, ToolResultContent, ToolResultMessage,
    UserMessage,
};
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
use crate::context::discover_project_context_with_diagnostics;
use crate::context_window::{
    ContextUsageEstimate, DEFAULT_COMPACTION_KEEP_RECENT_TOKENS, DEFAULT_CONTEXT_WINDOW_TOKENS,
    SUMMARIZATION_SYSTEM_PROMPT, auto_compaction_threshold_for_context_window,
    build_compaction_summary_prompt, estimate_context_usage, estimate_message_tokens,
    summarize_messages_for_compaction,
};
use crate::diagnostics::{
    AgentCallDiagnosticContext, AgentCallDiagnosticLogger, new_agent_call_run_id,
};
use crate::events::{
    AgentSettledEvent, AutoRetryEndEvent, AutoRetryStartEvent, CodingSessionEvent,
    CompactionEndEvent, CompactionReason, CompactionStartEvent, QueueUpdateEvent,
    SessionAgentEndEvent,
};
use crate::paths::RhoPaths;
use crate::resources::{ResourceDiagnostic, RhoResourcePaths, resource_paths_with_cwd};
use crate::session_manager::SessionManager;
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
            auto_compact_token_threshold: None,
            auto_compact_enabled: true,
            thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
            index_on_first_persist: false,
            shell_command_prefix: None,
            skills_enabled: true,
            clock: system_clock(),
            id_gen: uuid_id_gen(),
        }
    }
}

/// Tau-owned resources loaded around a coding session (dispatch-1 subset:
/// context files + diagnostics; skills/prompt templates are dispatch-2).
struct SessionResources {
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
    resource_diagnostics: Vec<ResourceDiagnostic>,
    thinking_level: String,
    auto_compact_token_threshold: Option<i64>,
    auto_compact_enabled: bool,
    context_usage_cache: Option<ContextUsageEstimate>,
    diagnostic_logger: AgentCallDiagnosticLogger,
    last_diagnostic_log_path: Option<PathBuf>,
}

impl CodingSession {
    /// Load a coding session from append-only storage.
    pub async fn load(config: CodingSessionConfig) -> Result<Self, SessionError> {
        let mut entries = config.storage.read_all().await?;
        let mut pending_initial_entries: Vec<SessionEntry> = Vec::new();

        if entries.is_empty() {
            let mut info = SessionInfoEntry::new();
            info.id = config.id_gen.new_id();
            info.timestamp = config.clock.now_secs();
            info.created_at = config.clock.now_secs();
            info.cwd = Some(config.cwd.to_string_lossy().to_string());

            let mut model = ModelChangeEntry::new(config.model.clone());
            model.id = config.id_gen.new_id();
            model.timestamp = config.clock.now_secs();
            model.parent_id = Some(info.id.clone());

            let mut thinking = ThinkingLevelChangeEntry::new(Some(config.thinking_level.clone()));
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
        let resources = load_session_resources(&resource_paths, &config.context_files);

        let base_tools = config.tools.clone().unwrap_or_else(|| {
            create_coding_tools(&config.cwd, config.shell_command_prefix.as_deref())
        });
        let system = config.system.clone().unwrap_or_else(|| {
            build_system_prompt(&BuildSystemPromptOptions {
                cwd: config.cwd.clone(),
                tools: base_tools.clone(),
                custom_prompt: config.custom_system_prompt.clone(),
                append_system_prompt: config.append_system_prompt.clone(),
                context_files: resources.context_files.clone(),
                ..Default::default()
            })
        });

        let runtime_model = runtime_model_for_state(&config, &state);
        let harness = AgentHarness::new(
            AgentHarnessConfig::new(config.provider.clone(), runtime_model, system)
                .with_tools(base_tools)
                .with_clock(config.clock.clone()),
            state.messages.clone(),
        );

        let thinking_level = state_thinking_level(&state, &config.thinking_level);
        let diagnostic_logger =
            AgentCallDiagnosticLogger::new(diagnostics_log_path(&resource_paths));
        let auto_compact_token_threshold = config.auto_compact_token_threshold;
        let auto_compact_enabled = config.auto_compact_enabled;
        let last_parent_id = last_parent_id_from_state(&state);
        let context_files = resources.context_files.clone();

        let mut session = Self {
            config,
            state,
            harness,
            last_parent_id,
            pending_initial_entries,
            context_files,
            resource_diagnostics: resources.diagnostics,
            thinking_level,
            auto_compact_token_threshold,
            auto_compact_enabled,
            context_usage_cache: None,
            diagnostic_logger,
            last_diagnostic_log_path: None,
        };
        session.persist_loaded_interrupted_tool_repairs().await?;
        Ok(session)
    }

    // ---- properties -------------------------------------------------------

    /// Session working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.config.cwd
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

    /// Thinking levels available for the active model (dispatch-1: all of them).
    #[must_use]
    pub fn available_thinking_levels(&self) -> Vec<String> {
        THINKING_LEVELS.iter().map(|s| (*s).to_string()).collect()
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

    /// Active model's context window (dispatch-1 fallback).
    #[must_use]
    pub fn context_window_tokens(&self) -> i64 {
        DEFAULT_CONTEXT_WINDOW_TOKENS
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
            let context = self.diagnostic_context();
            let expanded_content = self.expand_prompt_text(&content);

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

            self.try_auto_compact(&context).await;
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

            self.try_auto_compact(&context).await;
            yield CodingSessionEvent::Session(AgentSettledEvent::new().into());
        }
    }

    /// Continue the agent from restored state, persisting new messages.
    pub fn continue_(&mut self) -> impl futures::Stream<Item = CodingSessionEvent> + '_ {
        async_stream::stream! {
            let context = self.diagnostic_context();
            let mut persisted_count = self.harness.messages().len();
            let Ok(mut events) = self.harness.continue_() else {
                return;
            };
            self.invalidate_context_usage_cache();
            while let Some(event) = events.next().await {
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
            self.try_auto_compact(&context).await;
            yield CodingSessionEvent::Session(AgentSettledEvent::new().into());
        }
    }

    // ---- thinking ---------------------------------------------------------

    /// Persist and activate a thinking mode for future turns.
    pub async fn set_thinking_level(&mut self, level: &str) -> Result<String, SessionError> {
        let normalized = normalize_thinking_level(Some(level)).map_err(SessionError::Value)?;
        if normalized == self.thinking_level {
            return Ok(format!("Thinking mode: {normalized}"));
        }
        self.thinking_level = normalized.clone();

        let mut entry = ThinkingLevelChangeEntry::new(Some(normalized.clone()));
        entry.id = self.config.id_gen.new_id();
        entry.timestamp = self.config.clock.now_secs();
        entry.parent_id = self.last_parent_id.clone();
        let entry_id = entry.id.clone();
        self.append_session_entry(SessionEntry::ThinkingLevelChange(entry))
            .await?;
        self.append_leaf(Some(entry_id.clone())).await?;
        self.last_parent_id = Some(entry_id.clone());
        self.refresh_persisted_state(Some(&entry_id)).await?;
        Ok(format!("Thinking mode: {normalized}"))
    }

    /// Cycle to the next thinking mode and persist it.
    pub async fn cycle_thinking_level(&mut self) -> Result<String, SessionError> {
        let available: Vec<&str> = THINKING_LEVELS.to_vec();
        let next = next_thinking_level(Some(&self.thinking_level), &available);
        self.set_thinking_level(&next).await
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

    async fn try_auto_compact(&mut self, _context: &AgentCallDiagnosticContext) -> bool {
        (self.maybe_auto_compact().await).unwrap_or(false)
    }

    async fn try_overflow_compact(&mut self, _context: &AgentCallDiagnosticContext) -> bool {
        let Some(plan) = self.recent_preserving_compaction_plan() else {
            return false;
        };
        let Ok(summary) = self
            .generate_compaction_summary(&plan.messages_to_summarize, None)
            .await
        else {
            return false;
        };
        self.append_compaction(&summary, plan.replace_entry_ids)
            .await
            .is_ok()
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
            self.config.provider.as_ref(),
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
        self.thinking_level = state_thinking_level(&self.state, &self.config.thinking_level);

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

    /// Expand prompt text (dispatch-1: prompt templates/skills are dispatch-2,
    /// so this is currently the identity).
    #[must_use]
    pub fn expand_prompt_text(&self, text: &str) -> String {
        text.to_string()
    }

    /// Persist pending session metadata and add this session to the resume index.
    pub async fn ensure_session_indexed(&mut self) -> Result<(), SessionError> {
        let (Some(session_id), Some(manager)) = (
            self.config.session_id.clone(),
            self.config.session_manager.clone(),
        ) else {
            return Ok(());
        };
        if manager.get_session(&session_id).is_none() {
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

    /// Persist and, on a storage failure, log it and return `None` so the caller
    /// (a turn stream) can abort — mirroring tau's raise-and-abort, without the
    /// stale-count re-append.
    async fn persist_or_log(
        &mut self,
        persisted_count: usize,
        context: &AgentCallDiagnosticContext,
        phase: &str,
    ) -> Option<usize> {
        match self.persist_messages_since(persisted_count).await {
            Ok(count) => Some(count),
            Err(err) => {
                let path = self.diagnostic_logger.log_exception(
                    context,
                    phase,
                    "StorageError",
                    &err.to_string(),
                );
                self.last_diagnostic_log_path = Some(path);
                None
            }
        }
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
        if manager.get_session(session_id).is_some() {
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
        if let Some(record) = manager.get_session(session_id) {
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
        if let Some(record) = manager.get_session(session_id) {
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
    state.model.clone().unwrap_or_else(|| config.model.clone())
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

fn load_session_resources(
    resource_paths: &RhoResourcePaths,
    explicit_context_files: &[ProjectContextFile],
) -> SessionResources {
    let (discovered, diagnostics) =
        discover_project_context_with_diagnostics(Some(resource_paths.clone()));
    SessionResources {
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
