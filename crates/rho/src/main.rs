//! The `rho` binary — a full-parity Rust port of the `tau` coding agent.
//!
//! Print mode (`rho -p`) drives a `CodingSession` against a provider resolved
//! from the built-in catalog (`--provider` / `--model`), or the scripted
//! `FakeProvider` (`--fake`) for an offline demo. The `sessions`, `providers`,
//! `export`, and `setup` subcommands mirror tau's CLI. Interactive TUI mode is
//! M5; running with neither `-p` nor a subcommand reports that.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use rho_agent::messages::{AssistantContent, AssistantMessage, TextContent, ToolCall};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
    ToolCallEndEvent,
};
use rho_agent::session::storage::JsonlSessionStorage;
use rho_agent::session::storage::SessionStorage as _;
use rho_ai::FakeProvider;
use rho_coding::catalog_loader::user_catalog_path;
use rho_coding::credentials::FileCredentialStore;
use rho_coding::provider_config::{
    CredentialReader, DEFAULT_MODEL, DEFAULT_PROVIDER_NAME, OpenAICompatibleProviderConfig,
    ProviderConfig, ProviderSettings, load_provider_settings, provider_kind,
    resolve_provider_selection, save_provider_settings, upsert_openai_compatible_provider,
};
use rho_coding::provider_runtime::create_model_provider;
use rho_coding::session_export::{
    export_session_artifact, normalize_export_format,
};
use rho_coding::session_manager::{CodingSessionRecord, SessionManager};
use rho_coding::thinking::DEFAULT_THINKING_LEVEL;
use rho_coding::{PrintOutputMode, SessionPrintModeConfig, run_session_print_mode};

/// A minimalist Pi-style coding-agent harness (Rust port of tau).
#[derive(Debug, Parser)]
#[command(name = "rho", version, about, long_about = None)]
struct Cli {
    /// Subcommand (sessions / providers / export / setup). Omit for print/TUI mode.
    #[command(subcommand)]
    command: Option<Command>,

    /// Run a single prompt in non-interactive print mode.
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT")]
    prompt: Option<String>,

    /// Configured provider name to use.
    #[arg(long = "provider", value_name = "NAME")]
    provider: Option<String>,

    /// Model to request from the provider.
    #[arg(short = 'm', long = "model", value_name = "MODEL")]
    model: Option<String>,

    /// Working directory for the built-in coding tools.
    #[arg(long = "cwd", value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Output mode for print mode (tau's `--output`/`-o`).
    #[arg(short = 'o', long = "output", value_name = "MODE", default_value = "text")]
    output_format: OutputFormat,

    /// Resume a session id (TUI mode; M5).
    #[arg(long = "resume", value_name = "ID")]
    resume: Option<String>,

    /// Create a new session (TUI mode; M5).
    #[arg(long = "new-session")]
    new_session: bool,

    /// Use the scripted `FakeProvider` (offline demo; ignores real API keys).
    #[arg(long = "fake")]
    fake: bool,

    /// Persist the session transcript to this JSONL path (resumable). Without
    /// it, print mode uses an in-memory session and leaves no files behind.
    #[arg(long = "session", value_name = "PATH")]
    session: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List indexed sessions.
    Sessions,
    /// List configured model providers.
    Providers,
    /// Export an indexed session id or JSONL file to HTML or JSONL.
    Export {
        /// Session id or path to a `.jsonl` transcript.
        session_ref: String,
        /// Export format (`html` or `jsonl`).
        #[arg(long = "format", value_name = "FMT")]
        format: Option<String>,
        /// Output file or directory (defaults into the cwd).
        output: Option<PathBuf>,
    },
    /// Create or update an OpenAI-compatible provider entry.
    Setup {
        /// Provider name to create/update.
        #[arg(long = "provider", value_name = "NAME", default_value = DEFAULT_PROVIDER_NAME)]
        provider: String,
        /// OpenAI-compatible base URL.
        #[arg(long = "base-url", default_value = "https://api.openai.com/v1")]
        base_url: String,
        /// API-key environment variable.
        #[arg(long = "api-key-env", default_value = "OPENAI_API_KEY")]
        api_key_env: String,
        /// Model id.
        #[arg(short = 'm', long = "model", default_value = DEFAULT_MODEL)]
        model: String,
        /// HTTP timeout in seconds.
        #[arg(long = "timeout-seconds", default_value_t = 60.0)]
        timeout_seconds: f64,
        /// Provider retry count.
        #[arg(long = "max-retries", default_value_t = 2)]
        max_retries: i64,
        /// Provider retry delay cap in seconds.
        #[arg(long = "max-retry-delay-seconds", default_value_t = 1.0)]
        max_retry_delay_seconds: f64,
        /// Make this provider the default.
        #[arg(long = "no-set-default", action = clap::ArgAction::SetFalse)]
        set_default: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Transcript,
}

impl From<OutputFormat> for PrintOutputMode {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Text => Self::Text,
            OutputFormat::Json => Self::Json,
            OutputFormat::Transcript => Self::Transcript,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Error: failed to start async runtime: {err}");
            std::process::exit(1);
        }
    };

    // Exit codes mirror tau: a configuration error (`BadParameter`) exits 2, a
    // non-recoverable run (assistant error) exits 1, success exits 0.
    match runtime.block_on(async move { run(cli).await }) {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(2);
        }
    }
}

