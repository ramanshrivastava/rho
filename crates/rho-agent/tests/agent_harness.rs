//! Ported from tau `tests/test_agent_harness.py`.
//!
//! The harness is wrapped in an `Arc` so listeners (which mutate the harness via
//! `steer`/`follow_up`) can hold a handle, mirroring tau's closures that close
//! over the harness. `prompt`/`continue_` return the event stream, whose
//! completion updates the shared transcript — so `harness.messages()` reflects a
//! run's appends without reconstruction.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::future::BoxFuture;

use rho_agent::clock::FixedClock;
use rho_agent::events::AgentEvent;
use rho_agent::fake::FakeProvider;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig, QueueMode};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
    UserMessage,
};
use rho_agent::provider::ModelProvider;
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolExecutor, ToolUpdateCallback};
use rho_agent::types::JsonMap;

mod helpers;
use helpers::{assistant_done, assistant_done_reason, assistant_start, text_delta, tool_call_end};

async fn drain(mut stream: rho_agent::harness::EventStream) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::event_type).collect()
}

/// `(role, text)` pairs (port of the test's `_texts`).
fn texts(harness: &AgentHarness) -> Vec<(String, String)> {
    harness
        .messages()
        .iter()
        .map(|m| (m.role().to_string(), m.text()))
        .collect()
}

fn config(provider: Arc<dyn ModelProvider>) -> AgentHarnessConfig {
    AgentHarnessConfig::new(provider, "fake", "You are Tau.")
}

#[tokio::test]
async fn prompt_appends_user_and_assistant_with_pi_lifecycle() {
    let assistant = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Hello"))]);
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        assistant_start(),
        assistant_done(assistant),
    ]]));
    let harness = AgentHarness::new(config(provider), Vec::new());

    let events = drain(harness.prompt("Hi").unwrap()).await;

    assert_eq!(
        event_types(&events),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let start_roles: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart(s) => Some(s.message.role().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(start_roles, ["user", "assistant"]);
    assert_eq!(
        texts(&harness),
        [
            ("user".to_string(), "Hi".to_string()),
            ("assistant".to_string(), "Hello".to_string()),
        ]
    );
}

#[tokio::test]
async fn subscribers_receive_nested_message_updates_and_unsubscribe() {
    let assistant = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Hello"))]);
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![
        vec![
            assistant_start(),
            text_delta("Hello"),
            assistant_done(assistant.clone()),
        ],
        vec![assistant_start(), assistant_done(assistant)],
    ]));
    let harness = AgentHarness::new(config(provider), Vec::new());

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = {
        let seen = seen.clone();
        Arc::new(move |event: &AgentEvent| {
            seen.lock().unwrap().push(event.event_type().to_string());
        })
    };
    let unsubscribe = harness.subscribe(listener);

    let _ = drain(harness.prompt("Hi").unwrap()).await;
    unsubscribe();
    let _ = drain(harness.continue_().unwrap()).await;

    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().any(|t| t == "message_update"));
    assert_eq!(seen.last().map(String::as_str), Some("agent_end"));
}

#[tokio::test]
async fn rejects_overlap_and_drains_followups() {
    let first = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("First"))]);
    let second = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Second"))]);
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![
        vec![assistant_start(), assistant_done(first)],
        vec![assistant_start(), assistant_done(second)],
    ]));
    let harness = Arc::new(AgentHarness::new(config(provider), Vec::new()));

    let mut stream = harness.prompt("Hi").unwrap();
    let mut queued = false;
    while let Some(event) = stream.next().await {
        if let AgentEvent::MessageStart(s) = &event {
            if s.message.role() == "assistant" && !queued {
                // Overlap is rejected.
                assert!(harness.prompt("overlap").is_err());
                harness.follow_up("Later");
                queued = true;
            }
        }
    }

    assert_eq!(
        texts(&harness),
        [
            ("user".to_string(), "Hi".to_string()),
            ("assistant".to_string(), "First".to_string()),
            ("user".to_string(), "Later".to_string()),
            ("assistant".to_string(), "Second".to_string()),
        ]
    );
}

