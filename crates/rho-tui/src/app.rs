//! The interactive TUI application (immediate-mode port of tau's Textual
//! `TauTuiApp` in `app.py`).
//!
//! tau is retained-mode: a persistent widget tree that mutates in place. rho is
//! immediate-mode: every frame is rebuilt by [`render`] from the pure
//! [`TuiState`] plus a [`ChromeSnapshot`] of session metadata, and the only
//! mutator of `TuiState` is [`TuiEventAdapter`] applied to the session event
//! stream. See `dev-notes/phase-5.md` for the retained→immediate re-derivation.
//!
//! ## The async borrow seam
//!
//! [`CodingSession::prompt`] returns a stream that borrows `&mut session` for its
//! whole lifetime, so a turn cannot call `&mut session` chrome methods or a
//! second `prompt`. During a run we therefore (a) drive cancellation / steering /
//! follow-up through the cloned [`HarnessControl`] handle, and (b) render from a
//! [`ChromeSnapshot`] captured before the turn — disjoint field borrows split off
//! `session`. Idle input (model/thinking cycles, pickers, resume) runs with full
//! `&mut session` access between turns.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::{Frame, Terminal};
use tui_textarea::TextArea;

use rho_agent::harness::HarnessControl;
use rho_coding::commands::{CommandRegistry, create_default_command_registry};
use rho_coding::session::{CodingSession, StreamingBehavior};
use rho_coding::session_manager::SessionManager;

use crate::adapter::TuiEventAdapter;
use crate::autocomplete::{CompletionInputs, CompletionState, build_completion_state};
use crate::modals::{
    Modal, ModalOutcome, ModelPickerKind, ModelPickerModal, SessionPickerModal, ThemePickerModal,
};
use crate::state::TuiState;
use crate::theme::{TuiKeybindings, TuiSettings, TuiTheme};
use crate::widgets::status::git_branch;
use crate::widgets::{
    FooterMode, SidebarInfo, StatusInfo, render_compact_session_info, render_completion_popup,
    render_footer, render_prompt_prefix, render_queued_messages, render_sidebar, render_transcript,
};

/// A snapshot of session-derived chrome, captured so the render layer never
/// borrows the live session (which a running turn's stream borrows mutably).
#[derive(Debug, Clone)]
pub struct ChromeSnapshot {
    /// The status-line facts.
    pub status: StatusInfo,
    /// The sidebar facts.
    pub sidebar: SidebarInfo,
}

/// The interactive TUI application state (tau `TauTuiApp`).
pub struct App {
    session: CodingSession,
    control: HarnessControl,
    registry: CommandRegistry,
    state: TuiState,
    settings: TuiSettings,
    theme: TuiTheme,
    textarea: TextArea<'static>,
    completion: CompletionState,
    modal: Option<Modal>,
    chrome: ChromeSnapshot,
    prompt_history: Vec<String>,
    activity_frame: usize,
    should_quit: bool,
    cwd: PathBuf,
}

impl App {
    /// Build an app around a loaded session.
    #[must_use]
    pub fn new(mut session: CodingSession, settings: TuiSettings) -> Self {
        let control = session.control();
        let theme = settings.resolved_theme();
        let cwd = session.cwd().to_path_buf();
        let mut state = TuiState::new();
        state.show_tool_results = false;
        state.show_thinking = true;
        seed_transcript(&mut state, &session);
        let chrome = build_chrome(&mut session, &cwd);
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type a message, /command, or !shell");
        Self {
            registry: create_default_command_registry(),
            control,
            state,
            settings,
            theme,
            textarea,
            completion: CompletionState::default(),
            modal: None,
            chrome,
            prompt_history: Vec::new(),
            activity_frame: 0,
            should_quit: false,
            cwd,
            session,
        }
    }

    /// The full prompt text (all editor lines joined).
    fn prompt_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Replace the editor contents with `text`, cursor at the end.
    fn set_prompt_text(&mut self, text: &str) {
        let mut textarea = TextArea::from(text.split('\n').map(str::to_string).collect::<Vec<_>>());
        textarea.set_placeholder_text("Type a message, /command, or !shell");
        textarea.move_cursor(tui_textarea::CursorMove::End);
        self.textarea = textarea;
    }

