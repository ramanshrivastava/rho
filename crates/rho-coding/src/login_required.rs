//! Placeholder provider used so the TUI can open before login (tau
//! `LoginRequiredProvider`, `tau_coding/tui/app.py`).
//!
//! When no usable credential exists for the resolved startup provider, the TUI
//! substitutes this placeholder for the real provider so the app can still
//! launch instead of aborting at the CLI. Any attempt to stream a response
//! surfaces a single polite `AssistantErrorEvent` carrying the login-required
//! message rather than crashing — the user then runs `/login` or picks a
//! credentialed provider/model, which swaps in a real provider via
//! [`CodingSession::set_model_choice`](crate::session::CodingSession::set_model_choice).

use std::sync::Arc;

use futures::stream::{self, StreamExt};

use rho_agent::messages::{AgentMessage, AssistantMessage, StopReason};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::provider_events::{AssistantErrorEvent, AssistantMessageEvent, ErrorReason};
use rho_agent::tools::AgentTool;

/// A provider that yields only a login-required error (tau `LoginRequiredProvider`).
///
/// Holds no client and performs no I/O; it exists purely so the harness has a
/// valid [`ModelProvider`] to run against until a real one is swapped in.
#[derive(Debug, Clone)]
pub struct LoginRequiredProvider {
    message: String,
}

impl LoginRequiredProvider {
    /// Build a placeholder that reports `message` on every request.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The login-required message this placeholder surfaces.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl ModelProvider for LoginRequiredProvider {
    fn stream_response(
        &self,
        model: &str,
        _system: &str,
        _messages: &[AgentMessage],
        _tools: &[AgentTool],
        _signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        // Mirror tau: yield a single errored assistant message carrying the
        // login prompt (empty content, `stop_reason = "error"`).
        let error = AssistantMessage::new(Vec::new())
            .with_model(model)
            .with_stop_reason(StopReason::Error)
            .with_error_message(self.message.clone());
        let event =
            AssistantMessageEvent::Error(AssistantErrorEvent::new(ErrorReason::Error, error));
        stream::once(async move { event }).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &str = "Login required. Run /login to choose a provider, \
         or /login openai to continue with the current provider.";

    #[tokio::test]
    async fn stream_response_yields_a_single_login_error() {
        let provider = LoginRequiredProvider::new(MESSAGE);
        let events: Vec<AssistantMessageEvent> = provider
            .stream_response("gpt-4", "system", &[], &[], None)
            .collect()
            .await;

        assert_eq!(events.len(), 1, "exactly one event is emitted");
        let AssistantMessageEvent::Error(error) = &events[0] else {
            panic!("expected an error event, got {:?}", events[0]);
        };
        assert_eq!(error.reason, ErrorReason::Error);
        assert_eq!(error.error.stop_reason, StopReason::Error);
        assert_eq!(error.error.model, "gpt-4");
        assert_eq!(error.error.error_message.as_deref(), Some(MESSAGE));
    }

    #[test]
    fn message_accessor_returns_the_prompt() {
        assert_eq!(LoginRequiredProvider::new(MESSAGE).message(), MESSAGE);
    }
}
