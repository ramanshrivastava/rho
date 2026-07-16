//! Ported from tau `tests/test_session.py` — session entries, JSONL storage,
//! state replay, and tree traversal.
//!
//! Entry ids/timestamps are set explicitly after construction (the `type`
//! discriminator is private, but `id`/`parent_id`/`timestamp` are `pub`), so
//! these mirror tau's keyword-constructed entries.

use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, CustomMessage, TextContent, ToolCall,
    ToolResultContent, ToolResultMessage, UserMessage,
};
use rho_agent::session::entries::{
    CompactionEntry, CustomEntry, LabelEntry, LeafEntry, MessageEntry, ModelChangeEntry,
    SessionEntry,
};
use rho_agent::session::jsonl::{entry_from_json_line, entry_to_json_line};
use rho_agent::session::memory::SessionState;
use rho_agent::session::storage::{JsonlSessionStorage, SessionStorage};
use rho_agent::session::tree::{SessionTreeError, path_to_entry};
use rho_agent::types::JsonMap;
use serde_json::Value;

fn message_entry(id: &str, message: AgentMessage) -> SessionEntry {
    let mut e = MessageEntry::new(message);
    e.id = id.to_string();
    SessionEntry::Message(e)
}

fn user(content: &str, timestamp: i64) -> AgentMessage {
    let mut m = UserMessage::new(content);
    m.timestamp = timestamp;
    AgentMessage::User(m)
}

#[test]
fn session_entry_round_trips_canonical_jsonl() {
    let mut entry = MessageEntry::new(user("Hello", 2));
    entry.id = "entry-1".into();
    entry.timestamp = 1.0;
    let entry = SessionEntry::Message(entry);

    let line = entry_to_json_line(&entry);
    assert_eq!(entry_from_json_line(line.trim_end(), None).unwrap(), entry);

    let parsed: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        parsed["message"],
        serde_json::json!({"role": "user", "content": "Hello", "timestamp": 2})
    );
}

#[test]
fn custom_message_round_trips_with_pi_role_and_metadata() {
    let mut custom = CustomMessage::new("subagent-notification", "<task-notification/>");
    custom.details = Some(serde_json::json!({"id": "run-1"}));
    let entry = message_entry("entry-1", AgentMessage::Custom(custom));

    let line = entry_to_json_line(&entry);
    let parsed = entry_from_json_line(line.trim_end(), None).unwrap();

    let payload: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(payload["message"]["role"], "custom");
    assert_eq!(payload["message"]["customType"], "subagent-notification");
    assert_eq!(parsed, entry);
}

#[test]
fn assistant_and_tool_result_round_trip_canonical_blocks() {
    let assistant = message_entry(
        "a",
        AgentMessage::Assistant(AssistantMessage::new(vec![AssistantContent::Text(
            TextContent::new("Hi"),
        )])),
    );
    let mut tr = ToolResultMessage::new(
        "call-1",
        "edit",
        vec![ToolResultContent::Text(TextContent::new(
            "Successfully replaced 1 block.",
        ))],
    );
    tr.details = Some(serde_json::json!({"patch": "--- a.py\n+++ a.py"}));
    let result = message_entry("r", AgentMessage::ToolResult(tr));

    let assistant_line = entry_to_json_line(&assistant);
    let result_line = entry_to_json_line(&result);
    let assistant_payload: Value = serde_json::from_str(&assistant_line).unwrap();
    let result_payload: Value = serde_json::from_str(&result_line).unwrap();

    assert_eq!(assistant_payload["message"]["content"][0]["text"], "Hi");
    assert_eq!(assistant_payload["message"]["usage"]["totalTokens"], 0);
    assert_eq!(result_payload["message"]["role"], "toolResult");
    assert_eq!(result_payload["message"]["toolName"], "edit");
    assert_eq!(
        entry_from_json_line(assistant_line.trim_end(), None).unwrap(),
        assistant
    );
    assert_eq!(
        entry_from_json_line(result_line.trim_end(), None).unwrap(),
        result
    );
}

