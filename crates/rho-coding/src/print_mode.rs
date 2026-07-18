//! Non-interactive print mode (a port of tau's `cli.run_print_mode`).
//!
//! [`run_print_mode`] is the M4a bare-[`AgentHarness`] slice, retained as the
//! crosscheck-v1 oracle (its stream is exactly the harness stream).
//! [`run_session_print_mode`] is the M4b session-backed path the `rho -p` CLI
//! now drives: it builds a [`CodingSession`], creates + indexes a session record
//! and persists the transcript as JSONL at its path (tau
//! `run_openai_print_mode`; `--session <path>` is a rho-only unindexed override),
//! and renders the full `CodingSessionEvent` stream (harness events plus the
//! session-owned `agent_settled` / `queue_update` / `compaction_*` /
//! `auto_retry_*`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;

use rho_agent::clock::{Clock, IdGen, system_clock, uuid_id_gen};
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::provider::ModelProvider;
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::storage::{SessionStorage, SessionStorageError};

use crate::commands::format_reload_summary;
use crate::events::CodingSessionEvent;
use crate::paths::RhoPaths;
use crate::provider_config::{ProviderConfig, ProviderSettings};
use crate::rendering::{PrintOutputMode, create_event_renderer};
use crate::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};
use crate::session_manager::SessionManager;
use crate::system_prompt::{BuildSystemPromptOptions, build_system_prompt};
use crate::tools::create_coding_tools;

/// An in-memory [`SessionStorage`] for one-shot print runs that do not persist
/// to a JSONL file (tau's `_MemorySessionStorage`).
#[derive(Debug, Default)]
pub struct MemorySessionStorage {
    entries: Mutex<Vec<SessionEntry>>,
}

#[async_trait]
impl SessionStorage for MemorySessionStorage {
    async fn append(&self, entry: &SessionEntry) -> Result<(), SessionStorageError> {
        self.entries.lock().expect("poisoned").push(entry.clone());
        Ok(())
    }

    async fn read_all(&self) -> Result<Vec<SessionEntry>, SessionStorageError> {
        Ok(self.entries.lock().expect("poisoned").clone())
    }
}

/// Configuration for [`run_session_print_mode`].
pub struct SessionPrintModeConfig {
    /// The prompt to run.
    pub prompt: String,
    /// The requested model id.
    pub model: String,
    /// The working directory for the coding tools.
    pub cwd: PathBuf,
    /// The model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// The output mode.
    pub output: PrintOutputMode,
    /// Optional shell-command prefix for the bash tool.
    pub shell_command_prefix: Option<String>,
    /// Clock for message/entry timestamps.
    pub clock: Arc<dyn Clock>,
    /// Id generator for session entries.
    pub id_gen: Arc<dyn IdGen>,
    /// Active provider name.
    pub provider_name: String,
    /// Durable provider settings (enables catalog-aware model/thinking).
    pub provider_settings: Option<ProviderSettings>,
    /// Runtime provider config for the active selection.
    pub runtime_provider_config: Option<ProviderConfig>,
    /// Explicit JSONL session path (`--session`, rho-only override). When set,
    /// the transcript is persisted here and *not* indexed. When `None`, the
    /// default tau path runs: a session record is created + indexed in
    /// [`session_manager`](Self::session_manager) and the transcript persists at
    /// its `record.path`.
    pub session_path: Option<PathBuf>,
    /// Session manager for the default (indexed) path. `None` uses the real
    /// `~/.rho` index; tests inject a temp-dir manager. Ignored when
    /// `session_path` is set.
    pub session_manager: Option<SessionManager>,
    /// Explicit extension component paths (the `-x/--extension` flag). Loaded on
    /// top of directory discovery. tau's extensions run in print mode too.
    pub extension_paths: Vec<PathBuf>,
}