    /// Clear the editor and completion state.
    fn clear_prompt(&mut self) {
        self.set_prompt_text("");
        self.completion = CompletionState::default();
    }

    /// Rebuild the completion state from the current prompt text (tau
    /// `_build_completion_state`, recomputed on every keystroke).
    fn rebuild_completion(&mut self) {
        let text = self.prompt_text();
        let model_names = self.session.available_models();
        let provider_names = self.session.available_providers();
        let thinking_levels = self.session.available_thinking_levels();
        let theme_names: Vec<String> = crate::theme::BUILTIN_TUI_THEME_NAMES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let session_ids: Vec<String> = Vec::new();
        let inputs = CompletionInputs {
            skills: self.session.skills(),
            prompt_templates: self.session.prompt_templates(),
            model_names: &model_names,
            provider_names: &provider_names,
            thinking_levels: &thinking_levels,
            theme_names: &theme_names,
            session_ids: &session_ids,
            session_options: &[],
            cwd: Some(&self.cwd),
        };
        self.completion = build_completion_state(&text, &self.registry, &inputs);
    }

    /// Refresh the chrome snapshot from the live session (after a turn or a
    /// model/thinking change).
    fn refresh_chrome(&mut self) {
        self.chrome = build_chrome(&mut self.session, &self.cwd);
    }

    fn footer_mode(&self) -> FooterMode {
        if !self.completion.items.is_empty() {
            FooterMode::Completion
        } else if self.state.running {
            FooterMode::Running
        } else {
            FooterMode::Normal
        }
    }

    /// The current render context (borrows only non-session fields).
    fn render_ctx(&self) -> RenderCtx<'_> {
        RenderCtx {
            state: &self.state,
            textarea: &self.textarea,
            completion: &self.completion,
            theme: &self.theme,
            status: &self.chrome.status,
            sidebar: &self.chrome.sidebar,
            keybindings: &self.settings.keybindings,
            modal: self.modal.as_ref(),
            activity_frame: self.activity_frame,
            footer_mode: self.footer_mode(),
        }
    }
}

/// Borrowed inputs to a single frame render (never includes the session).
struct RenderCtx<'a> {
    state: &'a TuiState,
    textarea: &'a TextArea<'a>,
    completion: &'a CompletionState,
    theme: &'a TuiTheme,
    status: &'a StatusInfo,
    sidebar: &'a SidebarInfo,
    keybindings: &'a TuiKeybindings,
    modal: Option<&'a Modal>,
    activity_frame: usize,
    footer_mode: FooterMode,
}

/// Render one frame from a [`RenderCtx`] (immediate-mode; called every tick).
fn render(frame: &mut Frame, ctx: &RenderCtx) {
    let area = frame.area();
    let show_sidebar = area.width >= 80;
    // Vertical split: workspace (flex) + footer (1).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let workspace = outer[0];
    let footer_area = outer[1];

    let (sidebar_area, main_area) = if show_sidebar {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(32), Constraint::Min(1)])
            .split(workspace);
        (Some(cols[0]), cols[1])
    } else {
        (None, workspace)
    };

    if let Some(sidebar_area) = sidebar_area {
        render_sidebar(frame, sidebar_area, ctx.sidebar, ctx.theme);
    }

    // Main pane vertical: transcript (flex) / queued / prompt-row / status / popup.
    let queued_lines = ctx.state.queued_steering.len() + ctx.state.queued_follow_up.len();
    let queued_h = u16::try_from(queued_lines.min(8)).unwrap_or(8);
    let prompt_h = u16::try_from(ctx.textarea.lines().len().clamp(1, 8)).unwrap_or(8);
    let completion_h = if ctx.completion.items.is_empty() {
        0
    } else {
        completion_popup_height(ctx.completion)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(queued_h),
            Constraint::Length(prompt_h),
            Constraint::Length(1),
            Constraint::Length(completion_h),
        ])
        .split(main_area);

    render_transcript_scrolled(frame, rows[0], ctx.state, ctx.theme);
    if queued_h > 0 {
        render_queued_messages(
            frame,
            rows[1],
            &ctx.state.queued_steering,
            &ctx.state.queued_follow_up,
            ctx.theme,
        );
    }
    render_prompt_row(frame, rows[2], ctx);
    render_compact_session_info(frame, rows[3], ctx.status, ctx.theme);
    if completion_h > 0 {
        render_completion_popup(frame, rows[4], ctx.completion, ctx.theme);
    }

    render_footer(
        frame,
        footer_area,
        ctx.footer_mode,
        ctx.keybindings,
        ctx.theme,
    );

    if let Some(modal) = ctx.modal {
        modal.render(frame, area, ctx.theme);
    }
}

