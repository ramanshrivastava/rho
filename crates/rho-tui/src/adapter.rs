//! Translate Pi-compatible session events into TUI display state (port of tau
//! `tau_coding/tui/adapter.py`).
//!
//! This is *the* seam between the session/harness event stream and the UI: it is
//! the only mutator of [`TuiState`] driven by events, so its assistant-buffer
//! flush rules, `agent_settled` handling, and error/abort mapping must match tau
//! exactly. Ported 1:1 and covered by the transferred `test_tui_adapter.py`.

use rho_agent::events::AgentEvent;
use rho_agent::messages::AgentMessage;
use rho_agent::messages::StopReason;
use rho_agent::provider_events::AssistantMessageEvent;
use rho_coding::events::{CodingSessionEvent, SessionOwnEvent};

use crate::state::{ChatItemRole, TuiState};

/// Applies session events to a [`TuiState`] (tau `TuiEventAdapter`).
pub struct TuiEventAdapter<'a> {
    state: &'a mut TuiState,
}

impl<'a> TuiEventAdapter<'a> {
    /// Wrap a mutable state reference.
    pub fn new(state: &'a mut TuiState) -> Self {
        Self { state }
    }

    /// Apply one coding-session event (tau `apply`).
    pub fn apply(&mut self, event: &CodingSessionEvent) {
        match event {
            CodingSessionEvent::Agent(agent_event) => self.apply_agent(agent_event),
            CodingSessionEvent::Session(session_event) => self.apply_session(session_event),
        }
    }

    fn apply_agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart(_) => {
                self.state.running = true;
                self.state.error = None;
            }
            AgentEvent::AgentEnd(_) => {
                self.flush();
                self.state.running = false;
            }
            AgentEvent::MessageStart(e) => {
                if let AgentMessage::Assistant(message) = &e.message {
                    self.state.assistant_buffer = message.text();
                }
            }
            AgentEvent::MessageUpdate(e) => match &e.assistant_message_event {
                AssistantMessageEvent::TextDelta(delta) => {
                    self.state.assistant_buffer.push_str(&delta.delta);
                }
                AssistantMessageEvent::ThinkingDelta(delta) => {
                    self.state.add_thinking_delta(&delta.delta);
                }
                _ => {}
            },
            AgentEvent::MessageEnd(e) => self.apply_message_end(&e.message),
            AgentEvent::ToolExecutionStart(e) => {
                self.flush();
                let tool_call = rho_agent::messages::ToolCall::new(
                    e.tool_call_id.clone(),
                    e.tool_name.clone(),
                    e.args.clone(),
                );
                self.state.add_tool_call(&tool_call);
            }
            AgentEvent::ToolExecutionUpdate(e) => {
                self.state
                    .record_tool_update(&e.tool_call_id, &e.partial_result.text());
            }
            AgentEvent::ToolExecutionEnd(e) => {
                self.state.record_tool_result(
                    &e.tool_call_id,
                    &e.tool_name,
                    e.result.clone(),
                    e.is_error,
                );
            }
            AgentEvent::TurnStart(_) | AgentEvent::TurnEnd(_) => {}
        }
    }

    fn apply_session(&mut self, event: &SessionOwnEvent) {
        match event {
            SessionOwnEvent::AgentSettled(_) => {
                self.flush();
                self.state.running = false;
            }
            SessionOwnEvent::QueueUpdate(e) => {
                self.state
                    .update_queue(e.steering.clone(), e.follow_up.clone());
            }
            SessionOwnEvent::AutoRetryStart(e) => {
                self.state
                    .add_item(ChatItemRole::Status, format!("… {}", e.error_message));
            }
            _ => {}
        }
    }

    fn apply_message_end(&mut self, message: &AgentMessage) {
        match message {
            AgentMessage::User(m) => {
                let text = m.text();
                // Reconcile against an optimistic echo: if the submit path already
                // rendered this exact user message on the frame the user pressed
                // Enter, consume the pending marker instead of adding a duplicate.
                // Any mismatch falls through to a normal add, so the transcript is
                // never wrong — worst case it is the pre-optimistic behavior.
                if self.state.optimistic_echo.as_deref() == Some(text.as_str()) {
                    self.state.optimistic_echo = None;
                } else {
                    self.state.add_user_message(&text, None, None);
                }
            }
            AgentMessage::Custom(m) => {
                let details = match &m.details {
                    Some(serde_json::Value::Object(_)) => m.details.clone(),
                    _ => None,
                };
                self.state
                    .add_user_message(&m.text(), Some(&m.custom_type), details);
            }
            AgentMessage::Assistant(m) => {
                if matches!(m.stop_reason, StopReason::Error | StopReason::Aborted) {
                    let text = m
                        .error_message
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Error")
                        .to_string();
                    self.state.error = Some(text.clone());
                    self.state.running = false;
                    self.state
                        .add_item(ChatItemRole::Error, format!("Error: {text}"));
                } else {
                    let message_text = m.text();
                    let text = if message_text.is_empty() {
                        self.state.assistant_buffer.clone()
                    } else {
                        message_text
                    };
                    if !text.is_empty() {
                        self.state.add_item(ChatItemRole::Assistant, text);
                    }
                }
                self.state.assistant_buffer.clear();
            }
            AgentMessage::ToolResult(_)
            | AgentMessage::BashExecution(_)
            | AgentMessage::BranchSummary(_)
            | AgentMessage::CompactionSummary(_) => {}
        }
    }

    fn flush(&mut self) {
        if !self.state.assistant_buffer.is_empty() {
            let text = std::mem::take(&mut self.state.assistant_buffer);
            self.state.add_item(ChatItemRole::Assistant, text);
        }
    }
}