impl SessionPrintModeConfig {
    /// Build a config with the real-time clock and default ids.
    #[must_use]
    pub fn new(
        prompt: impl Into<String>,
        model: impl Into<String>,
        cwd: PathBuf,
        provider: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            prompt: prompt.into(),
            model: model.into(),
            cwd,
            provider,
            output: PrintOutputMode::Text,
            shell_command_prefix: None,
            clock: system_clock(),
            id_gen: uuid_id_gen(),
            provider_name: "openai".to_string(),
            provider_settings: None,
            runtime_provider_config: None,
            session_path: None,
            session_manager: None,
            extension_paths: Vec::new(),
        }
    }

    /// Set explicit extension component paths (`-x/--extension`).
    #[must_use]
    pub fn with_extension_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extension_paths = paths;
        self
    }

    /// Set the output mode.
    #[must_use]
    pub fn with_output(mut self, output: PrintOutputMode) -> Self {
        self.output = output;
        self
    }

    /// Set the JSONL session path.
    #[must_use]
    pub fn with_session_path(mut self, path: Option<PathBuf>) -> Self {
        self.session_path = path;
        self
    }
}

/// Run one prompt through a [`CodingSession`] and render its event stream.
///
/// This is the session-backed CLI path. By default (tau parity) a session
/// record is created + indexed and the transcript persists as JSONL at its
/// `record.path`, so the run is listed by `rho sessions` and resumable. When
/// `session_path` is set (the rho-only `--session` override) the transcript is
/// persisted there instead and left unindexed.
pub async fn run_session_print_mode(config: SessionPrintModeConfig) -> bool {
    // Default path (tau `run_openai_print_mode`): create + index a session
    // record and persist the transcript at `record.path`, so the run is listed
    // by `rho sessions` and resumable. `--session <path>` is a rho-only explicit
    // override: persist there, unindexed.
    let (storage, cwd, session_id, session_manager): (
        Arc<dyn SessionStorage>,
        PathBuf,
        Option<String>,
        Option<SessionManager>,
    ) = if let Some(path) = config.session_path {
        (jsonl_session_storage(&path), config.cwd, None, None)
    } else {
        let manager = config
            .session_manager
            .unwrap_or_else(|| SessionManager::new(RhoPaths::default()));
        let record = manager.create_session(
            &config.cwd,
            &config.model,
            Some(&config.provider_name),
            None,
            None,
        );
        (
            jsonl_session_storage(&record.path),
            record.cwd.clone(),
            Some(record.id),
            Some(manager),
        )
    };
    let mut session_config = CodingSessionConfig::new(config.provider, config.model, storage, cwd);
    session_config.shell_command_prefix = config.shell_command_prefix;
    session_config.clock = config.clock;
    session_config.id_gen = config.id_gen;
    session_config.provider_name = config.provider_name;
    session_config.provider_settings = config.provider_settings;
    session_config.runtime_provider_config = config.runtime_provider_config;
    session_config.session_id = session_id;
    session_config.session_manager = session_manager;
    session_config.extension_paths = config.extension_paths;

    let mut session = match CodingSession::load(session_config).await {
        Ok(session) => session,
        Err(err) => {
            eprintln!("Error: {err}");
            return false;
        }
    };

    // `!cmd` / `!!cmd` run a terminal command instead of prompting the agent
    // (tau's `run_print_mode` routes these before the agent turn).
    if let Some(request) = crate::session::parse_terminal_command(&config.prompt) {
        let ok = match session
            .run_terminal_command(&request.command, request.add_to_context)
            .await
        {
            Ok(result) => {
                println!("{}", format_terminal_command_result(&result));
                result.ok
            }
            Err(err) => {
                eprintln!("Error: {err}");
                false
            }
        };
        emit_extension_shutdown(&session).await;
        return ok;
    }

    // Slash commands are handled before the agent turn (tau `run_print_mode`):
    // a handled command prints its message (running `/reload` if requested) and
    // returns; only an unhandled prompt drives the agent.
    let command = session.handle_command(&config.prompt);
    if command.handled {
        let mut message = command.message;
        if command.reload_requested {
            message = Some(match session.reload().await {
                Ok(summary) => format_reload_summary(&summary),
                Err(err) => format!("Could not reload: {err}"),
            });
        }
        if let Some(message) = message {
            if !message.is_empty() {
                println!("{message}");
            }
        }
        emit_extension_shutdown(&session).await;
        return true;
    }

    let mut renderer = create_event_renderer(config.output);
    let mut persist_failed = false;
    {
        let stream = session.prompt(config.prompt, None);
        futures::pin_mut!(stream);
        while let Some(event) = stream.next().await {
            renderer.render(&event);
        }
    }
    // tau's `prompt()` re-raises a persistence failure, aborting the turn with a
    // non-zero CLI exit; rho's stream ends without `agent_settled` and records
    // the error, which the CLI surfaces here as a failed run.
    if let Some(err) = session.take_run_error() {
        eprintln!("Error: {err}");
        persist_failed = true;
    }
    emit_extension_shutdown(&session).await;
    let ok = renderer.finish();
    ok && !persist_failed
}

