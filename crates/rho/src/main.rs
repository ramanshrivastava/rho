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
use rho_coding::login_required::LoginRequiredProvider;
use rho_coding::provider_config::{
    CredentialReader, DEFAULT_MODEL, DEFAULT_PROVIDER_NAME, OpenAICompatibleProviderConfig,
    ProviderConfig, ProviderConfigError, ProviderSelection, ProviderSettings,
    load_provider_settings, provider_has_usable_credentials, provider_kind,
    resolve_provider_selection, save_provider_settings, upsert_openai_compatible_provider,
};
use rho_coding::provider_runtime::create_model_provider;
use rho_coding::session_export::{export_session_artifact, normalize_export_format};
use rho_coding::session_manager::{CodingSessionRecord, SessionManager};
use rho_coding::thinking::DEFAULT_THINKING_LEVEL;
use rho_coding::{PrintOutputMode, SessionPrintModeConfig, run_session_print_mode};

/// A minimalist Pi-style coding-agent harness (Rust port of tau).
#[derive(Debug, Parser)]
#[command(
    name = "rho",
    version,
    about,
    long_about = "rho — a minimalist coding-agent harness.\n\nLineage: π → τ → ρ. \
                  rho (ρ) is a Rust port of tau (τ), itself a descendant of pi (π): \
                  byte-for-byte wire/session/CLI compatibility with tau, with rho's own \
                  look, feel, and performance."
)]
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
    #[arg(
        short = 'o',
        long = "output",
        value_name = "MODE",
        default_value = "text"
    )]
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

    /// Load a WASM extension component, a file or a directory (repeatable).
    /// Requires a `--features wasmtime` build; otherwise the specs are inert and
    /// a note is printed.
    #[arg(long = "extension", short = 'x', value_name = "SPEC")]
    extension: Vec<String>,

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
        /// Make this provider the default (the default behavior).
        #[arg(long = "set-default", action = clap::ArgAction::SetTrue, overrides_with = "no_set_default")]
        set_default: bool,
        /// Do not make this provider the default.
        #[arg(long = "no-set-default", action = clap::ArgAction::SetTrue, overrides_with = "set_default")]
        no_set_default: bool,
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
    match runtime.block_on(Box::pin(run(cli))) {
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
        // No `-p` and no subcommand: launch the interactive ratatui TUI.
        return Box::pin(run_tui_entry(cli)).await;
    };

    Box::pin(run_print(cli, prompt)).await
}

/// Launch the interactive TUI (the `rho` no-`-p`, no-subcommand entry point).
async fn run_tui_entry(cli: Cli) -> Result<bool, String> {
    // `--resume` and `--new-session` are mutually exclusive (tau `BadParameter`).
    if cli.resume.is_some() && cli.new_session {
        return Err("--resume and --new-session cannot be combined.".to_string());
    }
    warn_if_extensions_unavailable(&cli);

    let (session, startup_message) = build_interactive_session(&cli).await?;
    let paths = rho_coding::paths::RhoPaths::default();
    // Malformed `tui.json` shouldn't be silently ignored: warn (so the user knows
    // why their theme/keybindings were dropped) and fall back to defaults.
    let settings = match rho_tui::load_tui_settings(&paths) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("Warning: ignoring invalid TUI settings ({err}); using defaults.");
            rho_tui::TuiSettings::default()
        }
    };
    Box::pin(rho_tui::app::run_tui(session, settings, startup_message))
        .await
        .map_err(|err| format!("TUI error: {err}"))?;
    Ok(true)
}

