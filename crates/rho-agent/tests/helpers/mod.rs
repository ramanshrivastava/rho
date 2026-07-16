//! Canonical Pi event constructors (port of tau `tests/pi_event_helpers.py`).
//!
//! Shared by the `agent_loop`, `agent_harness`, and `pi_event_protocol` test
//! binaries. `assistant_done` reproduces the helper's finish-reason mapping.

#![allow(dead_code)]

use rho_agent::messages::{
    AssistantContent, AssistantMessage, StopReason, ThinkingContent, ToolCall,
};
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantErrorEvent, AssistantMessageEvent, AssistantStartEvent,
    DoneReason, ErrorReason, TextDeltaEvent, ThinkingDeltaEvent, ToolCallEndEvent,
};

pub fn assistant_start() -> AssistantMessageEvent {
    AssistantMessageEvent::Start(AssistantStartEvent::new(
        AssistantMessage::new(Vec::new()).with_model("fake"),
    ))
}

pub fn text_delta(delta: &str) -> AssistantMessageEvent {
    let partial = AssistantMessage::new(vec![AssistantContent::Text(
        rho_agent::messages::TextContent::new(delta),
    )]);
    AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, delta, partial))
}

pub fn thinking_delta(delta: &str) -> AssistantMessageEvent {
    let partial = AssistantMessage::new(vec![AssistantContent::Thinking(thinking(delta))]);
    AssistantMessageEvent::ThinkingDelta(ThinkingDeltaEvent::new(0, delta, partial))
}

fn thinking(text: &str) -> ThinkingContent {
    // ThinkingContent has no public constructor; build via JSON (wire shape).
    serde_json::from_value(serde_json::json!({"type": "thinking", "thinking": text}))
        .expect("thinking content")
}

pub fn tool_call_end(tool_call: ToolCall) -> AssistantMessageEvent {
    let partial = AssistantMessage::new(vec![AssistantContent::ToolCall(tool_call.clone())]);
    AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(0, tool_call, partial))
}

/// `assistant_done(message)` — infers `stop` unless the message carries tool
/// calls (then `toolUse`), matching the helper's default path.
pub fn assistant_done(message: AssistantMessage) -> AssistantMessageEvent {
    let reason = if message.tool_calls().is_empty() {
        DoneReason::Stop
    } else {
        DoneReason::ToolUse
    };
    finish(message, reason)
}

/// `assistant_done(message, finish_reason)` — maps the finish reason string as
/// tau's helper does (tool-ish → `toolUse`, length-ish → `length`, else `stop`).
pub fn assistant_done_reason(
    message: AssistantMessage,
    finish_reason: &str,
) -> AssistantMessageEvent {
    let reason = if !message.tool_calls().is_empty()
        || matches!(finish_reason, "tool_calls" | "tool_use" | "toolUse")
    {
        DoneReason::ToolUse
    } else if matches!(
        finish_reason,
        "length" | "max_tokens" | "MAX_TOKENS" | "incomplete"
    ) {
        DoneReason::Length
    } else {
        DoneReason::Stop
    };
    finish(message, reason)
}

fn finish(mut message: AssistantMessage, reason: DoneReason) -> AssistantMessageEvent {
    message.stop_reason = match reason {
        DoneReason::Stop => StopReason::Stop,
        DoneReason::Length => StopReason::Length,
        DoneReason::ToolUse => StopReason::ToolUse,
    };
    AssistantMessageEvent::Done(AssistantDoneEvent::new(reason, message))
}

pub fn assistant_error(message: &str) -> AssistantMessageEvent {
    let error = AssistantMessage::new(Vec::new())
        .with_stop_reason(StopReason::Error)
        .with_error_message(message);
    AssistantMessageEvent::Error(AssistantErrorEvent::new(ErrorReason::Error, error))
}
