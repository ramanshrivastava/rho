//! Ported from tau `tests/test_pi_event_protocol.py` (the harness/FakeProvider
//! event-protocol cases).
//!
//! The two import-cycle assertions in the Python file
//! (`test_tau_agent_does_not_import_tau_ai`,
//! `test_tau_ai_reexports_canonical_event_classes`) are **not** ported: Cargo's
//! acyclic crate graph makes `rho-agent` structurally incapable of depending on
//! `rho-ai`, so the property they guard holds by construction (see
//! `dev-notes/phase-1.md`). The wire shapes they exercise are covered by the
//! event-stream goldens.

use std::sync::Arc;

use futures::StreamExt;

use rho_agent::events::AgentEvent;
use rho_agent::fake::FakeProvider;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolCall,
};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
    TextEndEvent, TextStartEvent, ToolCallEndEvent, ToolCallStartEvent,
};
use rho_agent::types::JsonMap;

async fn collect(mut stream: rho_agent::harness::EventStream) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn done(reason: DoneReason, message: AssistantMessage) -> AssistantMessageEvent {
    AssistantMessageEvent::Done(AssistantDoneEvent::new(reason, message))
}

#[tokio::test]
async fn text_stream_has_nested_updates_and_terminal_messages() {
    let empty = AssistantMessage::new(Vec::new()).with_model("fake");
    let hello = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("hello"))])
        .with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(empty.clone())),
        AssistantMessageEvent::TextStart(TextStartEvent::new(0, empty)),
        AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, "hello", hello.clone())),
        AssistantMessageEvent::TextEnd(TextEndEvent::new(0, "hello", hello.clone())),
        done(DoneReason::Stop, hello.clone()),
    ]]));
    let harness = AgentHarness::new(
        AgentHarnessConfig::new(provider, "fake", "test"),
        Vec::new(),
    );

    let events = collect(harness.prompt("hi").unwrap()).await;

    assert_eq!(
        events
            .iter()
            .map(AgentEvent::event_type)
            .collect::<Vec<_>>(),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_update",
            "message_update",
            "message_update",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let update_types: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageUpdate(u) => Some(match &u.assistant_message_event {
                AssistantMessageEvent::TextStart(_) => "text_start",
                AssistantMessageEvent::TextDelta(_) => "text_delta",
                AssistantMessageEvent::TextEnd(_) => "text_end",
                _ => "other",
            }),
            _ => None,
        })
        .collect();
    assert_eq!(update_types, ["text_start", "text_delta", "text_end"]);

    let AgentEvent::AgentEnd(end) = events.last().unwrap() else {
        panic!("expected agent_end last");
    };
    assert!(matches!(end.messages.last(), Some(AgentMessage::Assistant(a)) if *a == hello));
}

#[tokio::test]
async fn tool_result_gets_execution_and_message_lifecycle_events() {
    let call = ToolCall::new("call-1", "missing", JsonMap::new());
    let partial =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let done_msg = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("done"))])
        .with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![
        vec![
            AssistantMessageEvent::Start(AssistantStartEvent::new(
                AssistantMessage::new(Vec::new()).with_model("fake"),
            )),
            AssistantMessageEvent::ToolCallStart(ToolCallStartEvent::new(0, partial.clone())),
            AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(0, call, partial.clone())),
            done(DoneReason::ToolUse, partial),
        ],
        vec![
            AssistantMessageEvent::Start(AssistantStartEvent::new(
                AssistantMessage::new(Vec::new()).with_model("fake"),
            )),
            done(DoneReason::Stop, done_msg),
        ],
    ]));
    let harness = AgentHarness::new(
        AgentHarnessConfig::new(provider, "fake", "test"),
        Vec::new(),
    );

    let events = collect(harness.prompt("use it").unwrap()).await;

    let start = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionStart(e) => Some(e.clone()),
            _ => None,
        })
        .unwrap();
    let end = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd(e) => Some(e.clone()),
            _ => None,
        })
        .unwrap();
    let result_id = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::MessageEnd(m) => match &m.message {
                AgentMessage::ToolResult(t) => Some(t.tool_call_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(start.tool_call_id, end.tool_call_id);
    assert_eq!(end.tool_call_id, result_id);
    assert!(end.is_error);
}