/// Build a persistent, resumable [`CodingSession`] for interactive use,
/// resolving the provider like print mode and honoring `--resume`.
async fn build_interactive_session(
    cli: &Cli,
) -> Result<(rho_coding::session::CodingSession, Option<String>), String> {
    use rho_coding::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};

    let default_cwd = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
    // Load the resume record up front so its stored provider/model drive the
    // startup selection (and thus the login-required decision), rather than
    // being stamped on after a provider was already built for a *different*
    // selection (tau resolves the resume record before constructing the
    // provider; `_resolve_tui_startup_selection(explicit_resume=...)`).
    let resume_record = match &cli.resume {
        Some(resume_id) => Some(
            manager
                .get_session(resume_id)
                .map_err(|err| err.0)?
                .ok_or_else(|| format!("Unknown session id: {resume_id}"))?,
        ),
        None => None,
    };

    let startup = resolve_startup_provider(cli, resume_record.as_ref())?;

    let (storage, cwd, session_id) = if let Some(record) = resume_record {
        (
            jsonl_session_storage(&record.path),
            record.cwd.clone(),
            Some(record.id),
        )
    } else {
        let record = manager.create_session(
            &default_cwd,
            &startup.model,
            Some(&startup.provider_name),
            None,
            None,
        );
        (
            jsonl_session_storage(&record.path),
            record.cwd.clone(),
            Some(record.id),
        )
    };

    let mut config = CodingSessionConfig::new(startup.provider, startup.model, storage, cwd);
    config.provider_name = startup.provider_name;
    config.provider_settings = startup.provider_settings;
    config.runtime_provider_config = startup.runtime_provider;
    config.session_id = session_id;
    config.session_manager = Some(manager);
    config.extension_paths = extension_paths_from_cli(cli);
    let session = CodingSession::load(config)
        .await
        .map_err(|err| err.to_string())?;
    Ok((session, startup.startup_message))
}

/// The explicit extension component paths from `-x/--extension`.
fn extension_paths_from_cli(cli: &Cli) -> Vec<PathBuf> {
    cli.extension.iter().map(PathBuf::from).collect()
}

/// Warn once when `-x` extensions were requested but this binary was built
/// without the `wasmtime` feature, so the components will be inert. With the
/// feature on this is a no-op (the runtime loads them). Per-extension load
/// failures still surface through the session's diagnostics either way.
fn warn_if_extensions_unavailable(cli: &Cli) {
    #[cfg(not(feature = "wasmtime"))]
    if !cli.extension.is_empty() {
        eprintln!(
            "Note: {} extension spec(s) requested, but this rho was built without the \
`wasmtime` feature; extensions are inert. Rebuild with `--features wasmtime` to load them.",
            cli.extension.len()
        );
    }
    #[cfg(feature = "wasmtime")]
    let _ = cli;
}

/// The live provider and session metadata to launch the interactive TUI with.
struct StartupProvider {
    provider: Arc<dyn ModelProvider>,
    model: String,
    provider_name: String,
    provider_settings: Option<ProviderSettings>,
    runtime_provider: Option<ProviderConfig>,
    /// A login-required notice to surface on launch (tau's startup warning),
    /// set only for the `LoginRequiredProvider` placeholder path.
    startup_message: Option<String>,
}

