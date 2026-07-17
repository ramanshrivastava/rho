//! Pi-compatible JSON event-stream renderer (tau `rendering/json.py`).

use std::io::Write;

use rho_agent::events::AgentEvent;
use rho_agent::messages::{AgentMessage, StopReason};

use super::{EventRenderer, Sink, stderr_sink, stdout_sink};
use crate::events::CodingSessionEvent;

/// Emits one canonical JSON object per event (tau `JsonEventRenderer`):
/// `model_dump_json(by_alias=True, exclude_none=True)` == serde `to_string`.
pub struct JsonEventRenderer {
    out: Sink,
    err: Sink,
    failed: bool,
}

impl JsonEventRenderer {
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
            failed: false,
        }
    }
}

impl Default for JsonEventRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRenderer for JsonEventRenderer {
    fn render(&mut self, event: &CodingSessionEvent) {
        if let CodingSessionEvent::Agent(AgentEvent::MessageEnd(end)) = event {
            if let AgentMessage::Assistant(message) = &end.message {
                if message.stop_reason == StopReason::Error {
                    self.failed = true;
                }
            }
        }
        match serde_json::to_string(event) {
            Ok(line) => {
                let _ = writeln!(self.out, "{line}");
                let _ = self.out.flush();
            }
            Err(err) => {
                let _ = writeln!(self.err, "Error: {err}");
            }
        }
    }

    fn finish(&mut self) -> bool {
        !self.failed
    }
}
