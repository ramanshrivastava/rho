//! Crosscheck v2 — the `CodingSession`-level tau/rho differential harness.
//!
//! Where `crosscheck.rs` (v1) pins the bare-harness event stream, v2 drives the
//! full [`CodingSession`] and proves **sessions are interchangeable on disk**.
//! For each scenario (text / tool / compaction / branch) this reproduces the
//! three artifacts the tau driver (`tools/crosscheck/driver.py`) emits and
//! asserts equality:
//!
//! 1. **Raw session file** — rho's own writer must reproduce
//!    `tools/crosscheck/sessions/<name>.session.jsonl` **byte-for-byte**. Since
//!    tau's `patch_determinism` (counter uuids + frozen clocks) equals rho's
//!    `SequentialIdGen` + `FixedClock::fixture` and cwd is pinned, the files are
//!    literally identical — the on-disk interchange proof.
//! 2. **Normalized event stream** — `expected/v2/<name>.events.jsonl`.
//! 3. **Resume-swap (tau → rho)** — rho loads the committed (tau-written)
//!    session file and replays to the transcript `(role, text)` recorded in
//!    `expected/v2/<name>.state.jsonl`.
//!
//! The rho → tau direction (tau resumes a rho-written file) is
//! `resume_swap.py`, exercised by the `#[ignore]` test below and by
//! `just crosscheck`.
//!
//! CI-runnable without `uv`/tau: the expected files and session files are
//! committed. `just crosscheck` additionally regenerates the tau side.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;

use rho_agent::clock::{FixedClock, SequentialIdGen};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
};
use rho_agent::provider::{CancellationToken, ModelProvider};
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
    ToolCallEndEvent,
};
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::jsonl::entry_from_json_line;
use rho_agent::tools::{AgentTool, AgentToolResult};
use rho_agent::types::JsonMap;
use rho_ai::FakeProvider;
use rho_coding::CodingSessionEvent;
use rho_coding::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};

const FIXED_MS: i64 = 1_700_000_000_123;
const FIXED_CWD: &str = "/rho-crosscheck-cwd";

fn crosscheck_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/crosscheck")
        .canonicalize()
        .expect("crosscheck dir exists")
}

// ---- scenario building blocks (mirror tools/crosscheck/driver.py) ----------

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

fn tool_stream() -> Vec<AssistantMessageEvent> {
    let call = ToolCall::new("call-1", "echo", json_map(serde_json::json!({"n": 1})));
    let tool_msg = fixed_assistant(vec![AssistantContent::ToolCall(call.clone())]);
    vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(fixed_assistant(Vec::new()))),
        AssistantMessageEvent::ToolCallEnd(ToolCallEndEvent::new(0, call, tool_msg.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::ToolUse, tool_msg)),
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

#[derive(Clone)]
enum Op {
    Prompt(&'static str),
    Compact,
    BranchFirstAssistant,
}

struct Scenario {
    name: &'static str,
    tools: bool,
    streams: Vec<Vec<AssistantMessageEvent>>,
    ops: Vec<Op>,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "text",
            tools: false,
            streams: vec![text_stream("hello")],
            ops: vec![Op::Prompt("hi")],
        },
        Scenario {
            name: "tool",
            tools: true,
            streams: vec![tool_stream(), text_stream("done")],
            ops: vec![Op::Prompt("use echo")],
        },
        Scenario {
            name: "compaction",
            tools: false,
            streams: vec![
                text_stream("did the work"),
                text_stream("## Summary\nprior work done"),
            ],
            ops: vec![Op::Prompt("work"), Op::Compact],
        },
        Scenario {
            name: "branch",
            tools: false,
            streams: vec![text_stream("first reply"), text_stream("second reply")],
            ops: vec![
                Op::Prompt("first"),
                Op::Prompt("second"),
                Op::BranchFirstAssistant,
            ],
        },
    ]
}

fn pinned_config(
    provider: Arc<dyn ModelProvider>,
    storage: Arc<dyn rho_agent::session::storage::SessionStorage>,
    tools: bool,
) -> CodingSessionConfig {
    let mut config = CodingSessionConfig::new(provider, "fake", storage, PathBuf::from(FIXED_CWD));
    config.clock = Arc::new(FixedClock::fixture());
    config.id_gen = Arc::new(SequentialIdGen::new());
    config.provider_name = "fake".to_string();
    config.tools = Some(if tools { vec![echo_tool()] } else { vec![] });
    config
}