fn completion_popup_height(completion: &CompletionState) -> u16 {
    // Approximate: one line per item plus category separators, capped at 12.
    let mut height = completion.items.len();
    height = height.min(12);
    u16::try_from(height.max(1)).unwrap_or(1)
}

/// Render the prompt-prefix cell + the editable text area.
fn render_prompt_row(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);
    render_prompt_prefix(
        frame,
        cols[0],
        ctx.state.running,
        ctx.activity_frame,
        ctx.theme,
    );
    frame.render_widget(ctx.textarea, cols[1]);
}

/// Render the transcript, following the bottom when it overflows the viewport.
fn render_transcript_scrolled(frame: &mut Frame, area: Rect, state: &TuiState, theme: &TuiTheme) {
    render_transcript(frame, area, state, theme);
}

fn seed_transcript(state: &mut TuiState, session: &CodingSession) {
    // Replay the persisted transcript through the adapter so a resumed session
    // shows its history (tau rebuilds the transcript from the session on mount).
    let mut adapter = TuiEventAdapter::new(state);
    for message in session.messages() {
        adapter.apply(&rho_coding::events::CodingSessionEvent::Agent(
            rho_agent::events::AgentEvent::MessageEnd(rho_agent::events::MessageEndEvent::new(
                message,
            )),
        ));
    }
}

/// Build the chrome snapshot from the live session.
fn build_chrome(session: &mut CodingSession, cwd: &Path) -> ChromeSnapshot {
    let provider_name = session.provider_name().to_string();
    let model = session.model();
    let thinking_display = thinking_display(session);
    let context_token_estimate = session.context_token_estimate();
    let context_window_tokens = session.context_window_tokens();
    let auto_compact_token_threshold = session.auto_compact_token_threshold();
    let tools_count = session.tools().len();
    let skills_count = session.skills().len();
    let tool_names: Vec<String> = session.tools().iter().map(|t| t.name.clone()).collect();
    let skill_names: Vec<String> = session.skills().iter().map(|s| s.name.clone()).collect();
    let prompt_names: Vec<String> = session
        .prompt_templates()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    let context_labels: Vec<String> = session
        .context_files()
        .iter()
        .map(|f| f.path.clone())
        .collect();

    let status = StatusInfo {
        cwd: cwd.to_path_buf(),
        provider_name: provider_name.clone(),
        model: model.clone(),
        thinking_display: thinking_display.clone(),
        context_token_estimate,
        context_window_tokens,
        auto_compact_token_threshold,
        git_branch: git_branch(cwd),
    };
    let sidebar = SidebarInfo {
        provider_name,
        model,
        thinking_display,
        tools_count,
        skills_count,
        context_labels,
        tool_names,
        skill_names,
        prompt_names,
    };
    ChromeSnapshot { status, sidebar }
}

/// Resolve the thinking-level display (tau `_thinking_level`).
fn thinking_display(session: &CodingSession) -> String {
    if session.available_thinking_levels().is_empty() {
        return "unavailable".to_string();
    }
    let level = session.thinking_level();
    if level.is_empty() {
        "--".to_string()
    } else {
        level.to_string()
    }
}

// --- terminal lifecycle -----------------------------------------------------

type Backend = CrosstermBackend<Stdout>;

fn init_terminal() -> io::Result<Terminal<Backend>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Terminal<Backend>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()
}

