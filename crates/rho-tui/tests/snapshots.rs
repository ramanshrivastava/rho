//! Render snapshots via ratatui's `TestBackend` (insta).
//!
//! Every widget + modal is rendered into a fixed-size test buffer and its text
//! grid is snapshotted, so a visual regression in the immediate-mode render
//! layer shows up as a diff. The transcript snapshot drives the *real*
//! [`TuiEventAdapter`] with a hand-built session-event sequence (the same seam
//! the live app uses), so the transcript text is exactly what a session produces.
//!
//! Snapshots are committed under `tests/snapshots/`. Regenerate intentionally
//! with `INSTA_UPDATE=always cargo test -p rho-tui --test snapshots`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use rho_agent::events::{
    AgentEvent, AgentStartEvent, MessageEndEvent, ToolExecutionEndEvent, ToolExecutionStartEvent,
};
use rho_agent::messages::{
    AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolResultContent,
    ToolResultMessage, UserMessage,
};
use rho_agent::tools::AgentToolResult;
use rho_agent::types::{JsonMap, JsonValue};
use rho_coding::events::CodingSessionEvent;
use rho_coding::session::{ModelChoice, SessionTreeChoice};
use rho_coding::session_manager::CodingSessionRecord;

use rho_tui::TuiEventAdapter;
use rho_tui::autocomplete::{CompletionItem, CompletionState};
use rho_tui::modals::{
    BranchSummaryModal, CommandOutputModal, Modal, ModelPickerKind, ModelPickerModal, NoticeModal,
    SessionPickerModal, ThemePickerModal, TreePickerModal,
};
use rho_tui::state::TuiState;
use rho_tui::theme::{TuiKeybindings, TuiThemeName, get_tui_theme};
use rho_tui::widgets::footer::FooterMode;
use rho_tui::widgets::sidebar::SidebarInfo;
use rho_tui::widgets::status::StatusInfo;
use rho_tui::widgets::{
    render_compact_session_info, render_completion_popup, render_footer, render_queued_messages,
    render_sidebar, render_transcript,
};

/// Render a closure into a `width`×`height` test buffer and return the text grid
/// (trailing whitespace trimmed per line for stable snapshots).
fn render_to_string<F>(width: u16, height: u16, draw: F) -> String
where
    F: FnOnce(&mut ratatui::Frame),
{
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| draw(frame)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut row = String::with_capacity(width as usize);
        for x in 0..width {
            row.push_str(
                buffer
                    .cell((x, y))
                    .map_or(" ", ratatui::buffer::Cell::symbol),
            );
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

fn dark() -> rho_tui::theme::TuiTheme {
    get_tui_theme(TuiThemeName::TauDark)
}

fn full(area: &mut ratatui::Frame) -> Rect {
    area.area()
}

// --- transcript (real adapter) ----------------------------------------------

fn tool_args() -> JsonMap {
    let mut m = JsonMap::new();
    m.insert("path".to_string(), JsonValue::from("notes.md"));
    m
}

/// Build a small transcript by driving the real adapter, exactly like the app.
fn conversation_state() -> TuiState {
    let mut state = TuiState::new();
    state.show_tool_results = true;
    let mut adapter = TuiEventAdapter::new(&mut state);
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::AgentStart(
        AgentStartEvent::new(),
    )));
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::MessageEnd(
        MessageEndEvent::new(AgentMessage::User(UserMessage::new(
            "Summarize the project structure.",
        ))),
    )));
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::ToolExecutionStart(
        ToolExecutionStartEvent::new("call-1", "read", tool_args()),
    )));
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::ToolExecutionEnd(
        ToolExecutionEndEvent::new(
            "call-1",
            "read",
            AgentToolResult::new(vec![ToolResultContent::Text(TextContent::new(
                "# Notes\nline one\nline two",
            ))]),
            false,
        ),
    )));
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::MessageEnd(
        MessageEndEvent::new(AgentMessage::ToolResult(ToolResultMessage::new(
            "call-1",
            "read",
            vec![ToolResultContent::Text(TextContent::new("ok"))],
        ))),
    )));
    adapter.apply(&CodingSessionEvent::Agent(AgentEvent::MessageEnd(
        MessageEndEvent::new(AgentMessage::Assistant(AssistantMessage::new(vec![
            AssistantContent::Text(TextContent::new(
                "The project has a **notes** file with two lines.\n\n- one\n- two",
            )),
        ]))),
    )));
    state
}

#[test]
fn snapshot_transcript() {
    let state = conversation_state();
    let theme = dark();
    let rendered = render_to_string(60, 16, |frame| {
        let area = full(frame);
        render_transcript(frame, area, &state, &theme);
    });
    insta::assert_snapshot!("transcript", rendered);
}

// --- chrome widgets ---------------------------------------------------------

fn status_info() -> StatusInfo {
    StatusInfo {
        cwd: std::path::PathBuf::from("/work/project"),
        provider_name: "anthropic".to_string(),
        model: "claude".to_string(),
        thinking_display: "medium".to_string(),
        context_token_estimate: 12_400,
        context_window_tokens: 128_000,
        auto_compact_token_threshold: None,
        git_branch: "main".to_string(),
    }
}

#[test]
fn snapshot_status_line() {
    let info = status_info();
    let theme = dark();
    let rendered = render_to_string(70, 1, |frame| {
        let area = full(frame);
        render_compact_session_info(frame, area, &info, &theme);
    });
    insta::assert_snapshot!("status_line", rendered);
}

