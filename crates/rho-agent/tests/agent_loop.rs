//! Ported from tau `tests/test_agent_loop.py`.
//!
//! Drives [`run_agent_loop`] directly with a [`FakeProvider`]. The shared
//! transcript is `Arc<Mutex<Vec<AgentMessage>>>` (rho's rendering of tau's
//! mutated-in-place `messages` list), so where tau asserts on the passed-in list
//! after the run, these assert on the same shared handle. The `FakeProvider`
//! handle is kept concretely (its clones share the recorded-call state), so
//! `fake.calls()` replaces tau's `provider.calls`.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::future::BoxFuture;

use rho_agent::agent_loop::{AgentLoopConfig, QueueDrain, run_agent_loop};
use rho_agent::clock::SystemClock;
use rho_agent::events::AgentEvent;
use rho_agent::fake::FakeProvider;
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, StopReason, TextContent, ToolCall,
    ToolResultContent, UserMessage,
};
use rho_agent::provider::{CancellationToken, ModelProvider, SimpleCancellationToken};
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolExecutor, ToolUpdateCallback};
use rho_agent::types::JsonMap;

mod helpers;
use helpers::{
    assistant_done, assistant_done_reason, assistant_error, assistant_start, text_delta,
    thinking_delta, tool_call_end,
};

fn shared(messages: Vec<AgentMessage>) -> Arc<Mutex<Vec<AgentMessage>>> {
    Arc::new(Mutex::new(messages))
}

fn base_config(
    provider: Arc<dyn ModelProvider>,
    messages: Arc<Mutex<Vec<AgentMessage>>>,
    tools: Vec<AgentTool>,
) -> AgentLoopConfig {
    AgentLoopConfig {
        provider,
        model: "fake".into(),
        system: "You are Tau.".into(),
        messages,
        tools,
        prompts: Vec::new(),
        max_turns: None,
        signal: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        before_tool_call: None,
        after_tool_call: None,
        clock: Arc::new(SystemClock),
    }
}

async fn collect(config: AgentLoopConfig) -> Vec<AgentEvent> {
    let stream = run_agent_loop(config);
    futures::pin_mut!(stream);
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::event_type).collect()
}

fn object(value: serde_json::Value) -> JsonMap {
    match value {
        serde_json::Value::Object(m) => m,
        _ => JsonMap::new(),
    }
}

fn tool(name: &str, execute: ToolExecutor) -> AgentTool {
    let mut title = name.to_string();
    if let Some(first) = title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    AgentTool::new(
        name,
        title,
        format!("Run {name}."),
        object(serde_json::json!({"type": "object"})),
        execute,
    )
}

fn user_texts(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::User(u) => Some(u.text()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn streams_canonical_nested_events() {
    let messages = shared(vec![AgentMessage::User(UserMessage::new("Say hello"))]);
    let assistant = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Hello"))])
        .with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        assistant_start(),
        text_delta("Hel"),
        text_delta("lo"),
        assistant_done(assistant.clone()),
    ]]));

    let events = collect(base_config(provider, messages.clone(), vec![])).await;

    assert_eq!(
        event_types(&events),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_update",
            "message_update",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let deltas: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageUpdate(u) => match &u.assistant_message_event {
                AssistantMessageEvent::TextDelta(d) => Some(d.delta.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["Hel", "lo"]);

    let final_messages = messages.lock().unwrap().clone();
    assert_eq!(final_messages.len(), 2);
    assert!(matches!(&final_messages[1], AgentMessage::Assistant(a) if *a == assistant));
}

#[tokio::test]
async fn nests_thinking_events_without_losing_final_message() {
    let messages = shared(vec![AgentMessage::User(UserMessage::new("Think briefly"))]);
    let assistant = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Done"))])
        .with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        assistant_start(),
        thinking_delta("hidden "),
        thinking_delta("reasoning"),
        text_delta("Done"),
        assistant_done(assistant.clone()),
    ]]));

    let events = collect(base_config(provider, messages.clone(), vec![])).await;

    let nested: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageUpdate(u) => match &u.assistant_message_event {
                AssistantMessageEvent::ThinkingDelta(d) => Some(d.delta.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(nested, ["hidden ", "reasoning"]);
    assert!(
        matches!(messages.lock().unwrap().last(), Some(AgentMessage::Assistant(a)) if *a == assistant)
    );
}

#[tokio::test]
async fn executes_tool_and_emits_tool_result_message_lifecycle() {
    let execute: ToolExecutor = Arc::new(
        |_id,
         arguments: JsonMap,
         _signal,
         _on_update: ToolUpdateCallback|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async move {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut result = AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new(format!("contents of {path}")),
                )]);
                result.details = Some(serde_json::json!({ "path": path }));
                Ok(result)
            })
        },
    );
    let tool_call = ToolCall::new(
        "call-1",
        "read",
        object(serde_json::json!({"path": "README.md"})),
    );
    let first = AssistantMessage::new(vec![
        AssistantContent::Text(TextContent::new("Reading.")),
        AssistantContent::ToolCall(tool_call.clone()),
    ])
    .with_model("fake");
    let final_msg = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Done."))])
        .with_model("fake");

    let fake = FakeProvider::new(vec![
        vec![
            assistant_start(),
            tool_call_end(tool_call.clone()),
            assistant_done_reason(first, "toolUse"),
        ],
        vec![
            assistant_start(),
            text_delta("Done."),
            assistant_done(final_msg),
        ],
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(fake.clone());
    let messages = shared(vec![AgentMessage::User(UserMessage::new("Read README.md"))]);

    let events = collect(base_config(
        provider,
        messages.clone(),
        vec![tool("read", execute)],
    ))
    .await;

    let final_messages = messages.lock().unwrap().clone();
    let result = final_messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.clone()),
            _ => None,
        })
        .expect("tool result present");
    assert_eq!(result.tool_name, "read");
    assert_eq!(result.text(), "contents of README.md");
    assert_eq!(
        result.details,
        Some(serde_json::json!({"path": "README.md"}))
    );

    assert_eq!(
        event_types(&events)
            .iter()
            .filter(|t| **t == "message_start")
            .count(),
        3
    );

    let calls = fake.calls();
    assert_eq!(calls[1].messages, final_messages[..3]);
}