/// Run the interactive TUI to completion (the `rho` no-`-p` entry point).
pub async fn run_tui(session: CodingSession, settings: TuiSettings) -> io::Result<()> {
    let mut app = App::new(session, settings);
    let mut terminal = init_terminal()?;
    let result = app.event_loop(&mut terminal).await;
    restore_terminal(&mut terminal)?;
    result
}

impl App {
    async fn event_loop(&mut self, terminal: &mut Terminal<Backend>) -> io::Result<()> {
        let mut events = EventStream::new();
        loop {
            terminal.draw(|f| render(f, &self.render_ctx()))?;
            if self.should_quit {
                return Ok(());
            }
            let Some(Ok(event)) = events.next().await else {
                continue;
            };
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                self.handle_key_idle(key, terminal, &mut events).await?;
            }
        }
    }

    /// Handle a key while idle (no run in progress).
    async fn handle_key_idle(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) -> io::Result<()> {
        // Modal overlay gets keys first.
        if self.modal.is_some() {
            self.handle_modal_key(key).await;
            return Ok(());
        }
        let kb = self.settings.keybindings.clone();
        // Enter submits (unless a completion is pending); Shift+Enter is newline.
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
            self.submit_prompt(terminal, events).await?;
            return Ok(());
        }
        if matches_binding(&key, &kb.accept_completion) {
            self.accept_completion();
            return Ok(());
        }
        if matches_binding(&key, &kb.completion_next) && !self.completion.items.is_empty() {
            self.completion = self.completion.select_next();
            return Ok(());
        }
        if matches_binding(&key, &kb.completion_previous) && !self.completion.items.is_empty() {
            self.completion = self.completion.select_previous();
            return Ok(());
        }
        if matches_binding(&key, &kb.quit) {
            self.should_quit = true;
            return Ok(());
        }
        if matches_binding(&key, &kb.command_palette) {
            self.set_prompt_text("/");
            self.rebuild_completion();
            return Ok(());
        }
        if matches_binding(&key, &kb.session_picker) {
            self.open_session_picker();
            return Ok(());
        }
        if matches_binding(&key, &kb.model_cycle) {
            self.open_model_picker();
            return Ok(());
        }
        if matches_binding(&key, &kb.thinking_cycle) {
            let _ = self.session.cycle_thinking_level().await;
            self.refresh_chrome();
            return Ok(());
        }
        if matches_binding(&key, &kb.toggle_tool_results) {
            self.state.show_tool_results = !self.state.show_tool_results;
            return Ok(());
        }
        if matches_binding(&key, &kb.toggle_thinking) {
            self.state.show_thinking = !self.state.show_thinking;
            return Ok(());
        }
        if matches_binding(&key, &kb.copy_message) {
            self.clear_prompt();
            return Ok(());
        }
        // Otherwise feed the editor and recompute completions.
        self.textarea.input(Event::Key(key));
        self.rebuild_completion();
        Ok(())
    }

    fn accept_completion(&mut self) {
        if let Some(item) = self.completion.selected() {
            let text = self.prompt_text();
            let applied = item.apply(&text);
            self.set_prompt_text(&applied);
            self.rebuild_completion();
        }
    }

    fn open_session_picker(&mut self) {
        let manager = SessionManager::new(rho_coding::paths::RhoPaths::default());
        let records = manager.list_sessions(None).unwrap_or_default();
        self.modal = Some(Modal::SessionPicker(SessionPickerModal::new(records)));
    }

    fn open_model_picker(&mut self) {
        let choices = self.session.available_model_choices();
        let scoped = self.session.scoped_model_choices();
        let current = self.session.model();
        let provider = self.session.provider_name().to_string();
        self.modal = Some(Modal::ModelPicker(ModelPickerModal::new(
            choices,
            scoped,
            current,
            provider,
            ModelPickerKind::Model,
        )));
    }

    async fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.as_mut() else {
            return;
        };
        let outcome = modal.handle_key(key);
        match outcome {
            ModalOutcome::Consumed => {}
            ModalOutcome::Cancelled => self.modal = None,
            ModalOutcome::Theme(name) => {
                self.settings.theme = name;
                self.theme = self.settings.resolved_theme();
                self.modal = None;
            }
            ModalOutcome::Model(choice) => {
                let _ = self.session.set_model_choice(&choice);
                self.refresh_chrome();
                self.modal = None;
            }
            ModalOutcome::ScopedToggle(choice) => {
                let _ = self.session.toggle_scoped_model(&choice);
            }
            ModalOutcome::Session(_id) => {
                // Resume-in-place requires rebuilding the session; deferred to the
                // launcher (a fresh `rho --resume <id>` re-enters the TUI).
                self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                    "Resume",
                    "Session resume from the picker lands with the resume launcher; \
                     restart with `rho --resume <id>` for now.",
                )));
            }
            ModalOutcome::OpenBranchSummary(entry_id) => {
                self.modal = Some(Modal::BranchSummaryInstructions(
                    crate::modals::BranchSummaryModal::new(entry_id),
                ));
            }
            ModalOutcome::Branch(result) => {
                let replace_instructions = result.custom_instructions.is_some();
                let _ = self
                    .session
                    .branch_to_entry(
                        &result.entry_id,
                        result.summarize,
                        result.custom_instructions.as_deref(),
                        replace_instructions,
                    )
                    .await;
                self.state = TuiState::new();
                seed_transcript(&mut self.state, &self.session);
                self.refresh_chrome();
                self.modal = None;
            }
        }
    }

    /// Submit the current prompt: accept a pending completion, run a terminal
    /// command, dispatch a slash command, or start/steer a real prompt turn.
    async fn submit_prompt(
        &mut self,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) -> io::Result<()> {
        // First Enter accepts a pending completion instead of submitting.
        if let Some(item) = self.completion.selected() {
            let text = self.prompt_text();
            let applied = item.apply(&text);
            if applied != text {
                self.set_prompt_text(&applied);
                self.rebuild_completion();
                return Ok(());
            }
        }
        let text = self.prompt_text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.clear_prompt();
            return Ok(());
        }
        self.prompt_history.push(text.clone());
        self.clear_prompt();

        // Slash command dispatch (tau `session.handle_command`).
        if trimmed.starts_with('/') && !trimmed.starts_with("//") {
            let result = self.session.handle_command(trimmed);
            if result.handled {
                self.apply_command_result(result).await;
                self.refresh_chrome();
                return Ok(());
            }
            // Unknown command: fall through and send it as a literal prompt,
            // matching tau (an unhandled `/x` is submitted verbatim).
        }

        // Running: queue as a steering message; idle: start a turn.
        if self.control.is_running() {
            self.control.steer(&text);
            self.state.update_queue(
                queue_texts(&self.control, true),
                queue_texts(&self.control, false),
            );
            return Ok(());
        }
        self.run_turn(text, terminal, events).await
    }

    /// Apply a handled slash-command result: the picker/toggle/exit effects that
    /// have an M5 home, plus any user-facing message. Effects that need the
    /// resume launcher (new-session / resume-id) or M7 (login) surface a notice;
    /// this deferral is journaled in `phase-5.md`.
    async fn apply_command_result(&mut self, result: rho_coding::commands::CommandResult) {
        if result.exit_requested {
            self.should_quit = true;
            return;
        }
        if result.clear_requested {
            self.state.items.clear();
            self.state.assistant_buffer.clear();
            self.state.error = None;
        }
        if result.theme_picker_requested {
            self.modal = Some(Modal::ThemePicker(ThemePickerModal::new(
                self.settings.theme,
            )));
            return;
        }
        if result.model_picker_requested {
            self.open_model_picker();
            return;
        }
        if result.scoped_models_picker_requested {
            let choices = self.session.available_model_choices();
            let scoped = self.session.scoped_model_choices();
            let current = self.session.model();
            let provider = self.session.provider_name().to_string();
            self.modal = Some(Modal::ModelPicker(ModelPickerModal::new(
                choices,
                scoped,
                current,
                provider,
                ModelPickerKind::Scoped,
            )));
            return;
        }
        if result.resume_picker_requested {
            self.open_session_picker();
            return;
        }
        if result.tree_picker_requested {
            match self.session.tree_choices().await {
                Ok(choices) => {
                    self.modal = Some(Modal::TreePicker(crate::modals::TreePickerModal::new(
                        choices,
                    )));
                }
                Err(err) => {
                    self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                        "Session tree",
                        err.to_string(),
                    )));
                }
            }
            return;
        }
        if let Some(level) = &result.thinking_level {
            let _ = self.session.set_thinking_level(level).await;
        }
        if let Some(theme_name) = &result.theme {
            if let Some(name) = crate::theme::TuiThemeName::parse(theme_name) {
                self.settings.theme = name;
                self.theme = self.settings.resolved_theme();
            }
        }
        if result.login_picker_requested
            || result.custom_provider_login_requested
            || result.logout_picker_requested
        {
            self.modal = Some(Modal::Notice(crate::modals::NoticeModal::m7(
                "Login / logout",
            )));
            return;
        }
        if result.new_session_requested || result.resume_session_id.is_some() {
            self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                "Session switch",
                "New-session / resume-by-id lands with the resume launcher; \
                 restart with `rho --new-session` or `rho --resume <id>` for now.",
            )));
            return;
        }
        if let Some(message) = &result.message {
            self.modal = Some(Modal::CommandOutput(
                crate::modals::CommandOutputModal::new("Command", message.clone()),
            ));
        }
    }

    /// Drive one prompt turn: stream session events into the transcript while
    /// still polling the keyboard for steer / follow-up / cancel / toggles.
    async fn run_turn(
        &mut self,
        text: String,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) -> io::Result<()> {
        let control = self.control.clone();
        let keybindings = self.settings.keybindings.clone();
        // Scope the disjoint field borrows (and the `session`-borrowing stream)
        // so they all drop before we touch `self` again after the turn.
        {
            let App {
                session,
                state,
                textarea,
                completion,
                theme,
                chrome,
                settings,
                activity_frame,
                ..
            } = &mut *self;
            let stream = session.prompt(text, Some(StreamingBehavior::Steer));
            futures::pin_mut!(stream);
            let mut ticker = tokio::time::interval(Duration::from_millis(150));

            loop {
                {
                    let ctx = RenderCtx {
                        state,
                        textarea,
                        completion,
                        theme,
                        status: &chrome.status,
                        sidebar: &chrome.sidebar,
                        keybindings: &settings.keybindings,
                        modal: None,
                        activity_frame: *activity_frame,
                        footer_mode: FooterMode::Running,
                    };
                    terminal.draw(|f| render(f, &ctx))?;
                }

                tokio::select! {
                    maybe_event = stream.next() => {
                        match maybe_event {
                            Some(event) => TuiEventAdapter::new(state).apply(&event),
                            None => break,
                        }
                    }
                    _ = ticker.tick() => {
                        *activity_frame = activity_frame.wrapping_add(1);
                        state.tool_spinner = Some(
                            crate::state::TOOL_SPINNER_FRAMES
                                [*activity_frame % crate::state::TOOL_SPINNER_FRAMES.len()]
                            .to_string(),
                        );
                    }
                    maybe_key = events.next() => {
                        if let Some(Ok(Event::Key(key))) = maybe_key {
                            if key.kind != KeyEventKind::Release {
                                handle_running_key(key, &keybindings, &control, textarea, state);
                            }
                        }
                    }
                }
            }
        }
        self.state.tool_spinner = None;
        self.refresh_chrome();
        Ok(())
    }
}

