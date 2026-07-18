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
        manager.get_session(&record.id).unwrap().is_none(),
        "not indexed before the first durable write"
    );
    drain(session.prompt("go".to_string(), None)).await;
    assert!(
        manager.get_session(&record.id).unwrap().is_some(),
        "indexed after the first durable write"
    );
}

/// `/session` reports tau's "Session name: {title}" line only when the active
/// session is indexed *and* named — driven through a real `CodingSession` +
/// `SessionManager` (not a fake) so the `session_title()` wiring is exercised
/// end to end. The prior fake-session test hard-set the title on the fake and
/// masked that `CodingSession::session_title` was stubbed to `None`.
#[tokio::test]
async fn session_command_reports_indexed_title_for_a_real_session() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let manager = SessionManager::new(RhoPaths::new(
        tmp.path().join(".rho"),
        tmp.path().join(".agents"),
    ));
    // Index an untitled record so `get_session` resolves but `title` is None.
    let record = manager.prepare_session(&cwd, "fake", Some("fake"), None, None);
    manager.index_session(&record);

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(&record.path);
    let mut config = pinned_config(provider, storage, cwd.clone());
    config.session_id = Some(record.id.clone());
    config.session_manager = Some(manager.clone());
    let mut session = CodingSession::load(config).await.unwrap();

    // Untitled: the "Session:" id line is present, "Session name:" is omitted
    // (tau commands.py:418-420 gates the name line on a truthy title).
    let message = session.handle_command("/session").message.expect("message");
    assert!(message.contains(&format!("Session: {}", record.id)));
    assert!(
        !message.contains("Session name:"),
        "untitled session must omit the name line:\n{message}"
    );

    // Name it via the manager index; `session_title()` reads the record live,
    // so the very next `/session` now emits tau's "Session name:" line.
    manager
        .touch_session(&record.id, None, None, Some("Customer bugfix"))
        .expect("record exists");
    let message = session.handle_command("/session").message.expect("message");
    assert!(
        message.contains("Session name: Customer bugfix"),
        "named session must include the title line:\n{message}"
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

// ---------------------------------------------------------------------------
// dispatch-2 provider/model/thinking/export/reload surface
// ---------------------------------------------------------------------------

use rho_coding::ModelChoice;
use rho_coding::context_window::DEFAULT_CONTEXT_WINDOW_TOKENS;
use rho_coding::credentials::FileCredentialStore;
use rho_coding::provider_config::{ProviderSettings, builtin_provider_configs};
use rho_coding::resources::RhoResourcePaths;

/// A hermetic resource-paths root under `home`, with an empty project cwd so
/// skill/prompt/context discovery finds nothing.
fn temp_resource_paths(home: &Path, cwd: &Path) -> RhoResourcePaths {
    let rho_home = home.join(".rho");
    let agents_home = home.join(".agents");
    std::fs::create_dir_all(&rho_home).unwrap();
    RhoResourcePaths {
        root: rho_home.clone(),
        cwd: Some(cwd.to_path_buf()),
        agents_root: Some(agents_home.clone()),
        paths: Some(RhoPaths::new(rho_home, agents_home)),
    }
}

/// Provider settings backed by the built-in catalog, defaulting to `openai`.
fn openai_provider_settings() -> ProviderSettings {
    ProviderSettings {
        default_provider: "openai".to_string(),
        providers: builtin_provider_configs(),
        scoped_models: Vec::new(),
    }
}

/// Write a plain API-key credential into the hermetic store so the provider is
/// reported usable.
fn write_openai_credential(home: &Path) {
    let rho_home = home.join(".rho");
    std::fs::create_dir_all(&rho_home).unwrap();
    let store = FileCredentialStore::new(rho_home.join("credentials.json"));
    store.set("openai", "sk-test-key").unwrap();
}

fn provider_aware_config(
    provider: Arc<dyn ModelProvider>,
    storage: Arc<dyn SessionStorage>,
    cwd: PathBuf,
    home: &Path,
    model: &str,
) -> CodingSessionConfig {
    let resource_paths = temp_resource_paths(home, &cwd);
    let mut config = CodingSessionConfig::new(provider, model, storage, cwd);
    config.clock = Arc::new(FixedClock::fixture());
    config.id_gen = Arc::new(SequentialIdGen::new());
    config.provider_name = "openai".to_string();
    config.provider_settings = Some(openai_provider_settings());
    config.resource_paths = Some(resource_paths);
    config
}

#[tokio::test]
async fn context_window_tokens_reads_active_provider_then_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    // With provider settings: the active provider's per-model window wins.
    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("with.jsonl"));
    let config = provider_aware_config(provider, storage, cwd.clone(), tmp.path(), "gpt-4");
    let session = CodingSession::load(config).await.unwrap();
    assert_eq!(session.model(), "gpt-4");
    assert_eq!(session.context_window_tokens(), 8192);

    // Without provider settings: rho's fixed fallback.
    let provider2 = Arc::new(FakeProvider::new(vec![]));
    let storage2 = jsonl_session_storage(tmp.path().join("without.jsonl"));
    let config2 = pinned_config(provider2, storage2, cwd);
    let session2 = CodingSession::load(config2).await.unwrap();
    assert_eq!(
        session2.context_window_tokens(),
        DEFAULT_CONTEXT_WINDOW_TOKENS
    );
}