/// Resolve the live provider for the interactive TUI: the scripted `--fake`
/// provider, a real credentialed provider, or the `LoginRequiredProvider`
/// placeholder when the resolved provider has no usable credential.
///
/// tau parity (`run_tui_app`): a missing credential must not abort the TUI. The
/// model is validated by `resolve_provider_selection` before this point, so a
/// missing credential is the only reason a real provider can't be built — no
/// error classification is needed. For the placeholder path the runtime provider
/// config is cleared to `None` (tau sets `runtime_provider_config = None`): this
/// keeps `CodingSession::load`'s eager `refresh_runtime_provider` a no-op instead
/// of having it re-build the real provider — which would hit the very missing-key
/// error we just sidestepped and abort before the TUI ever renders. A later
/// `/login` rebuilds the real provider by re-deriving the config from provider
/// settings (`CodingSession::set_model_provider`), so nothing is lost by clearing
/// it here. A genuine configuration failure on a credentialed provider still
/// aborts, matching tau (which lets `ProviderConfigError` propagate).
fn resolve_startup_provider(
    cli: &Cli,
    resume_record: Option<&CodingSessionRecord>,
) -> Result<StartupProvider, String> {
    let use_fake = cli.fake || std::env::var("RHO_FAKE").is_ok_and(|v| v == "1");
    if use_fake {
        return Ok(StartupProvider {
            provider: Arc::new(demo_fake_provider()),
            model: resume_record
                .map(|record| record.model.clone())
                .or_else(|| cli.model.clone())
                .unwrap_or_else(|| "fake".to_string()),
            provider_name: resume_record
                .and_then(|record| record.provider_name.clone())
                .or_else(|| cli.provider.clone())
                .unwrap_or_else(|| "fake".to_string()),
            provider_settings: None,
            runtime_provider: None,
            startup_message: None,
        });
    }

    let credentials = FileCredentialStore::at_default();
    let settings = load_provider_settings(None, Some(&credentials as &dyn CredentialReader))
        .map_err(|err| err.0)?;
    // A resume adopts the stored provider/model (authoritative); a fresh launch
    // honors `--provider`/`--model`, else a credentialed default.
    let selection = if let Some(record) = resume_record {
        let provider_name = record.provider_name.as_deref().or(cli.provider.as_deref());
        resolve_provider_selection(&settings, provider_name, Some(record.model.as_str()))
            .map_err(|err| err.0)?
    } else {
        resolve_tui_startup_selection(
            &settings,
            cli.provider.as_deref(),
            cli.model.as_deref(),
            Some(&credentials as &dyn CredentialReader),
        )
        .map_err(|err| err.0)?
    };
    let name = selection.provider.name().to_string();

    if provider_has_usable_credentials(
        &selection.provider,
        Some(&credentials as &dyn CredentialReader),
    ) {
        let provider = create_model_provider(
            &selection.provider,
            None,
            Some(&selection.model),
            Some(DEFAULT_THINKING_LEVEL),
        )
        .map_err(|err| err.0)?;
        return Ok(StartupProvider {
            provider,
            model: selection.model,
            provider_name: name,
            provider_settings: Some(settings),
            runtime_provider: Some(selection.provider),
            startup_message: None,
        });
    }

    let message = format!(
        "Login required. Run /login to choose a provider, \
         or /login {name} to continue with the current provider."
    );
    Ok(StartupProvider {
        provider: Arc::new(LoginRequiredProvider::new(message.clone())),
        model: selection.model,
        provider_name: name,
        provider_settings: Some(settings),
        // tau parity: clear the runtime config for the placeholder so the eager
        // `refresh_runtime_provider` in `CodingSession::load` stays a no-op rather
        // than re-building the credential-less real provider and aborting.
        runtime_provider: None,
        startup_message: Some(message),
    })
}

