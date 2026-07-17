//! `CodingSession` integration tests (dispatch-1 session core).
//!
//! These exercise the durable-message boundary end-to-end: a session written
//! through `CodingSession` with a pinned [`FixedClock`] / [`SequentialIdGen`]
//! produces deterministic entries, and a fresh `load` of the same storage
//! (resume) replays to the same [`SessionState`]. A second suite loads every
//! `fixtures/sessions/*.jsonl` through `CodingSession` and asserts the replayed
//! transcript matches a direct `SessionState` reconstruction (resume parity).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;

use rho_agent::clock::{FixedClock, SequentialIdGen};
use rho_agent::messages::{AssistantContent, AssistantMessage, StopReason, TextContent};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason, TextDeltaEvent,
};
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::jsonl::entry_from_json_line;
use rho_agent::session::memory::SessionState;
use rho_ai::FakeProvider;
use rho_coding::events::CodingSessionEvent;
use rho_coding::paths::RhoPaths;
use rho_coding::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};
use rho_coding::session_manager::SessionManager;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures dir exists")
}

/// A single text-answer turn for the `FakeProvider`.
fn text_turn(text: &str) -> Vec<AssistantMessageEvent> {
    let msg = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new(text))])
        .with_model("fake")
        .with_stop_reason(StopReason::Stop);
    vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(
            AssistantMessage::new(Vec::new()).with_model("fake"),
        )),
        AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, text, msg.clone())),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::Stop, msg)),
    ]
}

fn pinned_config(
    provider: Arc<dyn ModelProvider>,
    storage: Arc<dyn rho_agent::session::storage::SessionStorage>,
    cwd: PathBuf,
) -> CodingSessionConfig {
    let mut config = CodingSessionConfig::new(provider, "fake", storage, cwd);
    config.clock = Arc::new(FixedClock::fixture());
    config.id_gen = Arc::new(SequentialIdGen::new());
    config.provider_name = "fake".to_string();
    config
}

#[tokio::test]
async fn prompt_persists_user_assistant_and_leaf_entries_then_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let session_path = tmp.path().join("session.jsonl");

    let provider = Arc::new(FakeProvider::new(vec![text_turn("hello there")]));
    let storage = jsonl_session_storage(&session_path);
    let config = pinned_config(provider, storage, cwd.clone());

    let mut session = CodingSession::load(config).await.unwrap();
    // Empty session: nothing written until the first durable message.
    assert!(
        !session_path.exists(),
        "empty session defers its transcript file"
    );

    let events: Vec<CodingSessionEvent> = {
        let stream = session.prompt("hi".to_string(), None);
        futures::pin_mut!(stream);
        stream.collect().await
    };
    assert!(!events.is_empty());

    // The transcript now exists with: session_info, model_change, thinking,
    // message(user), leaf, message(assistant), leaf.
    let text = std::fs::read_to_string(&session_path).unwrap();
    let entries: Vec<SessionEntry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| entry_from_json_line(l, None).unwrap())
        .collect();
    let types: Vec<&str> = entries.iter().map(entry_type).collect();
    assert_eq!(
        types,
        vec![
            "session_info",
            "model_change",
            "thinking_level_change",
            "message",
            "leaf",
            "message",
            "leaf",
        ],
        "durable-message boundary writes each message + a leaf pointer"
    );

    // Deterministic ids from the SequentialIdGen: 0,1,2,... in creation order.
    assert_eq!(entries[0].id(), "0".repeat(32), "first entry id is 0");
    assert_eq!(session.messages().len(), 2);
    assert_eq!(session.messages()[0].text(), "hi");
    assert_eq!(session.messages()[1].text(), "hello there");

    // Resume: a fresh load of the same storage replays to the same transcript.
    let provider2 = Arc::new(FakeProvider::new(vec![]));
    let storage2 = jsonl_session_storage(&session_path);
    let config2 = pinned_config(provider2, storage2, cwd.clone());
    let resumed = CodingSession::load(config2).await.unwrap();
    assert_eq!(
        resumed.messages(),
        session.messages(),
        "resume replays to the same transcript"
    );
}

#[tokio::test]
async fn writing_two_prompts_advances_the_parent_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let session_path = tmp.path().join("session.jsonl");

    let provider = Arc::new(FakeProvider::new(vec![text_turn("one"), text_turn("two")]));
    let storage = jsonl_session_storage(&session_path);
    let mut session = CodingSession::load(pinned_config(provider, storage, cwd))
        .await
        .unwrap();

    drain(session.prompt("first".to_string(), None)).await;
    drain(session.prompt("second".to_string(), None)).await;

    let msgs = session.messages();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].text(), "first");
    assert_eq!(msgs[1].text(), "one");
    assert_eq!(msgs[2].text(), "second");
    assert_eq!(msgs[3].text(), "two");
}

