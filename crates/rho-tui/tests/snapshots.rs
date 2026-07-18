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
    ApiKeyLoginModal, BranchSummaryModal, CommandOutputModal, CustomProviderLoginModal,
    ExtensionConfirmModal, ExtensionInputModal, ExtensionSelectModal, LoginMethod,
    LoginMethodPickerModal, LoginProviderItem, LoginProviderPickerModal, Modal, ModelPickerKind,
    ModelPickerModal, NoticeModal, OAuthLoginModal, ProviderPickerPurpose, SessionPickerModal,
    ThemePickerModal, TreePickerModal,
};
use rho_tui::motion::MotionCaps;
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

#[test]
fn snapshot_transcript_follows_bottom_when_overflowing() {
    // C3 regression: a transcript taller than the pane must show its TAIL (newest
    // turns), not clip below the fold. Mirrors app.rs render_transcript_scrolled:
    // a bottom-anchored scroll offset over build_transcript_lines.
    use rho_tui::widgets::build_transcript_lines;
    let mut state = TuiState::new();
    for i in 1..=12 {
        state.add_item(rho_tui::ChatItemRole::User, format!("message number {i}"));
    }
    let theme = dark();
    let rendered = render_to_string(40, 6, |frame| {
        let area = full(frame);
        let lines = build_transcript_lines(&state, &theme, area.width);
        let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let offset = total.saturating_sub(area.height);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(lines).scroll((offset, 0)),
            area,
        );
    });
    // The newest message must be visible; the oldest must have scrolled off.
    assert!(
        rendered.contains("message number 12"),
        "tail must be visible"
    );
    assert!(
        !rendered.contains("message number 1\n"),
        "head must scroll off"
    );
    insta::assert_snapshot!("transcript_follows_bottom", rendered);
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
fn snapshot_rho_splash() {
    // The rho welcome splash on a fresh (empty) transcript, in the default rho
    // identity theme: the ρ mark, the animated π → τ → ρ heritage lineage, the
    // pitch, a real benchmark brag, the hints row, and a rotating fact. Rendered
    // with plain (no-motion) caps at frame 0 so the lineage settles on ρ and the
    // snapshot is deterministic.
    let theme = get_tui_theme(TuiThemeName::Rho);
    let rendered = render_to_string(80, 18, |frame| {
        let area = full(frame);
        rho_tui::widgets::render_splash(frame, area, &theme, 0, MotionCaps::plain());
    });
    insta::assert_snapshot!("rho_splash", rendered);
}

#[test]
fn splash_fills_entire_pane_with_theme_background() {
    // Regression for the "half-screen theme" bug: the splash must paint the theme
    // background across the WHOLE pane (no black seam above the centered block).
    // Verified at several terminal sizes.
    let theme = get_tui_theme(TuiThemeName::Rho);
    let expected = ratatui::style::Color::Rgb(0x0e, 0x0c, 0x0b); // rho transcript bg
    for (w, h) in [(48u16, 10u16), (80, 24), (100, 40), (120, 30)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = full(frame);
                rho_tui::widgets::render_splash(frame, area, &theme, 0, MotionCaps::plain());
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        for y in 0..h {
            for x in 0..w {
                let bg = buffer.cell((x, y)).expect("cell").bg;
                assert_eq!(
                    bg, expected,
                    "cell ({x},{y}) at {w}x{h} is not the theme background — seam!"
                );
            }
        }
    }
}

#[test]
fn snapshot_working_status_line() {
    // The working-state signature line (plain caps → the shimmer verb is a single
    // span, so the text is stable): `Tempering…  ·  2m 14s  ·  esc to interrupt`.
    let theme = get_tui_theme(TuiThemeName::Rho);
    let rendered = render_to_string(60, 1, |frame| {
        let area = full(frame);
        rho_tui::widgets::render_working_status(
            frame,
            area,
            "Tempering",
            134,
            0,
            "esc",
            MotionCaps::plain(),
            &theme,
        );
    });
    insta::assert_snapshot!("working_status_line", rendered);
}

#[test]
fn bench_brag_cites_real_committed_numbers() {
    // The brag is computed from the committed benchmarks.json (baked in), not
    // hardcoded, and reads as the expected shape.
    let brag = rho_tui::widgets::bench_brag_line().expect("brag");
    assert!(brag.starts_with("ρ · ~"), "{brag}");
    assert!(brag.contains("× faster cold start than τ"), "{brag}");
    assert!(brag.contains("× lighter"), "{brag}");
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

#[test]
fn snapshot_login_method_picker() {
    snapshot_modal(
        "modal_login_method_picker",
        Modal::LoginMethodPicker(LoginMethodPickerModal::new()),
    );
}

#[test]
fn snapshot_login_provider_picker() {
    let providers = vec![
        LoginProviderItem::new("anthropic", "Anthropic (Claude Pro/Max)"),
        LoginProviderItem::new("openai-codex", "OpenAI Codex (ChatGPT)"),
        LoginProviderItem::new("github-copilot", "GitHub Copilot"),
    ];
    snapshot_modal(
        "modal_login_provider_picker",
        Modal::LoginProviderPicker(LoginProviderPickerModal::new(
            providers,
            ProviderPickerPurpose::Login {
                method: LoginMethod::Subscription,
            },
            "Login",
        )),
    );
}

#[test]
fn snapshot_api_key_login() {
    snapshot_modal(
        "modal_api_key_login",
        Modal::ApiKeyLogin(ApiKeyLoginModal::new("OpenAI")),
    );
}

#[test]
fn snapshot_oauth_login() {
    let mut modal = OAuthLoginModal::new("Anthropic (Claude Pro/Max)");
    modal.set_auth(
        "https://claude.ai/oauth/authorize?client_id=demo".to_string(),
        Some("Complete login in your browser.".to_string()),
    );
    snapshot_modal("modal_oauth_login", Modal::OAuthLogin(modal));
}

#[test]
fn snapshot_oauth_login_device_code() {
    let mut modal = OAuthLoginModal::new("GitHub Copilot");
    modal.set_device_code("https://github.com/login/device".to_string(), "ABCD-1234");
    snapshot_modal("modal_oauth_login_device", Modal::OAuthLogin(modal));
}

#[test]
fn snapshot_custom_provider_login() {
    snapshot_modal(
        "modal_custom_provider_login",
        Modal::CustomProviderLogin(CustomProviderLoginModal::new()),
    );
}

#[test]
fn snapshot_extension_select() {
    snapshot_modal(
        "modal_extension_select",
        Modal::ExtensionSelect(ExtensionSelectModal::new(
            "Pick a branch",
            vec!["main".to_string(), "develop".to_string()],
        )),
    );
}

#[test]
fn snapshot_extension_confirm() {
    snapshot_modal(
        "modal_extension_confirm",
        Modal::ExtensionConfirm(ExtensionConfirmModal::new(
            "Proceed?",
            "This will overwrite the file.",
        )),
    );
}

#[test]
fn snapshot_extension_input() {
    snapshot_modal(
        "modal_extension_input",
        Modal::ExtensionInput(ExtensionInputModal::new("Enter name", "e.g. feature-x")),
    );
}