/// Fire `session_shutdown` for extensions on print-mode exit (tau's quit
/// lifecycle). A cheap no-op when no extensions are loaded.
async fn emit_extension_shutdown(session: &CodingSession) {
    if session.extension_runtime().has_extensions() {
        session
            .extension_runtime()
            .emit_session_shutdown("quit")
            .await;
    }
}

/// Format an input-bar terminal command result (tau
/// `cli._format_terminal_command_result`): a `$ cmd` echo, a context-status
/// line, then the raw output.
fn format_terminal_command_result(result: &crate::session::TerminalCommandResult) -> String {
    let context_status = if result.added_to_context {
        "added to context"
    } else {
        "not added to context"
    };
    format!(
        "$ {}\n[{context_status}]\n{}",
        result.command, result.output
    )
}

/// Configuration for [`run_print_mode`].
pub struct PrintModeConfig {
    /// The prompt to run.
    pub prompt: String,
    /// The requested model id.
    pub model: String,
    /// The working directory for the coding tools.
    pub cwd: PathBuf,
    /// The model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// The output mode.
    pub output: PrintOutputMode,
    /// Optional shell-command prefix for the bash tool.
    pub shell_command_prefix: Option<String>,
    /// Clock for harness-authored message timestamps.
    pub clock: Arc<dyn Clock>,
}

impl PrintModeConfig {
    /// Build a config with print-text output and the real-time clock.
    #[must_use]
    pub fn new(
        prompt: impl Into<String>,
        model: impl Into<String>,
        cwd: PathBuf,
        provider: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            prompt: prompt.into(),
            model: model.into(),
            cwd,
            provider,
            output: PrintOutputMode::Text,
            shell_command_prefix: None,
            clock: system_clock(),
        }
    }

    /// Set the output mode.
    #[must_use]
    pub fn with_output(mut self, output: PrintOutputMode) -> Self {
        self.output = output;
        self
    }

    /// Set the clock (tests / reproducible goldens).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// Run one prompt and render the harness event stream, returning success.
pub async fn run_print_mode(config: PrintModeConfig) -> bool {
    let tools = create_coding_tools(&config.cwd, config.shell_command_prefix.as_deref());
    let system = build_system_prompt(&BuildSystemPromptOptions {
        cwd: config.cwd.clone(),
        tools: tools.clone(),
        ..Default::default()
    });

    let harness_config = AgentHarnessConfig::new(config.provider, config.model, system)
        .with_tools(tools)
        .with_clock(config.clock);
    let harness = AgentHarness::new(harness_config, Vec::new());

    let mut renderer = create_event_renderer(config.output);
    let mut stream = match harness.prompt(&config.prompt) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("Error: {err}");
            return false;
        }
    };
    while let Some(event) = stream.next().await {
        renderer.render(&CodingSessionEvent::Agent(event));
    }
    renderer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TerminalCommandResult;

    #[test]
    fn terminal_command_result_matches_tau_format() {
        // tau: f"$ {command}\n[{context_status}]\n{output}".
        let added = TerminalCommandResult {
            command: "ls -la".to_string(),
            output: "a\nb".to_string(),
            exit_code: Some(0),
            ok: true,
            added_to_context: true,
        };
        assert_eq!(
            format_terminal_command_result(&added),
            "$ ls -la\n[added to context]\na\nb"
        );
        let not_added = TerminalCommandResult {
            added_to_context: false,
            ..added
        };
        assert_eq!(
            format_terminal_command_result(&not_added),
            "$ ls -la\n[not added to context]\na\nb"
        );
    }
}
