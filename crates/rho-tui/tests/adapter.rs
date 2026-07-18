//! Port of tau `tests/test_tui_adapter.py` — the parity-critical event seam.
//!
//! Drives the real [`TuiEventAdapter`] with hand-built session events and asserts
//! the resulting [`TuiState`] matches tau's, item for item.

use std::path::PathBuf;

use rho_agent::events::{
    AgentEndEvent, AgentEvent, AgentStartEvent, MessageEndEvent, MessageStartEvent,
    MessageUpdateEvent, ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, StopReason, TextContent, ToolCall,
    ToolResultContent, UserMessage,
};
use rho_agent::provider_events::{AssistantMessageEvent, TextDeltaEvent, ThinkingDeltaEvent};
use rho_agent::tools::AgentToolResult;
use rho_agent::types::{JsonMap, JsonValue};
use rho_coding::events::{AutoRetryStartEvent, CodingSessionEvent, QueueUpdateEvent};
use rho_coding::skills::{Skill, format_skill_invocation};
use rho_tui::state::{format_tool_call_block, format_tool_result_block};
use rho_tui::{ChatItemRole, TuiEventAdapter, TuiState};

// Takes the event by value so call sites can pass a freshly-built event inline
// (`apply(&mut state, agent(...))`); the adapter itself borrows it.
#[allow(clippy::needless_pass_by_value)]
fn apply(state: &mut TuiState, event: CodingSessionEvent) {
    TuiEventAdapter::new(state).apply(&event);
}

fn agent(event: AgentEvent) -> CodingSessionEvent {
    CodingSessionEvent::Agent(event)
}

fn args(pairs: &[(&str, JsonValue)]) -> JsonMap {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn assistant_text(text: &str) -> AssistantMessage {
    AssistantMessage::new(vec![AssistantContent::Text(TextContent::new(text))])
}

fn tool_result(text: &str) -> AgentToolResult {
    AgentToolResult::new(vec![ToolResultContent::Text(TextContent::new(text))])
}

fn roles_and_text(state: &TuiState) -> Vec<(ChatItemRole, String)> {
    state
        .items
        .iter()
        .map(|i| (i.role, i.text.clone()))
        .collect()
}

fn review_skill() -> Skill {
    Skill {
        name: "review".into(),
        path: PathBuf::from("/workspace/.tau/skills/review.md"),
        content: "# Review\nFull instructions.".into(),
        description: Some("Review code".into()),
    }
}

#[test]
fn tracks_running_state() {
    let mut state = TuiState::new();
    apply(
        &mut state,
        agent(AgentEvent::AgentStart(AgentStartEvent::new())),
    );
    assert!(state.running);
    apply(
        &mut state,
        agent(AgentEvent::AgentEnd(AgentEndEvent::new(vec![]))),
    );
    assert!(!state.running);
}

#[test]
fn builds_assistant_item_from_nested_stream_events() {
    let mut state = TuiState::new();
    let partial = AssistantMessage::new(vec![]);

    apply(
        &mut state,
        agent(AgentEvent::MessageStart(MessageStartEvent::new(
            AgentMessage::Assistant(partial.clone()),
        ))),
    );
    for delta in ["Hel", "lo"] {
        apply(
            &mut state,
            agent(AgentEvent::MessageUpdate(MessageUpdateEvent::new(
                AgentMessage::Assistant(partial.clone()),
                AssistantMessageEvent::TextDelta(TextDeltaEvent::new(0, delta, partial.clone())),
            ))),
        );
    }
    assert_eq!(state.assistant_buffer, "Hello");

    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::Assistant(assistant_text("Hello")),
        ))),
    );
    assert_eq!(state.assistant_buffer, "");
    assert_eq!(
        roles_and_text(&state),
        vec![(ChatItemRole::Assistant, "Hello".to_string())]
    );
}

#[test]
fn builds_user_and_compact_skill_items() {
    let skill = review_skill();
    let mut state = TuiState::new();

    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::User(UserMessage::new("Hello Tau")),
        ))),
    );
    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::User(UserMessage::new(format_skill_invocation(
                &skill,
                Some("check auth"),
            ))),
        ))),
    );

    assert_eq!(
        roles_and_text(&state),
        vec![
            (ChatItemRole::User, "Hello Tau".to_string()),
            (ChatItemRole::Skill, "Using skill: review".to_string()),
            (ChatItemRole::User, "check auth".to_string()),
        ]
    );
}

#[test]
fn groups_nested_thinking_deltas() {
    let mut state = TuiState::new();
    let partial = AssistantMessage::new(vec![]);
    for delta in ["hidden ", "reasoning"] {
        apply(
            &mut state,
            agent(AgentEvent::MessageUpdate(MessageUpdateEvent::new(
                AgentMessage::Assistant(partial.clone()),
                AssistantMessageEvent::ThinkingDelta(ThinkingDeltaEvent::new(
                    0,
                    delta,
                    partial.clone(),
                )),
            ))),
        );
    }
    assert_eq!(
        roles_and_text(&state),
        vec![(ChatItemRole::Thinking, "hidden reasoning".to_string())]
    );
    assert!(!state.show_thinking);
}

#[test]
fn records_tool_progress_and_result() {
    let mut state = TuiState::new();
    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionStart(
            ToolExecutionStartEvent::new(
                "call-1",
                "read",
                args(&[("path", JsonValue::from("notes.md"))]),
            ),
        )),
    );
    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionUpdate(
            ToolExecutionUpdateEvent::new(
                "call-1",
                "read",
                args(&[("path", JsonValue::from("notes.md"))]),
                tool_result("reading"),
            ),
        )),
    );
    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionEnd(ToolExecutionEndEvent::new(
            "call-1",
            "read",
            tool_result("done"),
            false,
        ))),
    );

    assert_eq!(state.items.len(), 1);
    let item = &state.items[0];
    assert_eq!(item.role, ChatItemRole::Tool);
    assert_eq!(item.text, "→ read notes.md");
    assert_eq!(item.tool_result_text.as_deref(), Some("✓ read\ndone"));
    assert_eq!(item.update_text, None);
}

