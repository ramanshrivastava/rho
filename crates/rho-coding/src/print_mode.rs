//! Non-interactive print mode (a port of tau's `cli.run_print_mode`).
//!
//! [`run_print_mode`] is the M4a bare-[`AgentHarness`] slice, retained as the
//! crosscheck-v1 oracle (its stream is exactly the harness stream).
//! [`run_session_print_mode`] is the M4b session-backed path the `rho -p` CLI
//! now drives: it builds a [`CodingSession`], persists the transcript (JSONL
//! when `--session` is given, in-memory otherwise), and renders the full
//! `CodingSessionEvent` stream (harness events plus the session-owned
//! `agent_settled` / `queue_update` / `compaction_*` / `auto_retry_*`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;

use rho_agent::clock::{Clock, IdGen, system_clock, uuid_id_gen};
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::provider::ModelProvider;
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::storage::{SessionStorage, SessionStorageError};

use crate::events::CodingSessionEvent;
use crate::rendering::{PrintOutputMode, create_event_renderer};
use crate::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};
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
    /// JSONL session path (`--session`); `None` uses in-memory storage.
    pub session_path: Option<PathBuf>,
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
            session_path: None,
        }
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
/// This is the session-backed CLI path. When `session_path` is set the
/// transcript is persisted as JSONL (resumable with `--session`); otherwise an
/// in-memory store is used so a one-shot `rho -p` leaves no files behind.
pub async fn run_session_print_mode(config: SessionPrintModeConfig) -> bool {
    let storage: Arc<dyn SessionStorage> = match &config.session_path {
        Some(path) => jsonl_session_storage(path),
        None => Arc::new(MemorySessionStorage::default()),
    };
    let mut session_config =
        CodingSessionConfig::new(config.provider, config.model, storage, config.cwd);
    session_config.shell_command_prefix = config.shell_command_prefix;
    session_config.clock = config.clock;
    session_config.id_gen = config.id_gen;
    session_config.provider_name = config.provider_name;

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
        return match session
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
    let ok = renderer.finish();
    ok && !persist_failed
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