#[tokio::test]
async fn passes_call_id_signal_and_progress_to_tool() {
    // Record the (call_id, signal) the tool actually received, so we can assert
    // the loop threads through the *same* token (tau asserts token identity).
    type Observed = Vec<(String, Option<Arc<dyn CancellationToken>>)>;
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let observed_clone = observed.clone();
    let execute: ToolExecutor = Arc::new(
        move |id: String,
              _arguments,
              signal: Option<Arc<dyn CancellationToken>>,
              on_update: ToolUpdateCallback|
              -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            let observed = observed_clone.clone();
            Box::pin(async move {
                observed.lock().unwrap().push((id, signal));
                on_update(AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new("working"),
                )]));
                Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new("done"),
                )]))
            })
        },
    );
    let call = ToolCall::new("call-1", "work", JsonMap::new());
    let first =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let final_msg =
        AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("finished"))])
            .with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![
        vec![
            assistant_start(),
            tool_call_end(call),
            assistant_done_reason(first, "toolUse"),
        ],
        vec![assistant_start(), assistant_done(final_msg)],
    ]));

    let signal: Arc<dyn CancellationToken> = Arc::new(SimpleCancellationToken::new());
    let mut config = base_config(
        provider,
        shared(vec![AgentMessage::User(UserMessage::new("work"))]),
        vec![tool("work", execute)],
    );
    config.signal = Some(signal.clone());
    let events = collect(config).await;

    // The tool was called once, with the call id and the *same* signal token the
    // loop was given (identity, not merely presence — tau asserts `== signal`).
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0, "call-1");
    let received = observed[0].1.as_ref().expect("tool received a signal");
    assert!(
        Arc::ptr_eq(received, &signal),
        "tool must receive the exact signal token the loop was given"
    );
    let updates: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionUpdate(u) => Some(u.partial_result.text()),
            _ => None,
        })
        .collect();
    assert_eq!(updates, ["working"]);
}

#[tokio::test]
async fn records_unknown_tool_as_canonical_error_result() {
    let call = ToolCall::new("call-1", "missing", JsonMap::new());
    let assistant =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let messages = shared(vec![AgentMessage::User(UserMessage::new("Use it"))]);
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        assistant_start(),
        tool_call_end(call),
        assistant_done_reason(assistant, "toolUse"),
    ]]));

    let mut config = base_config(provider, messages.clone(), vec![]);
    config.max_turns = Some(1);
    let events = collect(config).await;

    let end = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd(e) => Some(e.clone()),
            _ => None,
        })
        .expect("tool execution end");
    assert!(end.is_error);
    assert_eq!(end.result.text(), "Tool missing not found");
    let result = messages
        .lock()
        .unwrap()
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.clone()),
            _ => None,
        })
        .expect("tool result");
    assert!(result.is_error);
    assert_eq!(result.text(), "Tool missing not found");
}

