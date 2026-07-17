//! Pi-style final-text renderer for print mode (tau `rendering/plain.py`).

use std::io::Write;

use rho_agent::events::AgentEvent;
use rho_agent::messages::{AgentMessage, StopReason};

use super::{EventRenderer, Sink, stderr_sink, stdout_sink};
use crate::events::CodingSessionEvent;

/// Prints only the last assistant message's text on success; errors go to
/// stderr at [`finish`](FinalTextRenderer::finish) (tau `FinalTextRenderer`).
pub struct FinalTextRenderer {
    out: Sink,
    err: Sink,
    last_assistant_text: String,
    failed: bool,
    error_messages: Vec<String>,
}

impl FinalTextRenderer {
    /// Build a renderer writing to real stdout/stderr.
    #[must_use]
    pub fn new() -> Self {
        Self::with_sinks(stdout_sink(), stderr_sink())
    }

    /// Build a renderer writing to the given sinks (tests).
    #[must_use]
    pub fn with_sinks(out: Sink, err: Sink) -> Self {
        Self {
            out,
            err,
            last_assistant_text: String::new(),
            failed: false,
            error_messages: Vec::new(),
        }
    }
}

impl Default for FinalTextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRenderer for FinalTextRenderer {
    fn render(&mut self, event: &CodingSessionEvent) {
        let CodingSessionEvent::Agent(AgentEvent::MessageEnd(end)) = event else {
            return;
        };
        let AgentMessage::Assistant(message) = &end.message else {
            return;
        };
        self.last_assistant_text = message.text();
        if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
            self.failed = message.stop_reason == StopReason::Error;
            // tau appends only a truthy `error_message` (an empty string is
            // falsy and produces no "Error: …" line).
            if let Some(error) = &message.error_message {
                if !error.is_empty() {
                    self.error_messages.push(error.clone());
                }
            }
        }
    }

    fn finish(&mut self) -> bool {
        if self.failed {
            for message in &self.error_messages {
                let _ = writeln!(self.err, "Error: {message}");
            }
            let _ = self.err.flush();
            return false;
        }
        if !self.last_assistant_text.is_empty() {
            let _ = writeln!(self.out, "{}", self.last_assistant_text);
            let _ = self.out.flush();
        }
        true
    }
}