#[tokio::test]
async fn available_models_and_providers_reflect_provider_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    write_openai_credential(tmp.path());

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("s.jsonl"));
    let config = provider_aware_config(provider, storage, cwd.clone(), tmp.path(), "gpt-4");
    let session = CodingSession::load(config).await.unwrap();

    let providers = session.available_providers();
    assert!(
        providers.iter().any(|name| name == "openai"),
        "usable openai provider is listed: {providers:?}"
    );
    let models = session.available_models();
    assert!(
        models.iter().any(|model| model == "gpt-4"),
        "active provider models are listed: {models:?}"
    );
    assert!(
        session
            .available_model_choices()
            .contains(&ModelChoice::new("openai", "gpt-4")),
        "provider/model choices include openai:gpt-4"
    );

    // Without provider settings the accessors collapse to the fixed pair.
    let bare_provider = Arc::new(FakeProvider::new(vec![]));
    let bare_storage = jsonl_session_storage(tmp.path().join("s2.jsonl"));
    let bare_config = pinned_config(bare_provider, bare_storage, cwd);
    let bare_session = CodingSession::load(bare_config).await.unwrap();
    assert_eq!(bare_session.available_providers(), vec!["fake".to_string()]);
    assert_eq!(bare_session.available_models(), vec!["fake".to_string()]);
}

#[tokio::test]
async fn set_model_updates_harness_model_and_validates() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    write_openai_credential(tmp.path());

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("s.jsonl"));
    let config = provider_aware_config(provider, storage, cwd, tmp.path(), "gpt-4");
    let mut session = CodingSession::load(config).await.unwrap();

    // A configured model switches the live harness model.
    session.set_model("gpt-4o").unwrap();
    assert_eq!(session.model(), "gpt-4o");

    // An unconfigured model is rejected and leaves the model unchanged.
    let err = session.set_model("nonexistent-model-xyz").unwrap_err();
    assert!(
        err.to_string().contains("nonexistent-model-xyz"),
        "validation error names the bad model: {err}"
    );
    assert_eq!(session.model(), "gpt-4o");
}

#[tokio::test]
async fn export_writes_an_html_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let session_path = tmp.path().join("session.jsonl");

    let provider = Arc::new(FakeProvider::new(vec![text_turn("exported answer")]));
    let storage = jsonl_session_storage(&session_path);
    let config = pinned_config(provider, storage, cwd);
    let mut session = CodingSession::load(config).await.unwrap();
    drain(session.prompt("hi".to_string(), None)).await;

    let destination = tmp.path().join("out.html");
    let written = session
        .export(Some(destination.clone()), None)
        .await
        .unwrap();
    assert_eq!(written, destination);
    let html = std::fs::read_to_string(&destination).unwrap();
    assert!(!html.is_empty(), "export writes a non-empty artifact");
    assert!(
        html.to_lowercase().contains("<!doctype html") || html.contains("<html"),
        "export writes HTML"
    );
}

#[tokio::test]
async fn reload_reports_unchanged_when_nothing_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("s.jsonl"));
    // Hermetic, empty resource root: nothing to load, nothing changes.
    let resource_paths = temp_resource_paths(tmp.path(), &cwd);
    let mut config = pinned_config(provider, storage, cwd);
    config.resource_paths = Some(resource_paths);
    let mut session = CodingSession::load(config).await.unwrap();

    let summary = session.reload().await.unwrap();
    assert!(!summary.skills.changed);
    assert!(!summary.prompt_templates.changed);
    assert!(!summary.context_files.changed);
    assert!(!summary.extensions.changed);
    assert!(!summary.diagnostics.changed);
    assert!(!summary.system_prompt_rebuilt);
    assert_eq!(summary.skills.before, summary.skills.after);
}