#[tokio::test]
async fn queue_mode_all_drains_messages_together() {
    let first = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("First"))]);
    let second = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Second"))]);
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![
        vec![assistant_start(), assistant_done(first)],
        vec![assistant_start(), assistant_done(second)],
    ]));
    let harness = Arc::new(AgentHarness::new(
        config(provider).with_queue_mode(QueueMode::All),
        Vec::new(),
    ));

    let mut stream = harness.prompt("Hi").unwrap();
    while let Some(event) = stream.next().await {
        if let AgentEvent::MessageEnd(e) = &event {
            if let AgentMessage::Assistant(a) = &e.message {
                if a.text() == "First" {
                    harness.follow_up("Second prompt");
                    harness.follow_up("Third prompt");
                }
            }
        }
    }

    let user_texts: Vec<String> = harness
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::User(u) => Some(u.text()),
            _ => None,
        })
        .collect();
    assert_eq!(user_texts, ["Hi", "Second prompt", "Third prompt"]);
}

#[tokio::test]
async fn passes_canonical_tools_to_loop() {
    let execute: ToolExecutor = Arc::new(
        |_id,
         arguments: JsonMap,
         _signal,
         _on_update: ToolUpdateCallback|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async move {
                let text = arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new(text),
                )]))
            })
        },
    );
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo text.",
        match serde_json::json!({"type": "object"}) {
            serde_json::Value::Object(m) => m,
            _ => JsonMap::new(),
        },
        execute,
    );
    let call = ToolCall::new(
        "call-1",
        "echo",
        match serde_json::json!({"text": "hi"}) {
            serde_json::Value::Object(m) => m,
            _ => JsonMap::new(),
        },
    );
    let first = AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]);
    let final_msg = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Done"))]);
    let fake = FakeProvider::new(vec![
        vec![
            assistant_start(),
            tool_call_end(call),
            assistant_done_reason(first, "toolUse"),
        ],
        vec![assistant_start(), assistant_done(final_msg)],
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(fake.clone());
    let harness = AgentHarness::new(config(provider).with_tools(vec![tool.clone()]), Vec::new());

    let _ = drain(harness.prompt("echo").unwrap()).await;

    let result = harness
        .messages()
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.clone()),
            _ => None,
        })
        .expect("tool result");
    assert_eq!(result.tool_name, "echo");
    assert_eq!(result.text(), "hi");

    // The loop passed the canonical tool through (compared by name — see the
    // journal: `AgentTool` holds an `Arc<dyn Fn>` executor, so it is not `Eq`).
    let call = &fake.calls()[0];
    let names: Vec<&str> = call.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo"]);
}

/// A user message with a pinned timestamp, so equality is deterministic (the
/// harness below is pinned to the same clock).
fn fixed_user(content: &str) -> AgentMessage {
    let mut m = UserMessage::new(content);
    m.timestamp = FixedClock::fixture().ms;
    AgentMessage::User(m)
}

#[test]
fn queue_mutators_return_canonical_snapshots() {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(Vec::new()));
    let harness = AgentHarness::new(
        config(provider).with_clock(Arc::new(FixedClock::fixture())),
        Vec::new(),
    );

    harness.steer("First");
    harness.steer("Second");
    harness.follow_up("Later");
    assert_eq!(harness.pop_latest_steering(), Some(fixed_user("Second")));
    assert_eq!(harness.pop_latest_follow_up(), Some(fixed_user("Later")));
    let steering_texts: Vec<String> = harness
        .queued_messages()
        .steering
        .iter()
        .map(AgentMessage::text)
        .collect();
    assert_eq!(steering_texts, ["First"]);

    let cleared = harness.clear_queues();
    let cleared_texts: Vec<String> = cleared.steering.iter().map(AgentMessage::text).collect();
    assert_eq!(cleared_texts, ["First"]);
    assert_eq!(harness.pending_message_count(), 0);
}

#[test]
fn repairs_interrupted_tool_calls() {
    let call = ToolCall::new(
        "call-1",
        "read",
        match serde_json::json!({"path": "README.md"}) {
            serde_json::Value::Object(m) => m,
            _ => JsonMap::new(),
        },
    );
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(Vec::new()));
    let harness = AgentHarness::new(
        config(provider),
        vec![AgentMessage::Assistant(AssistantMessage::new(vec![
            AssistantContent::Text(TextContent::new("Reading")),
            AssistantContent::ToolCall(call),
        ]))],
    );

    assert_eq!(harness.append_interrupted_tool_results(), 1);
    let repair = harness.messages().last().cloned().unwrap();
    let AgentMessage::ToolResult(repair) = repair else {
        panic!("expected tool result");
    };
    assert!(repair.is_error);
    assert_eq!(repair.text(), "Tool call interrupted by user");
}