#[test]
fn invalid_jsonl_line_raises_useful_error() {
    let err = entry_from_json_line(r#"{"type":"unknown"}"#, Some(3)).unwrap_err();
    assert!(
        err.to_string().contains("Invalid session entry on line 3"),
        "got: {err}"
    );
}

#[tokio::test]
async fn jsonl_storage_append_is_byte_identical_to_session_fixtures() {
    // Deliverable #7: appending each parsed entry back through
    // `JsonlSessionStorage` reproduces the fixture's bytes exactly (the
    // `exclude_none` storage path). Legacy fixtures are excluded — they migrate,
    // so the appended bytes are the *post-migration* canonical form, not the
    // legacy input (that path is pinned by `legacy_session_migration`).
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sessions");
    for name in ["linear", "branched", "compaction", "kitchen-sink"] {
        let expected = std::fs::read_to_string(fixtures.join(format!("{name}.jsonl"))).unwrap();
        let out_dir = std::env::temp_dir().join(format!(
            "rho-session-golden-{}",
            rho_agent::session::entries::new_entry_id()
        ));
        let path = out_dir.join(format!("{name}.jsonl"));
        let storage = JsonlSessionStorage::new(&path);
        for line in expected.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry = entry_from_json_line(line, None).unwrap();
            storage.append(&entry).await.unwrap();
        }
        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            actual, expected,
            "sessions/{name}.jsonl storage byte mismatch"
        );
        let _ = std::fs::remove_dir_all(&out_dir);
    }
}

#[tokio::test]
async fn jsonl_storage_appends_and_reads_entries() {
    let dir = std::env::temp_dir().join(format!(
        "rho-session-{}",
        rho_agent::session::entries::new_entry_id()
    ));
    let storage = JsonlSessionStorage::new(dir.join("sessions").join("one.jsonl"));

    let first = message_entry("one", AgentMessage::User(UserMessage::new("Hi")));
    let mut label = LabelEntry::new("Greeting");
    label.id = "two".into();
    let second = SessionEntry::Label(label);

    storage.append(&first).await.unwrap();
    storage.append(&second).await.unwrap();

    assert_eq!(storage.read_all().await.unwrap(), vec![first, second]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_state_replays_linear_entries() {
    let user_msg = user("Hi", 1);
    let assistant = {
        let mut a = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("Hello"))]);
        a.timestamp = 2;
        AgentMessage::Assistant(a)
    };
    let mut model = ModelChangeEntry::new("fake-model");
    model.id = "model".into();
    let mut label = LabelEntry::new("Greeting");
    label.id = "label".into();
    let mut custom = CustomEntry::new(
        "test",
        match serde_json::json!({"ok": true}) {
            Value::Object(m) => m,
            _ => JsonMap::new(),
        },
    );
    custom.id = "custom".into();
    let mut leaf = LeafEntry::new(Some("assistant".into()));
    leaf.id = "leaf".into();

    let entries = vec![
        message_entry("user", user_msg.clone()),
        SessionEntry::ModelChange(model),
        message_entry("assistant", assistant.clone()),
        SessionEntry::Label(label),
        SessionEntry::Custom(custom),
        SessionEntry::Leaf(leaf),
    ];

    let state = SessionState::from_entries(&entries);
    assert_eq!(state.messages, vec![user_msg, assistant]);
    assert_eq!(state.model.as_deref(), Some("fake-model"));
    assert_eq!(state.label.as_deref(), Some("Greeting"));
    assert_eq!(state.active_leaf_id.as_deref(), Some("assistant"));
}

#[test]
fn session_state_applies_compaction_and_branch_summary() {
    let mut compaction = CompactionEntry::new(
        "The user asked about sessions.",
        vec!["user".into(), "assistant".into()],
    );
    compaction.id = "compact".into();
    let mut branch =
        rho_agent::session::entries::BranchSummaryEntry::new("A side branch explored storage.");
    branch.id = "branch".into();

    let entries = vec![
        message_entry(
            "user",
            AgentMessage::User(UserMessage::new("Explain sessions.")),
        ),
        message_entry(
            "assistant",
            AgentMessage::Assistant(AssistantMessage::new(vec![AssistantContent::Text(
                TextContent::new("They are trees."),
            )])),
        ),
        SessionEntry::Compaction(compaction),
        SessionEntry::BranchSummary(branch),
    ];

    let state = SessionState::from_entries(&entries);
    let roles: Vec<&str> = state.messages.iter().map(AgentMessage::role).collect();
    assert_eq!(roles, ["user", "user"]);
    assert!(
        state.messages[0]
            .text()
            .contains("The user asked about sessions.")
    );
    assert!(
        state.messages[1]
            .text()
            .contains("A side branch explored storage.")
    );
}

