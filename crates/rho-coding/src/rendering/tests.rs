//! Ported from tau `tests/test_rendering.py`. Output is captured through the
//! injectable [`Sink`] seam (rho's analogue of pytest's `capsys`).

use std::io::Write;
use std::sync::{Arc, Mutex};

use rho_agent::events::{
    AgentEvent, MessageEndEvent, MessageStartEvent, MessageUpdateEvent, ToolExecutionEndEvent,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent,
};
use rho_agent::messages::{AgentMessage, AssistantMessage, StopReason, TextContent};
use rho_agent::provider_events::{AssistantMessageEvent, TextDeltaEvent, ThinkingDeltaEvent};
use rho_agent::tools::AgentToolResult;
use rho_agent::types::JsonMap;

use super::*;
use crate::events::{AutoRetryStartEvent, CodingSessionEvent, QueueUpdateEvent, SessionOwnEvent};

#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl SharedBuffer {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sinks() -> (SharedBuffer, SharedBuffer, Sink, Sink) {
    let out = SharedBuffer::default();
    let err = SharedBuffer::default();
    (out.clone(), err.clone(), Box::new(out), Box::new(err))
}

fn args(value: serde_json::Value) -> JsonMap {
    match value {
        serde_json::Value::Object(m) => m,
        _ => JsonMap::new(),
    }
}

fn assistant_update(event: AssistantMessageEvent) -> CodingSessionEvent {
    let message = AgentMessage::Assistant(event.partial().clone());
    CodingSessionEvent::Agent(AgentEvent::MessageUpdate(MessageUpdateEvent::new(
        message, event,
    )))
}

fn error_end(message: &str) -> CodingSessionEvent {
    let assistant = AssistantMessage::new(Vec::new())
        .with_stop_reason(StopReason::Error)
        .with_error_message(message);
    CodingSessionEvent::Agent(AgentEvent::MessageEnd(MessageEndEvent::new(
        AgentMessage::Assistant(assistant),
    )))
}

#[test]
fn transcript_renderer_streams_text_and_tool_events() {
    let (out, err, out_sink, err_sink) = sinks();
    let mut renderer = TranscriptRenderer::with_sinks(out_sink, err_sink);
    let partial = AssistantMessage::new(Vec::new());

    renderer.render(&CodingSessionEvent::Agent(AgentEvent::MessageStart(
        MessageStartEvent::new(AgentMessage::Assistant(partial.clone())),
    )));
    renderer.render(&assistant_update(AssistantMessageEvent::ThinkingDelta(
        ThinkingDeltaEvent::new(0, "hidden reasoning", partial.clone()),
    )));
    renderer.render(&assistant_update(AssistantMessageEvent::TextDelta(
        TextDeltaEvent::new(0, "Hel", partial.clone()),
    )));
    renderer.render(&assistant_update(AssistantMessageEvent::TextDelta(
        TextDeltaEvent::new(0, "lo", partial.clone()),
    )));
    renderer.render(&CodingSessionEvent::Session(
        SessionOwnEvent::AutoRetryStart(AutoRetryStartEvent::new(
            2,
            3,
            0,
            "Retrying provider request 2/3 after HTTP 503.".into(),
        )),
    ));
    renderer.render(&CodingSessionEvent::Agent(AgentEvent::ToolExecutionStart(
        ToolExecutionStartEvent::new("call-1", "read", args(serde_json::json!({"path": "a.py"}))),
    )));
    renderer.render(&CodingSessionEvent::Agent(AgentEvent::ToolExecutionUpdate(
        ToolExecutionUpdateEvent::new(
            "call-1",
            "read",
            args(serde_json::json!({"path": "a.py"})),
            AgentToolResult::new(vec![rho_agent::messages::ToolResultContent::Text(
                TextContent::new("reading"),
            )]),
        ),
    )));
    renderer.render(&CodingSessionEvent::Agent(AgentEvent::ToolExecutionEnd(
        ToolExecutionEndEvent::new(
            "call-1",
            "read",
            AgentToolResult::new(vec![rho_agent::messages::ToolResultContent::Text(
                TextContent::new("done"),
            )]),
            false,
        ),
    )));

    assert!(renderer.finish());
    assert_eq!(out.contents(), "Hello\n");
    let err = err.contents();
    assert!(!err.contains("hidden reasoning"));
    assert!(err.contains("… Retrying provider request 2/3 after HTTP 503."));
    assert!(err.contains("→ read a.py"));
    assert!(err.contains("… reading"));
    assert!(err.contains("✓ read"));
    assert!(err.contains("done"));
}