async fn run(cli: Cli) -> Result<bool, String> {
    if let Some(command) = cli.command {
        return run_subcommand(command).await;
    }

    let Some(prompt) = cli.prompt.clone() else {
        // Interactive TUI mode is M5.
        if cli.resume.is_some() || cli.new_session {
            return Err("--resume / --new-session require interactive mode, which is M5.".to_string());
        }
        return Err(format!(
            "rho {}: interactive mode is not implemented yet (M5). Use -p to run a prompt, or a \
subcommand (sessions/providers/export/setup).",
            env!("CARGO_PKG_VERSION")
        ));
    };

    run_print(cli, prompt).await
}

async fn run_subcommand(command: Command) -> Result<bool, String> {
    match command {
        Command::Sessions => {
            let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
            render_session_list(&manager.list_sessions(None));
            Ok(true)
        }
        Command::Providers => {
            let credentials = FileCredentialStore::at_default();
            let settings =
                load_provider_settings(None, Some(&credentials as &dyn CredentialReader))
                    .map_err(|err| err.0)?;
            render_provider_settings(&settings, Some(&credentials));
            Ok(true)
        }
        Command::Export {
            session_ref,
            format,
            output,
        } => {
            let path = export_session_command(&session_ref, output, format.as_deref()).await?;
            println!("Exported session to {}", path.display());
            Ok(true)
        }
        Command::Setup {
            provider,
            base_url,
            api_key_env,
            model,
            timeout_seconds,
            max_retries,
            max_retry_delay_seconds,
            set_default,
        } => {
            let settings = load_provider_settings(None, None).map_err(|err| err.0)?;
            let mut config = OpenAICompatibleProviderConfig::new(provider);
            config.base_url = base_url.trim_end_matches('/').to_string();
            config.api_key_env = api_key_env.clone();
            config.models = vec![model.clone()];
            config.default_model = model;
            config.timeout_seconds = timeout_seconds;
            config.max_retries = max_retries;
            config.max_retry_delay_seconds = max_retry_delay_seconds;
            let provider_name = config.name.clone();
            let updated = upsert_openai_compatible_provider(&settings, config, set_default)
                .map_err(|err| err.0)?;
            let path = save_provider_settings(&updated, None).map_err(|err| err.0)?;
            println!(
                "Saved provider '{provider_name}' to {} and preferences to {}",
                user_catalog_path(None).display(),
                path.display()
            );
            if std::env::var(&api_key_env).is_err() {
                eprintln!("Set {api_key_env} before running rho with this provider.");
            }
            Ok(true)
        }
    }
}

async fn run_print(cli: Cli, prompt: String) -> Result<bool, String> {
    let cwd = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let use_fake = cli.fake || std::env::var("RHO_FAKE").is_ok_and(|v| v == "1");
    let (provider, model, provider_name): (Arc<dyn ModelProvider>, String, String) = if use_fake {
        (
            Arc::new(demo_fake_provider()),
            cli.model.clone().unwrap_or_else(|| "fake".to_string()),
            cli.provider.clone().unwrap_or_else(|| "fake".to_string()),
        )
    } else {
        let credentials = FileCredentialStore::at_default();
        let settings = load_provider_settings(None, Some(&credentials as &dyn CredentialReader))
            .map_err(|err| err.0)?;
        let selection =
            resolve_provider_selection(&settings, cli.provider.as_deref(), cli.model.as_deref())
                .map_err(|err| err.0)?;
        let provider = create_model_provider(
            &selection.provider,
            None,
            Some(&selection.model),
            Some(DEFAULT_THINKING_LEVEL),
        )
        .map_err(|err| err.0)?;
        let provider_name = selection.provider.name().to_string();
        (provider, selection.model, provider_name)
    };

    let mut config = SessionPrintModeConfig::new(prompt, model, cwd, provider)
        .with_output(cli.output_format.into())
        .with_session_path(cli.session.clone());
    config.provider_name = provider_name;
    Ok(run_session_print_mode(config).await)
}

/// Render indexed sessions for the CLI (tau `render_session_list`).
fn render_session_list(records: &[CodingSessionRecord]) {
    if records.is_empty() {
        println!("No sessions found.");
        return;
    }
    for record in records {
        let title = record.title.as_deref().unwrap_or("Untitled");
        println!(
            "{}\t{title}\t{}\t{}",
            record.id,
            record.model,
            record.cwd.display()
        );
    }
}