/// Handle a key while a turn streams: steer / follow-up / cancel via the control
/// handle, plus the pure-UI toggles that don't touch the borrowed session.
fn handle_running_key(
    key: KeyEvent,
    kb: &TuiKeybindings,
    control: &HarnessControl,
    textarea: &mut TextArea<'static>,
    state: &mut TuiState,
) {
    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
        let text = textarea.lines().join("\n");
        if !text.trim().is_empty() {
            control.steer(&text);
            let q = control.queued_messages();
            state.update_queue(
                q.steering
                    .iter()
                    .map(rho_agent::messages::AgentMessage::text)
                    .collect(),
                q.follow_up
                    .iter()
                    .map(rho_agent::messages::AgentMessage::text)
                    .collect(),
            );
            *textarea = fresh_textarea();
        }
        return;
    }
    if matches_binding(&key, &kb.queue_follow_up) {
        let text = textarea.lines().join("\n");
        if !text.trim().is_empty() {
            control.follow_up(&text);
            let q = control.queued_messages();
            state.update_queue(
                q.steering
                    .iter()
                    .map(rho_agent::messages::AgentMessage::text)
                    .collect(),
                q.follow_up
                    .iter()
                    .map(rho_agent::messages::AgentMessage::text)
                    .collect(),
            );
            *textarea = fresh_textarea();
        }
        return;
    }
    if matches_binding(&key, &kb.cancel) {
        control.cancel();
        return;
    }
    if matches_binding(&key, &kb.toggle_tool_results) {
        state.show_tool_results = !state.show_tool_results;
        return;
    }
    if matches_binding(&key, &kb.toggle_thinking) {
        state.show_thinking = !state.show_thinking;
        return;
    }
    // Otherwise let the user keep composing the next steering message.
    textarea.input(Event::Key(key));
}