#[tokio::test]
async fn converts_provider_error_to_assistant_error_message() {
    let messages = shared(vec![AgentMessage::User(UserMessage::new("hello"))]);
    let provider: Arc<dyn ModelProvider> =
        Arc::new(FakeProvider::new(vec![vec![assistant_error(
            "provider failed",
        )]]));

    let events = collect(base_config(provider, messages.clone(), vec![])).await;

    assert_eq!(
        event_types(&events),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let last = messages.lock().unwrap().last().cloned().unwrap();
    let AgentMessage::Assistant(error) = last else {
        panic!("expected assistant error");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("provider failed"));
}

#[tokio::test]
async fn injects_steering_and_follow_up_messages() {
    let call = ToolCall::new("call-1", "work", JsonMap::new());
    let execute: ToolExecutor = Arc::new(
        |_id,
         _arguments,
         _signal,
         _on_update: ToolUpdateCallback|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async move {
                Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new("ok"),
                )]))
            })
        },
    );
    let first =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let second = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("second"))])
        .with_model("fake");
    let third = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("third"))])
        .with_model("fake");
    let fake = FakeProvider::new(vec![
        vec![
            assistant_start(),
            tool_call_end(call),
            assistant_done_reason(first, "toolUse"),
        ],
        vec![assistant_start(), assistant_done(second)],
        vec![assistant_start(), assistant_done(third)],
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(fake.clone());

    let steering = Arc::new(Mutex::new(vec![UserMessage::new("steer")]));
    let follow_up = Arc::new(Mutex::new(vec![UserMessage::new("follow up")]));
    let pop = |queue: Arc<Mutex<Vec<UserMessage>>>| -> QueueDrain {
        Arc::new(move || {
            let mut guard = queue.lock().unwrap();
            if guard.is_empty() {
                Vec::new()
            } else {
                vec![AgentMessage::User(guard.remove(0))]
            }
        })
    };

    let messages = shared(vec![AgentMessage::User(UserMessage::new("start"))]);
    let mut config = base_config(provider, messages.clone(), vec![tool("work", execute)]);
    config.get_steering_messages = Some(pop(steering));
    config.get_follow_up_messages = Some(pop(follow_up));
    let _ = collect(config).await;

    assert_eq!(
        user_texts(&messages.lock().unwrap()),
        ["start", "steer", "follow up"]
    );
    assert_eq!(fake.call_count(), 3);
}

#[tokio::test]
async fn stops_with_assistant_error_after_max_turns() {
    let call = ToolCall::new("call-1", "missing", JsonMap::new());
    let assistant =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let fake = FakeProvider::new(vec![vec![
        assistant_start(),
        tool_call_end(call),
        assistant_done_reason(assistant, "toolUse"),
    ]]);
    let provider: Arc<dyn ModelProvider> = Arc::new(fake.clone());
    let messages = shared(vec![AgentMessage::User(UserMessage::new("loop"))]);

    let mut config = base_config(provider, messages.clone(), vec![]);
    config.max_turns = Some(1);
    let _ = collect(config).await;

    let last = messages.lock().unwrap().last().cloned().unwrap();
    let AgentMessage::Assistant(error) = last else {
        panic!("expected assistant error");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Agent stopped after max_turns=1")
    );
    assert_eq!(fake.call_count(), 1);
}

#[tokio::test]
async fn tool_error_becomes_is_error_result() {
    // rho-specific: a tool returning `Err` is isolated into an `is_error` result
    // (tau's `except Exception` path). Verifies the errors-are-data contract.
    let boom: ToolExecutor = Arc::new(
        |_id,
         _arguments,
         _signal,
         _on_update: ToolUpdateCallback|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async move { Err(ToolError("kaboom".into())) })
        },
    );
    let call = ToolCall::new("call-1", "boom", JsonMap::new());
    let assistant =
        AssistantMessage::new(vec![AssistantContent::ToolCall(call.clone())]).with_model("fake");
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(vec![vec![
        assistant_start(),
        tool_call_end(call),
        assistant_done_reason(assistant, "toolUse"),
    ]]));
    let messages = shared(vec![AgentMessage::User(UserMessage::new("go"))]);
    let mut config = base_config(provider, messages.clone(), vec![tool("boom", boom)]);
    config.max_turns = Some(1);
    let _ = collect(config).await;

    let result = messages
        .lock()
        .unwrap()
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.clone()),
            _ => None,
        })
        .expect("tool result");
    assert!(result.is_error);
    assert_eq!(result.text(), "kaboom");
}