/// Render configured providers for the CLI (tau `render_provider_settings`).
fn render_provider_settings(settings: &ProviderSettings, credentials: Option<&FileCredentialStore>) {
    for provider in &settings.providers {
        let marker = if provider.name() == settings.default_provider {
            "*"
        } else {
            " "
        };
        let models = provider.models().join(",");
        println!(
            "{marker}\t{}\t{}\t{}\t{models}\t{}\t{}\t{}\t{}s\tretries={}\tretry_delay={}s",
            provider.name(),
            provider_kind(provider),
            provider.default_model(),
            provider.api_key_env(),
            provider_credential_status(provider, credentials),
            provider.base_url(),
            format_g(provider.timeout_seconds()),
            provider.max_retries(),
            format_g(provider.max_retry_delay_seconds()),
        );
    }
}

fn provider_credential_status(
    provider: &ProviderConfig,
    credentials: Option<&FileCredentialStore>,
) -> String {
    if let (Some(credential_name), Some(store)) = (provider.credential_name(), credentials) {
        if provider_kind(provider) == "openai-codex" {
            if store.get_oauth(credential_name).ok().flatten().is_some() {
                return format!("stored:{credential_name}");
            }
        } else if store.get(credential_name).ok().flatten().is_some() {
            return format!("stored:{credential_name}");
        }
    }
    if std::env::var(provider.api_key_env()).is_ok_and(|value| !value.is_empty()) {
        return format!("env:{}", provider.api_key_env());
    }
    "missing".to_string()
}

/// Export an indexed session id or JSONL file path (tau `export_session_command`).
async fn export_session_command(
    session_ref: &str,
    output: Option<PathBuf>,
    format: Option<&str>,
) -> Result<PathBuf, String> {
    let (session_path, title) = resolve_export_source(session_ref)?;
    let entries = JsonlSessionStorage::new(&session_path)
        .read_all()
        .await
        .map_err(|err| err.to_string())?;
    let output_suffix = output
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|ext| ext.to_str());
    let normalized_format = normalize_export_format(format.or(output_suffix).or(Some("html")))
        .map_err(|err| err.to_string())?;
    let destination = resolve_export_destination(output.as_deref(), &session_path, &normalized_format);
    export_session_artifact(
        &entries,
        &destination,
        &title,
        Some(&session_path.to_string_lossy()),
        Some(&normalized_format),
    )
    .map_err(|err| err.to_string())
}

fn resolve_export_source(session_ref: &str) -> Result<(PathBuf, String), String> {
    let candidate = expanduser(session_ref);
    if candidate.exists() {
        if candidate.is_dir() {
            return Err(format!(
                "Session export source is a directory: {}",
                candidate.display()
            ));
        }
        let stem = candidate
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok((candidate, format!("Tau session {stem}")));
    }
    let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
    let record = manager
        .get_session(session_ref)
        .ok_or_else(|| format!("Unknown session or file: {session_ref}"))?;
    let title = record
        .title
        .clone()
        .unwrap_or_else(|| format!("Tau session {}", record.id));
    Ok((record.path, title))
}

fn resolve_export_destination(
    output: Option<&std::path::Path>,
    session_path: &std::path::Path,
    format: &str,
) -> PathBuf {
    match output {
        None => rho_coding::session_export::default_session_export_artifact_path(
            session_path,
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            format,
        ),
        Some(path) if path.extension().is_some() => path.to_path_buf(),
        Some(dir) => {
            rho_coding::session_export::default_session_export_artifact_path(session_path, dir, format)
        }
    }
}

/// Expand a leading `~` / `~/` against `$HOME` (tau's `Path.expanduser`).
fn expanduser(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

/// Python `%g`-style float formatting for the provider table (trims trailing
/// zeros; whole numbers render without a decimal point).
fn format_g(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e16 {
        return format!("{}", value as i64);
    }
    let mut text = format!("{value}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

/// A scripted [`FakeProvider`] that showcases the whole slice offline: turn 1
/// calls the real `bash` tool (`ls -la`), turn 2 answers in text.
fn demo_fake_provider() -> FakeProvider {
    let model = "fake";

    let tool_call = ToolCall::new("call_1", "bash", bash_args("ls -la"));
    let tool_msg = AssistantMessage::new(vec![AssistantContent::ToolCall(tool_call.clone())])
        .with_model(model)
        .with_stop_reason(rho_agent::messages::StopReason::ToolUse);
    let stream1 = vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(
            AssistantMessage::new(Vec::new()).with_model(model),
        )),
        AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(0, tool_call, tool_msg.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::ToolUse, tool_msg)),
    ];

    let answer = "I ran `ls -la` in the working directory (see the tool output above) and \
summarized the project structure. This response was produced offline by rho's FakeProvider.";
    let text_msg = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new(answer))])
        .with_model(model);
    let stream2 = vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(
            AssistantMessage::new(Vec::new()).with_model(model),
        )),
        AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, answer, text_msg.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::Stop, text_msg)),
    ];

    FakeProvider::new(vec![stream1, stream2])
}

fn bash_args(command: &str) -> rho_agent::types::JsonMap {
    let mut map = rho_agent::types::JsonMap::new();
    map.insert(
        "command".to_string(),
        serde_json::Value::String(command.to_string()),
    );
    map
}
