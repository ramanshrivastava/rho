//! rho side of the tau/rho differential crosscheck.
//!
//! Reproduces the same three scripted fake-provider sessions the tau driver
//! (`tools/crosscheck/driver.py`) runs, through the *identical* serialization
//! path as `rho -p --output-format json` (the `JsonEventRenderer` — i.e.
//! `serde_json::to_string` of each `CodingSessionEvent`), normalizes the streams
//! with the same rules as `tools/crosscheck/normalizer.py`, and asserts equality
//! against the committed `tools/crosscheck/expected/*.jsonl`.
//!
//! Both sides are made deterministic with a fixed clock (`FixedClock::fixture()`
//! ⇔ tau's `patch_determinism`), so the timestamp tokens are language-agnostic.
//! This runs in CI without `uv`/tau (the expected files are committed); `just
//! crosscheck` additionally regenerates the tau side to confirm the oracle.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;

use rho_agent::clock::{Clock, FixedClock};
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::messages::{
    AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
};
use rho_agent::provider::{CancellationToken, ModelProvider};
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
    ToolCallEndEvent,
};
use rho_agent::tools::{AgentTool, AgentToolResult};
use rho_agent::types::JsonMap;
use rho_ai::FakeProvider;
use rho_coding::CodingSessionEvent;

/// tau's frozen message clock (`_FIXED_MESSAGE_TIME * 1000`).
const FIXED_MS: i64 = 1_700_000_000_123;

fn fixed_assistant(content: Vec<AssistantContent>) -> AssistantMessage {
    let mut message = AssistantMessage::new(content).with_model("fake");
    message.timestamp = FIXED_MS;
    message
}

fn text_stream(text: &str) -> Vec<AssistantMessageEvent> {
    let full = fixed_assistant(vec![AssistantContent::Text(TextContent::new(text))]);
    vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(fixed_assistant(Vec::new()))),
        AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, text, full.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::Stop, full)),
    ]
}

fn echo_tool() -> AgentTool {
    AgentTool::new(
        "echo",
        "echo",
        "echo",
        json_map(serde_json::json!({"type": "object", "properties": {}})),
        Arc::new(
            |_id, _args, _signal: Option<Arc<dyn CancellationToken>>, _on_update| {
                Box::pin(async {
                    Ok(AgentToolResult {
                        content: vec![ToolResultContent::Text(TextContent::new("ok"))],
                        details: Some(serde_json::Value::Object(serde_json::Map::new())),
                        added_tool_names: None,
                        terminate: None,
                    })
                })
            },
        ),
    )
}

fn json_map(value: serde_json::Value) -> JsonMap {
    match value {
        serde_json::Value::Object(map) => map,
        _ => JsonMap::new(),
    }
}

async fn run_scenario(
    prompt: &str,
    tools: Vec<AgentTool>,
    streams: Vec<Vec<AssistantMessageEvent>>,
    follow_up: Option<&str>,
) -> Vec<String> {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(streams));
    let config = AgentHarnessConfig::new(provider, "fake", "crosscheck")
        .with_tools(tools)
        .with_clock(Arc::new(FixedClock::fixture()) as Arc<dyn Clock>);
    let harness = AgentHarness::new(config, Vec::new());
    if let Some(text) = follow_up {
        harness.follow_up(text);
    }

    let mut stream = harness.prompt(prompt).expect("harness not running");
    let mut lines = Vec::new();
    while let Some(event) = stream.next().await {
        let event = CodingSessionEvent::Agent(event);
        lines.push(serde_json::to_string(&event).expect("event serializes"));
    }
    lines
}

// ---- normalizer (port of tools/crosscheck/normalizer.py) -------------------

