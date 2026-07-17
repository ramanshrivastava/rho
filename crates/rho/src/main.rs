//! The `rho` binary — a full-parity Rust port of the `tau` coding agent.
//!
//! M4a wires the first runnable vertical slice: non-interactive print mode
//! (`rho -p`) driving the real coding tools against a provider selected from the
//! environment, or the scripted `FakeProvider` (`--fake` / `RHO_FAKE=1`) for an
//! offline end-to-end demo. Interactive TUI mode and the full provider catalog
//! land in M5 / M4b respectively; this binary stubs the no-prompt path.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use rho_agent::messages::{AssistantContent, AssistantMessage, TextContent, ToolCall};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
    ToolCallEndEvent,
};
use rho_ai::{AnthropicConfig, AnthropicProvider, FakeProvider, OpenAICompatibleProvider};
use rho_coding::{PrintModeConfig, PrintOutputMode, run_print_mode};

/// A minimalist Pi-style coding-agent harness (Rust port of tau).
#[derive(Debug, Parser)]
#[command(name = "rho", version, about, long_about = None)]
struct Cli {
    /// Run a single prompt in non-interactive print mode.
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT")]
    prompt: Option<String>,

    /// Working directory for the built-in coding tools.
    #[arg(long = "cwd", value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Output format for print mode.
    #[arg(
        short = 'o',
        long = "output-format",
        value_name = "FORMAT",
        default_value = "text"
    )]
    output_format: OutputFormat,

    /// Model to request from the provider.
    #[arg(short = 'm', long = "model", value_name = "MODEL")]
    model: Option<String>,

    /// Use the scripted `FakeProvider` (offline demo; ignores real API keys).
    #[arg(long = "fake")]
    fake: bool,
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

    let Some(prompt) = cli.prompt.clone() else {
        // Interactive TUI mode is M5; the no-prompt path is a stub for now.
        eprintln!(
            "rho {}: interactive mode is not implemented yet (M5). Use -p to run a prompt.",
            env!("CARGO_PKG_VERSION")
        );
        std::process::exit(2);
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Error: failed to start async runtime: {err}");
            std::process::exit(1);
        }
    };

    let ok = runtime.block_on(async move { run(cli, prompt).await });
    if !ok {
        std::process::exit(1);
    }
}

async fn run(cli: Cli, prompt: String) -> bool {
    let cwd = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let use_fake = cli.fake || std::env::var("RHO_FAKE").is_ok_and(|v| v == "1");
    let (provider, model): (Arc<dyn ModelProvider>, String) = if use_fake {
        (
            Arc::new(demo_fake_provider()),
            cli.model.unwrap_or_else(|| "fake".to_string()),
        )
    } else {
        match select_provider(cli.model.clone()) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("Error: {err}");
                return false;
            }
        }
    };

    let config =
        PrintModeConfig::new(prompt, model, cwd, provider).with_output(cli.output_format.into());
    run_print_mode(config).await
}

/// Minimal env-based provider selection (full catalog is M4b): `ANTHROPIC_API_KEY`
/// → Anthropic, else `OPENAI_API_KEY` → OpenAI-compatible.
fn select_provider(model: Option<String>) -> Result<(Arc<dyn ModelProvider>, String), String> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            let provider = AnthropicProvider::new(AnthropicConfig::new(key));
            let model = model
                .or_else(|| std::env::var("ANTHROPIC_MODEL").ok())
                .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
            return Ok((Arc::new(provider), model));
        }
    }
    match rho_ai::openai_compatible_config_from_env() {
        Ok(config) => {
            let provider = OpenAICompatibleProvider::new(config);
            let model = model
                .or_else(|| std::env::var("OPENAI_MODEL").ok())
                .unwrap_or_else(|| "gpt-4o".to_string());
            Ok((Arc::new(provider), model))
        }
        Err(_) => Err(
            "No provider configured. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or pass --fake for \
an offline demo."
                .to_string(),
        ),
    }
}

/// A scripted [`FakeProvider`] that showcases the whole slice offline: turn 1
/// calls the real `bash` tool (`ls -la`), turn 2 answers in text. The streams
/// are prompt-independent (the fake replays them in order).
fn demo_fake_provider() -> FakeProvider {
    let model = "fake";

    // Turn 1: a single bash tool call.
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

    // Turn 2: a text answer.
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
