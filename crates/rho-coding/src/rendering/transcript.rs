//! Human-readable streaming transcript renderer (tau `rendering/transcript.py`).
//!
//! Assistant text streams to **stdout**; tool activity, retries, and errors go
//! to **stderr** as plain lines. tau renders stderr through a `rich` `Console`,
//! which strips styling when the stream is not a TTY — the shape the ported
//! tests observe — so rho writes plain text (no ANSI), matching that shape.
//! Custom-message rendering is deferred to M4b (extensions).

use std::io::Write;

use rho_agent::events::AgentEvent;
use rho_agent::messages::{AgentMessage, StopReason, ToolCall};
use rho_agent::provider_events::AssistantMessageEvent;

use super::tool_call::format_tool_call_block;
use super::{EventRenderer, Sink, stderr_sink, stdout_sink};
use crate::events::{CodingSessionEvent, SessionOwnEvent};

/// Streams a compact live transcript (tau `TranscriptRenderer`).
pub struct TranscriptRenderer {
    out: Sink,
    err: Sink,
    assistant_started: bool,
    assistant_ended: bool,
    failed: bool,
}

impl TranscriptRenderer {
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
            assistant_started: false,
            assistant_ended: false,
            failed: false,
        }
    }

    fn newline(&mut self, final_: bool) {
        if self.assistant_started && !self.assistant_ended {
            let _ = writeln!(self.out);
            let _ = self.out.flush();
            self.assistant_ended = true;
        } else if final_ && !self.assistant_started {
            self.assistant_ended = true;
        }
    }
}

impl Default for TranscriptRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRenderer for TranscriptRenderer {
    fn render(&mut self, event: &CodingSessionEvent) {
        match event {
            CodingSessionEvent::Agent(AgentEvent::MessageUpdate(update)) => {
                if let AssistantMessageEvent::TextDelta(delta) = &update.assistant_message_event {
                    self.assistant_started = true;
                    let _ = write!(self.out, "{}", delta.delta);
                    let _ = self.out.flush();
                }
            }
            CodingSessionEvent::Agent(AgentEvent::ToolExecutionStart(start)) => {
                self.newline(false);
                let call = ToolCall::new(
                    start.tool_call_id.clone(),
                    start.tool_name.clone(),
                    start.args.clone(),
                );
                let _ = writeln!(self.err, "{}", format_tool_call_block(&call));
            }
            CodingSessionEvent::Agent(AgentEvent::ToolExecutionUpdate(update)) => {
                self.newline(false);
                let text = update.partial_result.text();
                if !text.is_empty() {
                    let _ = writeln!(self.err, "… {text}");
                }
            }
            CodingSessionEvent::Session(SessionOwnEvent::AutoRetryStart(retry)) => {
                self.newline(false);
                let _ = writeln!(self.err, "… {}", retry.error_message);
            }
            CodingSessionEvent::Agent(AgentEvent::ToolExecutionEnd(end)) => {
                let status = if end.is_error { "✗" } else { "✓" };
                let _ = writeln!(self.err, "{status} {}", end.tool_name);
                let text = end.result.text();
                if !text.is_empty() {
                    // tau uses `str.splitlines()`: a trailing newline yields no
                    // phantom blank line, and `\r\n` leaves no stray `\r`.
                    for line in crate::pystr::splitlines(&text, false) {
                        let _ = writeln!(self.err, "  {line}");
                    }
                }
            }
            CodingSessionEvent::Agent(AgentEvent::MessageEnd(end)) => {
                if let AgentMessage::Assistant(message) = &end.message {
                    if message.stop_reason == StopReason::Error {
                        self.failed = true;
                        self.newline(false);
                        // tau: `error_message or "Error"` — an empty string is
                        // falsy, so it also falls back to "Error".
                        let msg = message
                            .error_message
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Error");
                        let _ = writeln!(self.err, "Error: {msg}");
                    }
                    self.newline(true);
                }
            }
            CodingSessionEvent::Agent(AgentEvent::AgentEnd(_)) => {
                self.newline(true);
            }
            _ => {}
        }
        let _ = self.err.flush();
    }

    fn finish(&mut self) -> bool {
        !self.failed
    }
}
