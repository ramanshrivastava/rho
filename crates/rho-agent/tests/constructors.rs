//! Constructors must be usable from *outside* the crate — this is the API gap
//! that would otherwise block M2 (the `role`/`type` discriminator fields are
//! private, so struct literals are impossible externally). This integration test
//! lives in a separate crate, so it can only touch the public surface.

use rho_agent::events::{AgentEndEvent, AgentEvent, AgentStartEvent, MessageStartEvent};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolResultMessage, UserMessage,
};
use rho_agent::session::entries::{LeafEntry, MessageEntry, SessionEntry, SessionInfoEntry};
use rho_agent::tools::AgentToolResult;

#[test]
fn messages_construct_and_serialize() {
    // Convenience `Into<UserContent>` from a &str.
    let user = UserMessage::new("hello");
    let value = serde_json::to_value(&user).unwrap();
    assert_eq!(value["role"], "user");
    assert_eq!(value["content"], "hello");
    assert!(value["timestamp"].is_i64());

    // Assistant defaults mirror tau (unknown/stop) and serialize with the tag.
    let assistant = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("hi"))]);
    let value = serde_json::to_value(&assistant).unwrap();
    assert_eq!(value["role"], "assistant");
    assert_eq!(value["model"], "unknown");
    assert_eq!(value["stopReason"], "stop");

    // Optional fields set via the fluent builder...
    let errored = AssistantMessage::new(vec![])
        .with_model("claude")
        .with_stop_reason(rho_agent::messages::StopReason::Error)
        .with_error_message("boom");
    assert_eq!(errored.error_message.as_deref(), Some("boom"));
    assert_eq!(errored.model, "claude");

    // ...or by mutating the `pub` fields after `default()`/`new()`.
    let mut mutated = AssistantMessage::default();
    mutated.model = "m".into();
    assert_eq!(mutated.model, "m");

    let tr = ToolResultMessage::new("c1", "read", vec![]);
    assert_eq!(serde_json::to_value(&tr).unwrap()["toolName"], "read");

    let result = AgentToolResult::new(vec![]);
    assert!(result.details.is_none());
}

#[test]
fn events_and_entries_construct() {
    let start: AgentEvent = AgentEvent::AgentStart(AgentStartEvent::new());
    assert_eq!(serde_json::to_value(&start).unwrap()["type"], "agent_start");

    let msg = AgentMessage::User(UserMessage::new("hi"));
    let ev = AgentEvent::MessageStart(MessageStartEvent::new(msg.clone()));
    assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "message_start");

    let end = AgentEvent::AgentEnd(AgentEndEvent::new(vec![msg.clone()]));
    assert_eq!(
        serde_json::to_value(&end).unwrap()["messages"][0]["role"],
        "user"
    );

    // Entries auto-generate a 32-char hex id and a float timestamp.
    let entry = MessageEntry::new(msg);
    assert_eq!(entry.id.len(), 32);
    assert!(entry.id.chars().all(|c| c.is_ascii_hexdigit()));
    let se: SessionEntry = SessionEntry::Message(entry);
    assert_eq!(serde_json::to_value(&se).unwrap()["type"], "message");

    let mut info = SessionInfoEntry::new();
    info.parent_id = Some("p".into());
    assert_eq!(serde_json::to_value(&info).unwrap()["parent_id"], "p");

    let leaf = SessionEntry::Leaf(LeafEntry::new(Some("e1".into())));
    assert_eq!(serde_json::to_value(&leaf).unwrap()["entry_id"], "e1");
}
