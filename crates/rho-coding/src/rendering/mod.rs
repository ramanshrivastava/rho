//! Event renderers for rho coding frontends and print modes (port of tau's
//! `tau_coding/rendering/`). Three modes: `text` (final message only), `json`
//! (canonical event stream), and `transcript` (live human-readable stream).

mod json;
mod plain;
mod tool_call;
mod transcript;

pub use json::JsonEventRenderer;
pub use plain::FinalTextRenderer;
pub use transcript::TranscriptRenderer;

use std::io::Write;

use crate::events::CodingSessionEvent;

/// A byte sink a renderer writes to (real stdout/stderr in production; a shared
/// buffer in tests). tau writes to typer/`rich` globals; rho injects the sink so
/// the ported `capsys` tests can observe output.
pub type Sink = Box<dyn Write + Send>;

/// The default stdout sink.
#[must_use]
pub fn stdout_sink() -> Sink {
    Box::new(std::io::stdout())
}

/// The default stderr sink.
#[must_use]
pub fn stderr_sink() -> Sink {
    Box::new(std::io::stderr())
}

/// Output modes supported by non-interactive print mode (tau `PrintOutputMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintOutputMode {
    /// Print only the final assistant text.
    #[default]
    Text,
    /// Emit the canonical JSON event stream.
    Json,
    /// Stream a live human-readable transcript.
    Transcript,
}

/// Consumes agent events and renders them for an output mode (tau
/// `EventRenderer`).
pub trait EventRenderer {
    /// Render one event.
    fn render(&mut self, event: &CodingSessionEvent);
    /// Finish rendering; returns whether the run succeeded.
    fn finish(&mut self) -> bool;
}

/// Create a renderer for a print output mode (tau `create_event_renderer`).
#[must_use]
pub fn create_event_renderer(mode: PrintOutputMode) -> Box<dyn EventRenderer> {
    match mode {
        PrintOutputMode::Text => Box::new(FinalTextRenderer::new()),
        PrintOutputMode::Json => Box::new(JsonEventRenderer::new()),
        PrintOutputMode::Transcript => Box::new(TranscriptRenderer::new()),
    }
}

#[cfg(test)]
mod tests;