const ID_KEYS: &[&str] = &[
    "id",
    "parentId",
    "parent_id",
    "toolCallId",
    "tool_call_id",
    "responseId",
    "response_id",
    "entryId",
    "entry_id",
    "fromId",
    "from_id",
    "branchRootId",
    "branch_root_id",
];
const ID_LIST_KEYS: &[&str] = &["replacesEntryIds", "replaces_entry_ids"];
const TS_KEYS: &[&str] = &["timestamp", "createdAt", "created_at"];

#[derive(Default)]
struct Normalizer {
    ids: HashMap<String, String>,
    tss: HashMap<String, String>,
}

impl Normalizer {
    fn id_token(&mut self, value: &str) -> String {
        let next = format!("<id:{}>", self.ids.len());
        self.ids.entry(value.to_string()).or_insert(next).clone()
    }

    fn ts_token(&mut self, value: &serde_json::Number) -> String {
        let key = value.to_string();
        let next = format!("<ts:{}>", self.tss.len());
        self.tss.entry(key).or_insert(next).clone()
    }

    fn normalize(&mut self, value: serde_json::Value, key: Option<&str>) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let nv = self.normalize(v, Some(&k));
                        (k, nv)
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                if key.is_some_and(|k| ID_LIST_KEYS.contains(&k)) {
                    Value::Array(
                        items
                            .into_iter()
                            .map(|v| match v {
                                Value::String(s) => Value::String(self.id_token(&s)),
                                other => other,
                            })
                            .collect(),
                    )
                } else {
                    Value::Array(items.into_iter().map(|v| self.normalize(v, key)).collect())
                }
            }
            Value::String(s) if key.is_some_and(|k| ID_KEYS.contains(&k)) => {
                Value::String(self.id_token(&s))
            }
            Value::Number(n) if key.is_some_and(|k| TS_KEYS.contains(&k)) => {
                Value::String(self.ts_token(&n))
            }
            other => other,
        }
    }

    fn normalize_line(&mut self, line: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        let normalized = self.normalize(value, None);
        serde_json::to_string(&normalized).expect("normalized serializes")
    }
}

fn normalize_stream(lines: &[String]) -> Vec<String> {
    let mut normalizer = Normalizer::default();
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| normalizer.normalize_line(l))
        .collect()
}

fn expected(name: &str) -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/crosscheck/expected/"
    );
    let text = std::fs::read_to_string(format!("{path}{name}.jsonl"))
        .unwrap_or_else(|e| panic!("read expected {name}: {e}"));
    text.lines().map(str::to_string).collect()
}

fn assert_scenario(name: &str, got: &[String]) {
    let normalized = normalize_stream(got);
    let want = expected(name);
    assert_eq!(
        normalized.len(),
        want.len(),
        "{name}: event count differs (got {}, want {})",
        normalized.len(),
        want.len()
    );
    for (index, (got_line, want_line)) in normalized.iter().zip(want.iter()).enumerate() {
        assert_eq!(got_line, want_line, "{name}: line {index} diverged");
    }
}

#[tokio::test]
async fn crosscheck_text_scenario() {
    let got = run_scenario("hi", Vec::new(), vec![text_stream("hello")], None).await;
    assert_scenario("text", &got);
}

#[tokio::test]
async fn crosscheck_tool_scenario() {
    let call = ToolCall::new("call-1", "echo", json_map(serde_json::json!({"n": 1})));
    let tool_msg = fixed_assistant(vec![AssistantContent::ToolCall(call.clone())]);
    let stream1 = vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(fixed_assistant(Vec::new()))),
        AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(0, call, tool_msg.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::ToolUse, tool_msg)),
    ];
    let got = run_scenario(
        "use echo",
        vec![echo_tool()],
        vec![stream1, text_stream("done")],
        None,
    )
    .await;
    assert_scenario("tool", &got);
}

#[tokio::test]
async fn crosscheck_multiturn_scenario() {
    let got = run_scenario(
        "count",
        Vec::new(),
        vec![text_stream("one"), text_stream("two")],
        Some("again"),
    )
    .await;
    assert_scenario("multiturn", &got);
}