/// Resolve the provider/model to launch the interactive TUI with (tau
/// `_resolve_tui_startup_selection`, non-resume path).
///
/// An explicit `--provider`/`--model` is honored verbatim. Otherwise the default
/// selection is used when it has a usable credential; failing that, the first
/// configured provider with a usable credential is chosen so a user who is
/// logged into *some* provider still lands on a working one. When nothing is
/// usable the default selection is returned unchanged and the caller substitutes
/// the login-required placeholder.
fn resolve_tui_startup_selection(
    settings: &ProviderSettings,
    provider_name: Option<&str>,
    model: Option<&str>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSelection, ProviderConfigError> {
    if provider_name.is_some() || model.is_some() {
        return resolve_provider_selection(settings, provider_name, model);
    }
    let default_selection = resolve_provider_selection(settings, None, None)?;
    if provider_has_usable_credentials(&default_selection.provider, credentials) {
        return Ok(default_selection);
    }
    for provider in &settings.providers {
        if provider_has_usable_credentials(provider, credentials) {
            let model = provider.default_model().to_string();
            return Ok(ProviderSelection {
                provider: provider.clone(),
                model,
            });
        }
    }
    Ok(default_selection)
}

async fn run_subcommand(command: Command) -> Result<bool, String> {
    match command {
        Command::Sessions => {
            let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
            // A corrupt index is fatal (tau parity): surface it as a non-zero
            // CLI exit rather than silently listing a subset.
            let records = manager.list_sessions(None).map_err(|err| err.0)?;
            render_session_list(&records);
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
            no_set_default,
        } => {
            // tau `--set-default/--no-set-default`: default true; `overrides_with`
            // makes the last-specified flag win.
            let set_default = set_default || !no_set_default;
            let settings = load_provider_settings(None, None).map_err(|err| err.0)?;
            let mut config = OpenAICompatibleProviderConfig::new(provider);
            config.base_url = base_url.trim_end_matches('/').to_string();
            config.api_key_env.clone_from(&api_key_env);
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
    let mut settings = None;
    let mut runtime_provider = None;
    let (provider, model, provider_name): (Arc<dyn ModelProvider>, String, String) = if use_fake {
        (
            Arc::new(demo_fake_provider()),
            cli.model.clone().unwrap_or_else(|| "fake".to_string()),
            cli.provider.clone().unwrap_or_else(|| "fake".to_string()),
        )
    } else {
        let credentials = FileCredentialStore::at_default();
        let provider_settings =
            load_provider_settings(None, Some(&credentials as &dyn CredentialReader))
                .map_err(|err| err.0)?;
        let selection = resolve_provider_selection(
            &provider_settings,
            cli.provider.as_deref(),
            cli.model.as_deref(),
        )
        .map_err(|err| err.0)?;
        let provider = create_model_provider(
            &selection.provider,
            None,
            Some(&selection.model),
            Some(DEFAULT_THINKING_LEVEL),
        )
        .map_err(|err| err.0)?;
        let provider_name = selection.provider.name().to_string();
        runtime_provider = Some(selection.provider.clone());
        settings = Some(provider_settings);
        (provider, selection.model, provider_name)
    };

    warn_if_extensions_unavailable(&cli);
    let mut config = SessionPrintModeConfig::new(prompt, model, cwd, provider)
        .with_output(cli.output_format.into())
        .with_session_path(cli.session.clone())
        .with_extension_paths(extension_paths_from_cli(&cli));
    config.provider_name = provider_name;
    config.provider_settings = settings;
    config.runtime_provider_config = runtime_provider;
    // `Box::pin` keeps this future off the stack: the print-mode config grew
    // past clippy's `large_futures` threshold once it carried extension paths.
    Ok(Box::pin(run_session_print_mode(config)).await)
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
fn render_provider_settings(
    settings: &ProviderSettings,
    credentials: Option<&FileCredentialStore>,
) {
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
    let destination =
        resolve_export_destination(output.as_deref(), &session_path, &normalized_format);
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
        return Ok((candidate, format!("Rho session {stem}")));
    }
    let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
    let record = manager
        .get_session(session_ref)
        .map_err(|err| err.0)?
        .ok_or_else(|| format!("Unknown session or file: {session_ref}"))?;
    let title = record
        .title
        .clone()
        .unwrap_or_else(|| format!("Rho session {}", record.id));
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
        Some(dir) => rho_coding::session_export::default_session_export_artifact_path(
            session_path,
            dir,
            format,
        ),
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
        // Whole number: render with no fractional part (avoids a lossy cast).
        return format!("{value:.0}");
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

#[cfg(test)]
mod startup_selection_tests {
    use super::*;
    use rho_coding::provider_config::builtin_provider_configs;
    use std::collections::HashMap;

    /// In-memory credential reader for hermetic selection tests.
    struct MapCreds(HashMap<String, String>);

    impl CredentialReader for MapCreds {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        fn get_oauth(&self, _name: &str) -> Option<String> {
            None
        }
    }

    fn settings() -> ProviderSettings {
        ProviderSettings {
            default_provider: "openai".to_string(),
            providers: builtin_provider_configs(),
            scoped_models: Vec::new(),
        }
    }

    #[test]
    fn explicit_provider_and_model_are_honored_verbatim() {
        let settings = settings();
        // Credentials are irrelevant on the explicit path (no fallback applies).
        let selection =
            resolve_tui_startup_selection(&settings, Some("openai"), Some("gpt-4o"), None).unwrap();
        assert_eq!(selection.provider.name(), "openai");
        assert_eq!(selection.model, "gpt-4o");
    }

    #[test]
    fn default_selection_is_kept_when_it_has_a_usable_credential() {
        let settings = settings();
        let creds = MapCreds(HashMap::from([(
            "openai".to_string(),
            "sk-test".to_string(),
        )]));
        // A usable default is returned regardless of ambient env: no fallback.
        let selection = resolve_tui_startup_selection(&settings, None, None, Some(&creds)).unwrap();
        assert_eq!(selection.provider.name(), "openai");
        assert_eq!(
            selection.model,
            settings
                .get_provider(Some("openai"))
                .unwrap()
                .default_model()
        );
    }
}