#[test]
fn path_to_entry_follows_parent_chain() {
    let root = message_entry("root", AgentMessage::User(UserMessage::new("Hi")));
    let mut child_entry = MessageEntry::new(AgentMessage::Assistant(AssistantMessage::new(vec![
        AssistantContent::Text(TextContent::new("Hello")),
    ])));
    child_entry.id = "child".into();
    child_entry.parent_id = Some("root".into());
    let child = SessionEntry::Message(child_entry);
    let mut leaf = LeafEntry::new(Some("child".into()));
    leaf.id = "leaf".into();
    leaf.parent_id = Some("child".into());
    let leaf = SessionEntry::Leaf(leaf);

    let path = path_to_entry(&[root, child, leaf], "child").unwrap();
    let ids: Vec<&str> = path.iter().map(SessionEntry::id).collect();
    assert_eq!(ids, ["root", "child"]);
}

#[test]
fn path_to_entry_rejects_missing_or_cyclic_parent() {
    assert!(matches!(
        path_to_entry(&[], "missing"),
        Err(SessionTreeError::MissingEntry(_))
    ));

    let mut first = CustomEntry::new("x", JsonMap::new());
    first.id = "first".into();
    first.parent_id = Some("second".into());
    let mut second = CustomEntry::new("x", JsonMap::new());
    second.id = "second".into();
    second.parent_id = Some("first".into());
    let entries = vec![SessionEntry::Custom(first), SessionEntry::Custom(second)];
    assert!(matches!(
        path_to_entry(&entries, "first"),
        Err(SessionTreeError::Cycle(_))
    ));
}

#[test]
fn legacy_assistant_tool_and_custom_messages_migrate() {
    // Condensed port of the three tau legacy-migration cases; the byte-exact
    // migration is already pinned by `golden_roundtrip::wire_legacy_migration`,
    // so this asserts the decoded shape.
    let legacy_assistant = r#"{"type":"message","id":"a","timestamp":1,"message":{"role":"assistant","content":"Reading.","tool_calls":[{"id":"call-1","name":"read","arguments":{"path":"README.md"}}]}}"#;
    let SessionEntry::Message(entry) = entry_from_json_line(legacy_assistant, None).unwrap() else {
        panic!("expected message entry");
    };
    let AgentMessage::Assistant(assistant) = entry.message else {
        panic!("expected assistant");
    };
    assert_eq!(assistant.text(), "Reading.");
    assert_eq!(assistant.tool_calls()[0].name, "read");
    let _ = ToolCall::new("x", "y", JsonMap::new()); // keep import used

    let legacy_tool = r#"{"type":"message","id":"tool","timestamp":1,"message":{"role":"tool","tool_call_id":"call-1","name":"edit","content":"changed","ok":false,"error":"failed","data":{"patch":"diff"},"details":{"line":12}}}"#;
    let SessionEntry::Message(entry) = entry_from_json_line(legacy_tool, None).unwrap() else {
        panic!("expected message entry");
    };
    let AgentMessage::ToolResult(result) = entry.message else {
        panic!("expected tool result");
    };
    assert_eq!(result.tool_name, "edit");
    assert!(result.is_error);
    assert_eq!(result.text(), "changed");
    assert_eq!(
        result.details,
        Some(serde_json::json!({"patch": "diff", "line": 12}))
    );

    let legacy_custom = r#"{"type":"message","id":"custom","timestamp":1,"message":{"role":"user","content":"<task-notification/>","custom_type":"subagent-notification","details":{"id":"run-1"}}}"#;
    let SessionEntry::Message(entry) = entry_from_json_line(legacy_custom, None).unwrap() else {
        panic!("expected message entry");
    };
    let AgentMessage::Custom(custom) = entry.message else {
        panic!("expected custom");
    };
    assert_eq!(custom.custom_type, "subagent-notification");
}