#[test]
fn renders_skill_file_reads_with_skill_style() {
    let mut state = TuiState::new();
    state.set_skills([review_skill()]);

    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionStart(
            ToolExecutionStartEvent::new(
                "call-1",
                "read",
                args(&[("path", JsonValue::from("/workspace/.tau/skills/review.md"))]),
            ),
        )),
    );
    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionEnd(ToolExecutionEndEvent::new(
            "call-1",
            "read",
            tool_result("# Review\nFull instructions."),
            false,
        ))),
    );

    assert_eq!(state.items.len(), 1);
    let item = &state.items[0];
    assert_eq!(item.role, ChatItemRole::Skill);
    assert_eq!(item.text, "Loading skill: review");
    assert_eq!(
        item.tool_result_text.as_deref(),
        Some("✓ read\n# Review\nFull instructions.")
    );
}

#[test]
fn records_retry_and_queue_status() {
    let mut state = TuiState::new();
    apply(
        &mut state,
        AutoRetryStartEvent::new(
            2,
            3,
            0,
            "Retrying provider request 2/3 after HTTP 503.".to_string(),
        )
        .into(),
    );
    apply(
        &mut state,
        QueueUpdateEvent::new(vec!["adjust".into()], vec!["after".into()]).into(),
    );

    assert_eq!(
        roles_and_text(&state),
        vec![(
            ChatItemRole::Status,
            "… Retrying provider request 2/3 after HTTP 503.".to_string()
        )]
    );
    assert_eq!(state.queued_steering, vec!["adjust".to_string()]);
    assert_eq!(state.queued_follow_up, vec!["after".to_string()]);
}

#[test]
fn records_assistant_error_and_aborted_message() {
    let mut state = TuiState::new();
    state.running = true;
    state.assistant_buffer = "partial".to_string();

    let mut message = AssistantMessage::new(vec![]);
    message.stop_reason = StopReason::Error;
    message.error_message = Some("provider failed".to_string());
    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::Assistant(message),
        ))),
    );

    assert_eq!(state.error.as_deref(), Some("provider failed"));
    assert_eq!(
        roles_and_text(&state),
        vec![(ChatItemRole::Error, "Error: provider failed".to_string())]
    );
    assert_eq!(state.assistant_buffer, "");
}

#[test]
fn tool_formatters_keep_human_readable_output() {
    let block = format_tool_call_block(&ToolCall::new(
        "call-1",
        "read",
        args(&[
            ("path", JsonValue::from("tests/test_tui_app.py")),
            ("offset", JsonValue::from(1)),
            ("limit", JsonValue::from(80)),
        ]),
    ));
    assert_eq!(block, "→ read tests/test_tui_app.py:1-80");

    let content = (1..12)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let block = format_tool_result_block("read", true, &content, None);
    assert!(block.contains("line 8"));
    assert!(!block.contains("line 9"));
    assert!(block.contains("3 more lines"));
}

#[test]
fn uses_canonical_result_details_for_patch() {
    let mut state = TuiState::new();
    let mut result = AgentToolResult::new(vec![ToolResultContent::Text(TextContent::new(
        "Successfully replaced 1 block.",
    ))]);
    result.details = Some(serde_json::json!({"patch": "--- a.py\n+++ a.py\n@@\n-old\n+new"}));

    apply(
        &mut state,
        agent(AgentEvent::ToolExecutionEnd(ToolExecutionEndEvent::new(
            "call-1", "edit", result, false,
        ))),
    );

    assert!(
        state.items[0]
            .tool_result_text
            .as_deref()
            .unwrap_or("")
            .contains("Patch:\n--- a.py\n+++ a.py")
    );
}

// --- optimistic first-message echo (task #45 latency fix) -------------------

#[test]
fn optimistic_echo_reconciles_without_double_render() {
    // The submit path renders the user's message optimistically (so it appears on
    // the next frame, before prompt()'s stream echoes it back). When the turn's
    // real user MessageEnd arrives with the same text, it must reconcile — not
    // render a duplicate — and clear the pending marker.
    let mut state = TuiState::new();
    state.add_optimistic_user_echo("hello world");
    assert_eq!(
        roles_and_text(&state),
        vec![(ChatItemRole::User, "hello world".into())]
    );
    assert_eq!(state.optimistic_echo.as_deref(), Some("hello world"));

    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::User(UserMessage::new("hello world")),
        ))),
    );
    assert_eq!(
        roles_and_text(&state),
        vec![(ChatItemRole::User, "hello world".into())],
        "the real echo must reconcile, not duplicate"
    );
    assert_eq!(state.optimistic_echo, None);
}

#[test]
fn optimistic_echo_mismatch_still_renders_the_real_message() {
    // Safety net: if the streamed user message does not match the optimistic
    // echo, it is still rendered (the transcript is never wrong — worst case it
    // is the pre-optimistic behavior).
    let mut state = TuiState::new();
    state.add_optimistic_user_echo("first");
    apply(
        &mut state,
        agent(AgentEvent::MessageEnd(MessageEndEvent::new(
            AgentMessage::User(UserMessage::new("second")),
        ))),
    );
    assert_eq!(
        roles_and_text(&state),
        vec![
            (ChatItemRole::User, "first".into()),
            (ChatItemRole::User, "second".into()),
        ]
    );
}
