//! Golden event-stream tests — the M2 loop/harness correctness oracle.
//!
//! For every scenario in `fixtures/event-streams/`, this drives the **real**
//! [`AgentHarness`] + [`run_agent_loop`] with a [`FakeProvider`] replaying the
//! scenario's `script.json`, then asserts the emitted [`AgentEvent`] JSON
//! sequence is **byte-identical** to the `agent-events.jsonl` tau produced.
//!
//! The clock is pinned to tau's frozen extraction values
//! ([`FixedClock::fixture`] = `1_700_000_000_123` ms), so loop-authored messages
//! (prompts, tool results, max-turns errors) carry the same timestamps tau's
//! monkeypatched clock stamped. Per repo policy, a diff here is a bug in the rho
//! *code*, never in the fixture.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::future::BoxFuture;

use rho_agent::clock::FixedClock;
use rho_agent::events::AgentEvent;
use rho_agent::fake::FakeProvider;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig, Unsubscribe};
use rho_agent::messages::{TextContent, ToolResultContent};
use rho_agent::provider::{CancellationToken, ModelProvider};
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolUpdateCallback};
use rho_agent::types::JsonMap;
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures dir exists")
}

/// Python `json.dumps(value, sort_keys=True)` — the exact formatting the fixture
/// extraction's `_echo_tool` used (spaces after `:` and `,`, sorted keys).
fn py_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap(),
                        py_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(py_json).collect();
            format!("[{}]", inner.join(", "))
        }
        other => serde_json::to_string(other).unwrap(),
    }
}

/// Port of the extraction's `_echo_tool`: echoes sorted-JSON args as text with
/// `details = {"echoed": true}`.
fn echo_tool() -> AgentTool {
    let execute: rho_agent::tools::ToolExecutor = Arc::new(
        |_id: String,
         arguments: JsonMap,
         _signal: Option<Arc<dyn CancellationToken>>,
         _on_update: ToolUpdateCallback|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async move {
                let text = py_json(&Value::Object(arguments));
                let mut result =
                    AgentToolResult::new(vec![ToolResultContent::Text(TextContent::new(text))]);
                result.details = Some(serde_json::json!({"echoed": true}));
                Ok(result)
            })
        },
    );
    AgentTool::new(
        "echo",
        "echo",
        "Echo arguments back.",
        match serde_json::json!({"type": "object", "properties": {}}) {
            Value::Object(m) => m,
            _ => JsonMap::new(),
        },
        execute,
    )
}

/// Run one scenario and return the emitted events serialized as JSONL bytes.
async fn run_scenario(script: &Value) -> String {
    let prompt = script["prompt"].as_str().unwrap();
    let system = script["system"].as_str().unwrap();
    let model = script["model"].as_str().unwrap();
    let max_turns = script["max_turns"].as_i64();
    let uses_tools = script["tools"]
        .as_array()
        .is_some_and(|arr| !arr.is_empty());

    // Deserialize each scripted stream into typed assistant events.
    let streams: Vec<Vec<AssistantMessageEvent>> = script["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| {
            stream
                .as_array()
                .unwrap()
                .iter()
                .map(|event| serde_json::from_value(event.clone()).expect("assistant event"))
                .collect()
        })
        .collect();

    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(streams));
    let tools = if uses_tools {
        vec![echo_tool()]
    } else {
        vec![]
    };

    let config = AgentHarnessConfig::new(provider, model, system)
        .with_tools(tools)
        .with_max_turns(max_turns)
        .with_clock(Arc::new(FixedClock::fixture()));
    let harness = Arc::new(AgentHarness::new(config, Vec::new()));

    // Steering injection between turns (mirrors the extraction's subscriber).
    if let Some(steer_after) = script["steer_after_turn"].as_u64() {
        let steer_text = script["steer_text"].as_str().unwrap().to_string();
        let count = Arc::new(AtomicU64::new(0));
        let unsub: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
        let listener = {
            let harness = harness.clone();
            let count = count.clone();
            let unsub = unsub.clone();
            Arc::new(move |event: &AgentEvent| {
                if matches!(event, AgentEvent::TurnEnd(_)) {
                    let seen = count.fetch_add(1, Ordering::SeqCst) + 1;
                    if seen == steer_after {
                        harness.steer(&steer_text);
                        if let Some(unsubscribe) = unsub.lock().unwrap().take() {
                            unsubscribe();
                        }
                    }
                }
            })
        };
        *unsub.lock().unwrap() = Some(harness.subscribe(listener));
    }

    let mut stream = harness.prompt(prompt).expect("harness not running");
    let mut lines: Vec<String> = Vec::new();
    while let Some(event) = stream.next().await {
        lines.push(serde_json::to_string(&event).expect("serialize event"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

type ScenarioFuture = Pin<Box<dyn Future<Output = ()>>>;

fn check(name: &str) -> ScenarioFuture {
    let name = name.to_string();
    Box::pin(async move {
        let dir = fixtures_dir().join("event-streams").join(&name);
        let script: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("script.json")).unwrap())
                .unwrap();
        let expected = std::fs::read_to_string(dir.join("agent-events.jsonl")).unwrap();
        let actual = run_scenario(&script).await;
        assert_eq!(
            actual, expected,
            "\nevent-streams/{name}/agent-events.jsonl BYTE MISMATCH"
        );
    })
}

#[tokio::test]
async fn golden_text_only() {
    check("text-only").await;
}

#[tokio::test]
async fn golden_multi_tool_call() {
    check("multi-tool-call").await;
}

#[tokio::test]
async fn golden_thinking() {
    check("thinking").await;
}

#[tokio::test]
async fn golden_error_stop() {
    check("error-stop").await;
}

#[tokio::test]
async fn golden_steering() {
    check("steering").await;
}

#[tokio::test]
async fn golden_max_turns() {
    check("max-turns").await;
}