#[test]
fn transcript_renderer_fails_on_assistant_error() {
    let (_out, err, out_sink, err_sink) = sinks();
    let mut renderer = TranscriptRenderer::with_sinks(out_sink, err_sink);
    renderer.render(&error_end("provider failed"));
    assert!(!renderer.finish());
    assert!(err.contents().contains("Error: provider failed"));
}

#[test]
fn final_text_renderer_prints_only_final_message() {
    let (out, _err, out_sink, err_sink) = sinks();
    let mut renderer = FinalTextRenderer::with_sinks(out_sink, err_sink);
    let partial = AssistantMessage::new(vec![rho_agent::messages::AssistantContent::Text(
        TextContent::new("ignored"),
    )]);

    renderer.render(&assistant_update(AssistantMessageEvent::TextDelta(
        TextDeltaEvent::new(0, "ignored", partial),
    )));
    assert!(renderer.finish());
    assert_eq!(out.contents(), "");

    let final_msg = AssistantMessage::new(vec![rho_agent::messages::AssistantContent::Text(
        TextContent::new("Final answer"),
    )]);
    renderer.render(&CodingSessionEvent::Agent(AgentEvent::MessageEnd(
        MessageEndEvent::new(AgentMessage::Assistant(final_msg)),
    )));
    assert!(renderer.finish());
    assert_eq!(out.contents(), "Final answer\n");
}

#[test]
fn final_text_renderer_prints_errors_on_finish() {
    let (out, err, out_sink, err_sink) = sinks();
    let mut renderer = FinalTextRenderer::with_sinks(out_sink, err_sink);
    renderer.render(&error_end("provider failed"));
    assert_eq!(err.contents(), "");
    assert!(!renderer.finish());
    assert!(err.contents().contains("Error: provider failed"));
    assert_eq!(out.contents(), "");
}

#[test]
fn json_renderer_emits_canonical_jsonl() {
    let (out, _err, out_sink, err_sink) = sinks();
    let mut renderer = JsonEventRenderer::with_sinks(out_sink, err_sink);
    let partial = AssistantMessage::new(vec![rho_agent::messages::AssistantContent::Text(
        TextContent::new("hidden reasoning"),
    )]);

    renderer.render(&CodingSessionEvent::Agent(AgentEvent::MessageStart(
        MessageStartEvent::new(AgentMessage::Assistant(AssistantMessage::new(Vec::new()))),
    )));
    renderer.render(&CodingSessionEvent::Session(SessionOwnEvent::QueueUpdate(
        QueueUpdateEvent::new(vec!["adjust".into()], vec!["after".into()]),
    )));
    renderer.render(&assistant_update(AssistantMessageEvent::ThinkingDelta(
        ThinkingDeltaEvent::new(0, "hidden reasoning", partial),
    )));
    renderer.render(&error_end("provider failed"));

    let lines: Vec<serde_json::Value> = out
        .contents()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "message_start");
    assert_eq!(
        lines[1],
        serde_json::json!({"type": "queue_update", "steering": ["adjust"], "followUp": ["after"]})
    );
    assert_eq!(lines[2]["type"], "message_update");
    assert_eq!(lines[2]["assistantMessageEvent"]["type"], "thinking_delta");
    assert_eq!(lines[3]["message"]["stopReason"], "error");
    assert!(!renderer.finish());
}
