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
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
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
use rho_agent::session::storage::{SessionStorage, SessionStorageError};
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

    // Assert the durable append-only structure this test claims to cover: each
    // message entry chains off the previous leaf, and every message is followed
    // by a leaf pointing at it (the durable-message boundary).
    let text = std::fs::read_to_string(&session_path).unwrap();
    let entries: Vec<SessionEntry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| entry_from_json_line(l, None).unwrap())
        .collect();
    let messages: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| matches!(e, SessionEntry::Message(_)))
        .collect();
    assert_eq!(messages.len(), 4);
    for pair in messages.windows(2) {
        // The next message's parent is the previous message entry (the leaf that
        // was written between them points at the same id, so the chain is linear).
        assert_eq!(
            pair[1].parent_id(),
            Some(pair[0].id()),
            "each message entry chains off its predecessor"
        );
    }
    // Every message is immediately followed by a leaf whose entry_id == its id.
    for (i, entry) in entries.iter().enumerate() {
        if let SessionEntry::Message(m) = entry {
            match &entries[i + 1] {
                SessionEntry::Leaf(leaf) => {
                    assert_eq!(leaf.entry_id.as_deref(), Some(m.id.as_str()));
                    assert_eq!(leaf.parent_id.as_deref(), Some(m.id.as_str()));
                }
                other => panic!("message not followed by its leaf: {other:?}"),
            }
        }
    }
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

/// A storage that fails `append` after `fail_after` successful writes, to
/// exercise the persist-failure path.
struct FailAfterStorage {
    inner: rho_agent::session::storage::JsonlSessionStorage,
    calls: AtomicUsize,
    fail_after: usize,
}

#[async_trait]
impl SessionStorage for FailAfterStorage {
    async fn append(&self, entry: &SessionEntry) -> Result<(), SessionStorageError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n >= self.fail_after {
            return Err(SessionStorageError::Io(std::io::Error::other("disk full")));
        }
        self.inner.append(entry).await
    }

    async fn read_all(&self) -> Result<Vec<SessionEntry>, SessionStorageError> {
        self.inner.read_all().await
    }
}

#[tokio::test]
async fn persist_failure_surfaces_run_error_and_skips_settled() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let session_path = tmp.path().join("session.jsonl");

    // Fail on the 4th append: the 3 deferred initial entries succeed, then the
    // first message entry write fails mid-turn.
    let storage = Arc::new(FailAfterStorage {
        inner: rho_agent::session::storage::JsonlSessionStorage::new(&session_path),
        calls: AtomicUsize::new(0),
        fail_after: 3,
    });
    let provider = Arc::new(FakeProvider::new(vec![text_turn("hello")]));
    let mut session = CodingSession::load(pinned_config(provider, storage, cwd))
        .await
        .unwrap();

    let events: Vec<CodingSessionEvent> = {
        let stream = session.prompt("hi".to_string(), None);
        futures::pin_mut!(stream);
        stream.collect().await
    };

    // The turn aborts: the error is recorded (not swallowed) and no
    // `agent_settled` is emitted (tau re-raises before settling).
    assert!(
        session.take_run_error().is_some(),
        "a persistence failure must be recorded on the session"
    );
    let settled = events.iter().any(|e| {
        serde_json::to_string(e)
            .unwrap()
            .contains("\"agent_settled\"")
    });
    assert!(!settled, "no agent_settled event after a persist failure");
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

/// An explicit root leaf (`entry_id: null`, from branching before the first
/// message) must replay to the EMPTY pre-root context on resume — not a linear
/// replay of the abandoned log. This is tau's `from_entries(entries,
/// leaf_id=None)` distinction (Codex P1). We hand-build such a transcript and
/// assert `CodingSession::load` yields no messages.
#[tokio::test]
async fn explicit_root_leaf_replays_to_empty_context() {
    use rho_agent::messages::{AgentMessage, UserMessage};
    use rho_agent::session::entries::{
        LeafEntry, MessageEntry, ModelChangeEntry, SessionInfoEntry, ThinkingLevelChangeEntry,
    };
    use rho_agent::session::jsonl::entry_to_json_line;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("root-leaf.jsonl");

    let mut info = SessionInfoEntry::new();
    info.id = "i".to_string();
    let mut model = ModelChangeEntry::new("fake");
    model.id = "m".to_string();
    model.parent_id = Some("i".to_string());
    let mut think = ThinkingLevelChangeEntry::new(Some("medium".to_string()));
    think.id = "t".to_string();
    think.parent_id = Some("m".to_string());
    let mut user = MessageEntry::new(AgentMessage::User(UserMessage::new("abandoned")));
    user.id = "u".to_string();
    user.parent_id = Some("t".to_string());
    let mut user_leaf = LeafEntry::new(Some("u".to_string()));
    user_leaf.id = "lu".to_string();
    user_leaf.parent_id = Some("u".to_string());
    // The branch-to-root leaf: entry_id = None, parent_id = None.
    let mut root_leaf = LeafEntry::new(None);
    root_leaf.id = "lr".to_string();
    root_leaf.parent_id = None;

    let entries = [
        SessionEntry::SessionInfo(info),
        SessionEntry::ModelChange(model),
        SessionEntry::ThinkingLevelChange(think),
        SessionEntry::Message(user),
        SessionEntry::Leaf(user_leaf),
        SessionEntry::Leaf(root_leaf),
    ];
    let jsonl: String = entries.iter().map(entry_to_json_line).collect();
    std::fs::write(&path, jsonl).unwrap();

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(&path);
    let session = CodingSession::load(pinned_config(provider, storage, tmp.path().to_path_buf()))
        .await
        .unwrap();
    assert!(
        session.messages().is_empty(),
        "explicit root leaf replays to empty context, not the abandoned log"
    );
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