// ---------------------------------------------------------------------------
// Dispatch-2 command handling + resource expansion (tau test_coding_session.py
// `test_minimal_commands_are_handled`, `test_session_loads_and_expands_skills`,
// `test_session_expands_prompt_templates_as_slash_commands`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handle_command_routes_minimal_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("session.jsonl"));
    let config = pinned_config(provider, storage, cwd);
    let mut session = CodingSession::load(config).await.unwrap();

    // A bare prompt and unregistered/unknown commands are unhandled; the
    // registered commands set their result flags (tau parity).
    assert!(!session.handle_command("hello").handled);
    assert!(session.handle_command("/new").new_session_requested);
    assert!(!session.handle_command("/clear").handled); // not a registered command
    assert!(session.handle_command("/quit").exit_requested);
    assert!(session.handle_command("/exit").exit_requested); // alias of /quit
    assert!(!session.handle_command("/unknown").handled);
    // A `/skill:` command is left unhandled here — it expands via prompt().
    assert!(!session.handle_command("/skill:foo").handled);
}

#[tokio::test]
async fn expand_prompt_text_expands_a_loaded_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    // A skill is a directory containing SKILL.md (Agent Skills spec, ADR 0003).
    let skill_dir = tmp.path().join(".rho/skills/refactor");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Refactor helper\n---\nRefactor the target module carefully.\n",
    )
    .unwrap();

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("session.jsonl"));
    let config = provider_aware_config(provider, storage, cwd, tmp.path(), "gpt-4");
    let session = CodingSession::load(config).await.unwrap();

    assert_eq!(session.skills().len(), 1, "the SKILL.md is discovered");
    let expanded = session.expand_prompt_text("/skill:refactor").unwrap();
    assert!(
        expanded.starts_with("<skill name=\"refactor\" location=\""),
        "skill command expands into an invocation block: {expanded}"
    );
    assert!(expanded.contains("Refactor the target module carefully."));
    // A non-skill, non-template prompt passes through unchanged.
    assert_eq!(
        session.expand_prompt_text("just a prompt").unwrap(),
        "just a prompt"
    );
    // An unknown skill is a ResourceError (tau raises a ValueError).
    assert!(session.expand_prompt_text("/skill:missing").is_err());
}

#[tokio::test]
async fn expand_prompt_text_renders_a_prompt_template() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let prompts_dir = tmp.path().join(".rho/prompts");
    std::fs::create_dir_all(&prompts_dir).unwrap();
    std::fs::write(prompts_dir.join("greet.md"), "Hello {{ args }}, welcome.").unwrap();

    let provider = Arc::new(FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("session.jsonl"));
    let config = provider_aware_config(provider, storage, cwd, tmp.path(), "gpt-4");
    let session = CodingSession::load(config).await.unwrap();

    assert_eq!(session.prompt_templates().len(), 1);
    assert_eq!(
        session.expand_prompt_text("/greet world").unwrap(),
        "Hello world, welcome."
    );
}

// ---------------------------------------------------------------------------
// M4b-2 review round: print-mode indexing (C1) + compaction diagnostics (I2)
// ---------------------------------------------------------------------------

use rho_coding::{SessionPrintModeConfig, run_session_print_mode};

/// C1 — the default print path persists + indexes a session (tau
/// `run_openai_print_mode`), so `rho sessions` lists a `rho -p` run. Before this
/// fix the print session had no manager/id, so nothing was indexed.
#[tokio::test]
async fn print_mode_indexes_a_default_session() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let manager = SessionManager::new(RhoPaths::new(
        tmp.path().join(".rho"),
        tmp.path().join(".agents"),
    ));

    // Two scripted turns: the agent response and the session auto-naming call
    // (an indexed session with one user message is auto-named — a second
    // provider call, exactly like tau's `run_openai_print_mode`).
    let provider = Arc::new(FakeProvider::new(vec![
        text_turn("done"),
        text_turn("Session title"),
    ]));
    let mut config = SessionPrintModeConfig::new("hello there", "fake", cwd.clone(), provider);
    config.provider_name = "fake".to_string();
    config.clock = Arc::new(FixedClock::fixture());
    config.id_gen = Arc::new(SequentialIdGen::new());
    // Inject the temp-dir manager (no `session_path` → the default indexed path).
    config.session_manager = Some(manager.clone());

    assert!(run_session_print_mode(config).await, "print run succeeds");

    let sessions = manager.list_sessions(Some(&cwd)).unwrap();
    assert_eq!(sessions.len(), 1, "the print run is indexed and listed");
    assert_eq!(sessions[0].model, "fake");
    assert!(
        sessions[0].path.exists(),
        "transcript persisted at the record path"
    );
}