#[tokio::test]
async fn thinking_level_change_is_persisted_and_replayed() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let session_path = tmp.path().join("session.jsonl");

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(&session_path);
    let mut session = CodingSession::load(pinned_config(provider, storage, cwd.clone()))
        .await
        .unwrap();
    assert_eq!(session.thinking_level(), "medium");
    session.set_thinking_level("high").await.unwrap();
    assert_eq!(session.thinking_level(), "high");

    // Resume restores the thinking level from the persisted entry.
    let storage2 = jsonl_session_storage(&session_path);
    let provider2 = Arc::new(FakeProvider::new(vec![]));
    let resumed = CodingSession::load(pinned_config(provider2, storage2, cwd))
        .await
        .unwrap();
    assert_eq!(resumed.thinking_level(), "high");
}

#[tokio::test]
async fn new_session_is_indexed_after_first_message() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let manager = SessionManager::new(RhoPaths::new(
        tmp.path().join(".rho"),
        tmp.path().join(".agents"),
    ));
    let record = manager.prepare_session(&cwd, "fake", Some("fake"), None, None);

    let provider = Arc::new(FakeProvider::new(vec![text_turn("done")]));
    let storage = jsonl_session_storage(&record.path);
    let mut config = pinned_config(provider, storage, cwd.clone());
    config.session_id = Some(record.id.clone());
    config.session_manager = Some(manager.clone());
    config.index_on_first_persist = true;

    let mut session = CodingSession::load(config).await.unwrap();
    assert!(
        manager.get_session(&record.id).is_none(),
        "not indexed before the first durable write"
    );
    drain(session.prompt("go".to_string(), None)).await;
    assert!(
        manager.get_session(&record.id).is_some(),
        "indexed after the first durable write"
    );
}

#[test]
fn parse_terminal_command_prefixes() {
    use rho_coding::session::parse_terminal_command;
    let ctx = parse_terminal_command("! ls -la").unwrap();
    assert_eq!(ctx.command, "ls -la");
    assert!(ctx.add_to_context);
    let no_ctx = parse_terminal_command("!!echo hi").unwrap();
    assert_eq!(no_ctx.command, "echo hi");
    assert!(!no_ctx.add_to_context);
    assert!(parse_terminal_command("plain text").is_none());
    assert!(parse_terminal_command("!  ").is_none());
}

/// Resume parity: every session fixture loads through `CodingSession` and
/// replays to the same transcript a direct `SessionState` reconstruction gives.
#[tokio::test]
async fn resume_of_every_fixture_session_matches_direct_replay() {
    let sessions = fixtures_dir().join("sessions");
    for name in ["linear", "branched", "compaction", "kitchen-sink"] {
        let src = sessions.join(format!("{name}.jsonl"));
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join(format!("{name}.jsonl"));
        std::fs::copy(&src, &dst).unwrap();

        // Direct reconstruction: replay at the latest leaf.
        let text = std::fs::read_to_string(&dst).unwrap();
        let entries: Vec<SessionEntry> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| entry_from_json_line(l, None).unwrap())
            .collect();
        let latest_leaf = entries.iter().rev().find_map(|e| match e {
            SessionEntry::Leaf(l) => Some(l.entry_id.clone()),
            _ => None,
        });
        let expected = match latest_leaf.flatten() {
            Some(id) => SessionState::from_entries_at_leaf(&entries, Some(&id)).unwrap(),
            None => SessionState::from_entries(&entries),
        };

        // CodingSession resume of the same file.
        let provider = Arc::new(FakeProvider::new(vec![]));
        let storage = jsonl_session_storage(&dst);
        let cwd = tmp.path().to_path_buf();
        let session = CodingSession::load(pinned_config(provider, storage, cwd))
            .await
            .unwrap();
        // Compare (role, text) per message rather than full structs: a
        // branch-summary message is *synthesized* during replay with a
        // wall-clock timestamp (rho-agent M2 behavior), so the two independent
        // reconstructions can differ by a millisecond in that field alone.
        let got: Vec<(&str, String)> = session
            .messages()
            .iter()
            .map(|m| (m.role(), m.text()))
            .collect();
        let want: Vec<(&str, String)> = expected
            .messages
            .iter()
            .map(|m| (m.role(), m.text()))
            .collect();
        assert_eq!(
            got, want,
            "resume of sessions/{name}.jsonl replays to the same transcript"
        );
    }
}

async fn drain(stream: impl futures::Stream<Item = CodingSessionEvent>) {
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}
}

fn entry_type(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::Message(_) => "message",
        SessionEntry::ModelChange(_) => "model_change",
        SessionEntry::ThinkingLevelChange(_) => "thinking_level_change",
        SessionEntry::Compaction(_) => "compaction",
        SessionEntry::BranchSummary(_) => "branch_summary",
        SessionEntry::Label(_) => "label",
        SessionEntry::Leaf(_) => "leaf",
        SessionEntry::SessionInfo(_) => "session_info",
        SessionEntry::Custom(_) => "custom",
    }
}
