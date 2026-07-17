//! The M4a non-interactive print-mode vertical slice (a thin port of tau's
//! `cli.run_print_mode` core path).
//!
//! This builds the coding tools + system prompt, drives an [`AgentHarness`] with
//! one prompt, and renders the harness event stream through the selected
//! renderer. It deliberately does **not** build a `CodingSession`: session
//! persistence, slash/terminal commands, project-context discovery, skills, and
//! extensions are M4b. The event stream it renders is therefore exactly the
//! harness stream — which is what the crosscheck oracle pins.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::provider::ModelProvider;

use crate::events::CodingSessionEvent;
use crate::rendering::{PrintOutputMode, create_event_renderer};
use crate::system_prompt::{BuildSystemPromptOptions, build_system_prompt};
use crate::tools::create_coding_tools;

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