/// I2 — a failing automatic compaction records a diagnostic and stashes its path
/// (tau `_try_auto_compact` phase `auto_compact_after_prompt`) instead of being
/// silently dropped. A huge assistant reply pushes an older message past the
/// 20k keep-recent budget so compaction has something to summarize; the summary
/// call is unscripted, so the fake replays an empty stream and summarization
/// fails.
#[tokio::test]
async fn failing_auto_compaction_records_a_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cwd = home.join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    let huge = "x ".repeat(60_000); // ~120k chars ≈ 30k tokens > 20k keep-recent
    let provider = Arc::new(FakeProvider::new(vec![text_turn(&huge)]));
    let storage = jsonl_session_storage(tmp.path().join("session.jsonl"));
    let mut config = provider_aware_config(provider, storage, cwd, home, "gpt-4");
    config.auto_compact_token_threshold = Some(1);

    let mut session = CodingSession::load(config).await.unwrap();
    assert!(session.last_diagnostic_log_path().is_none());

    drain(session.prompt("go".to_string(), None)).await;

    let path = session
        .last_diagnostic_log_path()
        .expect("a failed auto-compaction records a diagnostic path");
    assert!(
        path.exists(),
        "diagnostic file written at {}",
        path.display()
    );
    let logged = std::fs::read_to_string(path).unwrap();
    assert!(
        logged.contains("auto_compact_after_prompt"),
        "diagnostic records the failing phase:\n{logged}"
    );
}

// ---------------------------------------------------------------------------
// Login-required placeholder (tau parity): the TUI installs a
// `LoginRequiredProvider` when no credential is available at launch; prompting
// while locked surfaces the login message, and picking a credentialed
// provider/model swaps in a real provider (the model-picker / login unlock).
// ---------------------------------------------------------------------------

const LOGIN_REQUIRED_MESSAGE: &str = "Login required. Run /login to choose a provider, \
     or /login openai to continue with the current provider.";

#[tokio::test]
async fn login_required_placeholder_reports_the_login_message_when_prompted() {
    use rho_agent::messages::AgentMessage;
    use rho_coding::login_required::LoginRequiredProvider;

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    let provider: Arc<dyn ModelProvider> =
        Arc::new(LoginRequiredProvider::new(LOGIN_REQUIRED_MESSAGE));
    let storage = jsonl_session_storage(tmp.path().join("s.jsonl"));
    let config = provider_aware_config(provider, storage, cwd, tmp.path(), "gpt-4");
    let mut session = CodingSession::load(config).await.unwrap();

    // Prompting while locked yields the polite login error in the transcript
    // instead of crashing (tau parity).
    drain(session.prompt("hello".to_string(), None)).await;
    let has_login_error = session.messages().iter().any(|message| {
        matches!(
            message,
            AgentMessage::Assistant(assistant)
                if assistant.stop_reason == StopReason::Error
                    && assistant.error_message.as_deref() == Some(LOGIN_REQUIRED_MESSAGE)
        )
    });
    assert!(
        has_login_error,
        "the login-required message is surfaced as an assistant error: {:?}",
        session.messages()
    );
}

#[tokio::test]
async fn login_required_placeholder_unlocks_after_provider_swap() {
    use rho_coding::login_required::LoginRequiredProvider;

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    // A usable credential for `openai` lives in the hermetic store, so swapping
    // to it builds a real provider.
    write_openai_credential(tmp.path());

    let provider: Arc<dyn ModelProvider> =
        Arc::new(LoginRequiredProvider::new(LOGIN_REQUIRED_MESSAGE));
    let storage = jsonl_session_storage(tmp.path().join("s.jsonl"));
    let config = provider_aware_config(provider, storage, cwd, tmp.path(), "gpt-4");
    let mut session = CodingSession::load(config).await.unwrap();

    // Picking a credentialed provider/model swaps the placeholder for a real
    // provider (the same path `/login` and the model picker drive).
    session
        .set_model_choice(&ModelChoice::new("openai", "gpt-4o"))
        .expect("swapping to a credentialed provider/model succeeds");
    assert_eq!(session.provider_name(), "openai");
    assert_eq!(session.model(), "gpt-4o");
}