fn fresh_textarea() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_placeholder_text("Type a message, /command, or !shell");
    textarea
}

fn queue_texts(control: &HarnessControl, steering: bool) -> Vec<String> {
    let q = control.queued_messages();
    if steering {
        q.steering
            .iter()
            .map(rho_agent::messages::AgentMessage::text)
            .collect()
    } else {
        q.follow_up
            .iter()
            .map(rho_agent::messages::AgentMessage::text)
            .collect()
    }
}

/// Whether a crossterm key event matches a tau keybinding spec like
/// `"ctrl+k"` / `"shift+tab"` / `"escape"` / `"alt+enter"`.
fn matches_binding(key: &KeyEvent, spec: &str) -> bool {
    let mut wants_ctrl = false;
    let mut wants_alt = false;
    let mut wants_shift = false;
    let mut base = "";
    for part in spec.split('+') {
        match part {
            "ctrl" => wants_ctrl = true,
            "alt" => wants_alt = true,
            "shift" => wants_shift = true,
            other => base = other,
        }
    }
    let mods = key.modifiers;
    if wants_ctrl != mods.contains(KeyModifiers::CONTROL) {
        return false;
    }
    if wants_alt != mods.contains(KeyModifiers::ALT) {
        return false;
    }
    // Shift is implied for uppercase/`shift+` specs; only enforce when requested.
    if wants_shift && !mods.contains(KeyModifiers::SHIFT) {
        return false;
    }
    match key.code {
        KeyCode::Char(c) => {
            base.len() == 1 && c.eq_ignore_ascii_case(&base.chars().next().unwrap())
        }
        KeyCode::Esc => base == "escape" || base == "esc",
        KeyCode::Enter => base == "enter",
        KeyCode::Tab | KeyCode::BackTab => base == "tab",
        KeyCode::Up => base == "up",
        KeyCode::Down => base == "down",
        KeyCode::Left => base == "left",
        KeyCode::Right => base == "right",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn binding_matches_ctrl_letter() {
        assert!(matches_binding(
            &key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            "ctrl+k"
        ));
        assert!(!matches_binding(
            &key(KeyCode::Char('k'), KeyModifiers::empty()),
            "ctrl+k"
        ));
    }

    #[test]
    fn binding_matches_escape_and_enter() {
        assert!(matches_binding(
            &key(KeyCode::Esc, KeyModifiers::empty()),
            "escape"
        ));
        assert!(matches_binding(
            &key(KeyCode::Enter, KeyModifiers::empty()),
            "enter"
        ));
    }

    #[test]
    fn binding_matches_shift_tab() {
        assert!(matches_binding(
            &key(KeyCode::BackTab, KeyModifiers::SHIFT),
            "shift+tab"
        ));
    }

    #[test]
    fn binding_rejects_wrong_modifier() {
        assert!(!matches_binding(
            &key(KeyCode::Char('c'), KeyModifiers::ALT),
            "ctrl+c"
        ));
    }
}