fn first_assistant_entry_id(session_path: &Path) -> String {
    let text = std::fs::read_to_string(session_path).unwrap();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let SessionEntry::Message(m) = entry_from_json_line(line, None).unwrap() {
            if let AgentMessage::Assistant(_) = m.message {
                return m.id;
            }
        }
    }
    panic!("no assistant message entry in {}", session_path.display());
}

/// Run a scenario through rho's `CodingSession`, returning (raw session file
/// bytes, serialized event stream lines).
async fn run_scenario(scenario: &Scenario, session_path: &Path) -> (String, Vec<String>) {
    let provider = Arc::new(FakeProvider::new(scenario.streams.clone()));
    let storage = jsonl_session_storage(session_path);
    let mut session = CodingSession::load(pinned_config(provider, storage, scenario.tools))
        .await
        .unwrap();

    let mut events = Vec::new();
    for op in &scenario.ops {
        match op {
            Op::Prompt(text) => {
                let stream = session.prompt((*text).to_string(), None);
                futures::pin_mut!(stream);
                while let Some(event) = stream.next().await {
                    events.push(serialize_event(&event));
                }
            }
            Op::Compact => {
                session.compact(None).await.unwrap();
            }
            Op::BranchFirstAssistant => {
                let id = first_assistant_entry_id(session_path);
                session
                    .branch_to_entry(&id, false, None, false)
                    .await
                    .unwrap();
            }
        }
    }
    let bytes = std::fs::read_to_string(session_path).unwrap();
    (bytes, events)
}

fn serialize_event(event: &CodingSessionEvent) -> String {
    serde_json::to_string(event).expect("event serializes")
}

/// `(role, text)` of the transcript replayed from `session_path`.
async fn replay_state(session_path: &Path, tools: bool) -> Vec<String> {
    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(session_path);
    let session = CodingSession::load(pinned_config(provider, storage, tools))
        .await
        .unwrap();
    session
        .messages()
        .iter()
        .map(|m| {
            serde_json::to_string(&serde_json::json!({"role": m.role(), "text": m.text()})).unwrap()
        })
        .collect()
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn crosscheck_v2_all_scenarios() {
    let dir = crosscheck_dir();
    let mut failures = Vec::new();

    for scenario in scenarios() {
        let tmp = tempfile::tempdir().unwrap();
        let session_path = tmp.path().join("s.jsonl");
        let (rho_bytes, rho_events) = run_scenario(&scenario, &session_path).await;
        let name = scenario.name;

        // 1. Raw session file: rho's writer reproduces tau's bytes exactly.
        let expected_session =
            std::fs::read_to_string(dir.join(format!("sessions/{name}.session.jsonl"))).unwrap();
        if rho_bytes != expected_session {
            failures.push(format!(
                "{name}: session file BYTE MISMATCH\n--- expected (tau) ---\n{expected_session}\n--- actual (rho) ---\n{rho_bytes}"
            ));
        }

        // 2. Normalized event stream.
        let normalized = normalize_stream(&rho_events);
        let want_events = read_lines(&dir.join(format!("expected/v2/{name}.events.jsonl")));
        if normalized != want_events {
            failures.push(format!(
                "{name}: event stream diverged\n  got:  {normalized:?}\n  want: {want_events:?}"
            ));
        }

        // 3. Resume-swap tau -> rho: load the committed (tau-written) session
        //    file and assert the replayed transcript matches.
        let tau_session_path = dir.join(format!("sessions/{name}.session.jsonl"));
        let state = replay_state(&tau_session_path, scenario.tools).await;
        let want_state = read_lines(&dir.join(format!("expected/v2/{name}.state.jsonl")));
        if state != want_state {
            failures.push(format!(
                "{name}: resume-swap (tau->rho) state diverged\n  got:  {state:?}\n  want: {want_state:?}"
            ));
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// rho → tau resume-swap: tau resumes each committed (rho-byte-identical)
/// session file and must replay to the same `(role, text)` state. Requires
/// `uv`/tau, so it is `#[ignore]`d in CI and run by `just crosscheck`.
#[test]
#[ignore = "requires uv + tau checkout; run via `just crosscheck`"]
fn crosscheck_v2_resume_swap_rho_to_tau() {
    let dir = crosscheck_dir();
    let script = dir.join("resume_swap.py");
    let tau = std::env::var("TAU_CHECKOUT")
        .unwrap_or_else(|_| "/Users/ramanshrivastava/code/oss-gold/tau".to_string());
    let output = std::process::Command::new("uv")
        .args(["run", "--project", &tau, "python"])
        .arg(&script)
        .output()
        .expect("run resume_swap.py via uv");
    assert!(
        output.status.success(),
        "resume_swap.py failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
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