#[test]
fn snapshot_sidebar() {
    let info = SidebarInfo {
        provider_name: "anthropic".to_string(),
        model: "claude".to_string(),
        thinking_display: "medium".to_string(),
        tools_count: 2,
        skills_count: 1,
        context_labels: vec!["AGENTS.md".to_string()],
        tool_names: vec!["bash".to_string(), "read".to_string()],
        skill_names: vec!["review".to_string()],
        prompt_names: vec![],
    };
    let theme = dark();
    let rendered = render_to_string(32, 20, |frame| {
        let area = full(frame);
        render_sidebar(frame, area, &info, &theme);
    });
    insta::assert_snapshot!("sidebar", rendered);
}

#[test]
fn snapshot_footer_modes() {
    let kb = TuiKeybindings::default();
    let theme = dark();
    for (mode, name) in [
        (FooterMode::Normal, "footer_normal"),
        (FooterMode::Completion, "footer_completion"),
        (FooterMode::Running, "footer_running"),
    ] {
        let rendered = render_to_string(80, 1, |frame| {
            let area = full(frame);
            render_footer(frame, area, mode, &kb, &theme);
        });
        insta::assert_snapshot!(name, rendered);
    }
}

#[test]
fn snapshot_completion_popup() {
    let theme = dark();
    let items = vec![
        CompletionItem {
            display: "/clear".to_string(),
            replacement: "/clear".to_string(),
            start: 0,
            end: 0,
            description: Some("Clear the transcript".to_string()),
            category: Some("Commands".to_string()),
        },
        CompletionItem {
            display: "/compact".to_string(),
            replacement: "/compact".to_string(),
            start: 0,
            end: 0,
            description: Some("Compact the context".to_string()),
            category: Some("Commands".to_string()),
        },
    ];
    let state = CompletionState::new(items);
    let rendered = render_to_string(50, 4, |frame| {
        let area = full(frame);
        render_completion_popup(frame, area, &state, &theme);
    });
    insta::assert_snapshot!("completion_popup", rendered);
}

#[test]
fn snapshot_queued_messages() {
    let theme = dark();
    let steering = vec!["also handle errors".to_string()];
    let follow_up = vec!["then write tests".to_string()];
    let rendered = render_to_string(60, 2, |frame| {
        let area = full(frame);
        render_queued_messages(frame, area, &steering, &follow_up, &theme);
    });
    insta::assert_snapshot!("queued_messages", rendered);
}

// --- modals -----------------------------------------------------------------

fn session_records() -> Vec<CodingSessionRecord> {
    vec![
        CodingSessionRecord {
            id: "sess-001".into(),
            path: "/work/.rho/sess-001.jsonl".into(),
            cwd: "/work".into(),
            model: "claude".into(),
            title: Some("Refactor parser".into()),
            created_at: 0.0,
            updated_at: 0.0,
            provider_name: Some("anthropic".into()),
        },
        CodingSessionRecord {
            id: "sess-002".into(),
            path: "/work/.rho/sess-002.jsonl".into(),
            cwd: "/work".into(),
            model: "claude".into(),
            title: None,
            created_at: 0.0,
            updated_at: 0.0,
            provider_name: Some("anthropic".into()),
        },
    ]
}

// Takes the modal by value so call sites can pass a freshly-built `Modal` inline;
// rendering only borrows it.
#[allow(clippy::needless_pass_by_value)]
fn snapshot_modal(name: &str, modal: Modal) {
    let theme = dark();
    let rendered = render_to_string(70, 18, |frame| {
        let area = full(frame);
        modal.render(frame, area, &theme);
    });
    insta::assert_snapshot!(name, rendered);
}

#[test]
fn snapshot_session_picker() {
    snapshot_modal(
        "modal_session_picker",
        Modal::SessionPicker(SessionPickerModal::new(session_records())),
    );
}

#[test]
fn snapshot_theme_picker() {
    snapshot_modal(
        "modal_theme_picker",
        Modal::ThemePicker(ThemePickerModal::new(TuiThemeName::TauDark)),
    );
}

#[test]
fn snapshot_model_picker() {
    let choices = vec![
        ModelChoice::new("anthropic", "claude-opus"),
        ModelChoice::new("anthropic", "claude-sonnet"),
    ];
    let scoped = vec![ModelChoice::new("anthropic", "claude-sonnet")];
    snapshot_modal(
        "modal_model_picker",
        Modal::ModelPicker(ModelPickerModal::new(
            choices,
            scoped,
            "claude-opus".into(),
            "anthropic".into(),
            ModelPickerKind::Model,
        )),
    );
}

#[test]
fn snapshot_tree_picker() {
    let choices = vec![
        SessionTreeChoice {
            entry_id: "e1".into(),
            label: "user: start".into(),
            active: false,
            is_tool_call: false,
        },
        SessionTreeChoice {
            entry_id: "e2".into(),
            label: "assistant: reply".into(),
            active: true,
            is_tool_call: false,
        },
    ];
    snapshot_modal(
        "modal_tree_picker",
        Modal::TreePicker(TreePickerModal::new(choices)),
    );
}

#[test]
fn snapshot_branch_summary() {
    snapshot_modal(
        "modal_branch_summary",
        Modal::BranchSummaryInstructions(BranchSummaryModal::new("e1".into())),
    );
}

#[test]
fn snapshot_command_output() {
    snapshot_modal(
        "modal_command_output",
        Modal::CommandOutput(CommandOutputModal::new(
            "Reload",
            "Reloaded 3 skills\nReloaded 2 prompt templates",
        )),
    );
}

#[test]
fn snapshot_notice_m7() {
    snapshot_modal(
        "modal_notice_m7",
        Modal::Notice(NoticeModal::m7("Login / logout")),
    );
}
