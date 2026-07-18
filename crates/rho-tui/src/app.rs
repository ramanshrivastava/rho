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

use std::cell::RefCell;
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
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
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
    render_footer, render_prompt_prefix, render_queued_messages, render_sidebar,
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
    /// Memoized transcript render (rebuilt only when its fingerprint changes).
    /// `RefCell` so the immutable-borrow render path can refresh it in place.
    transcript_cache: RefCell<crate::widgets::TranscriptCache>,
}

impl App {
    /// Build an app around a loaded session.
    ///
    /// `startup_message`, when present, is surfaced as a status notice on launch
    /// (tau's warning-severity startup toast — used for the login-required
    /// prompt when the session opened without a usable credential).
    #[must_use]
    pub fn new(
        mut session: CodingSession,
        settings: TuiSettings,
        startup_message: Option<String>,
    ) -> Self {
        let theme = settings.resolved_theme();
        let cwd = session.cwd().to_path_buf();
        let mut state = TuiState::new();
        state.show_tool_results = false;
        state.show_thinking = true;
        seed_transcript(&mut state, &session);
        if let Some(message) = startup_message {
            state.add_item(crate::state::ChatItemRole::Status, message);
        }
        let chrome = build_chrome(&mut session, &cwd);
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type a message, /command, or !shell");
        Self {
            registry: create_default_command_registry(),
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
            transcript_cache: RefCell::new(crate::widgets::TranscriptCache::default()),
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
            transcript_cache: &self.transcript_cache,
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
    transcript_cache: &'a RefCell<crate::widgets::TranscriptCache>,
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
    let completion_h = completion_popup_height(ctx.completion, ctx.theme);
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

    render_transcript_scrolled(frame, rows[0], ctx.state, ctx.theme, ctx.transcript_cache);
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

fn completion_popup_height(completion: &CompletionState, theme: &TuiTheme) -> u16 {
    if completion.items.is_empty() {
        return 0;
    }
    // Count the ACTUAL rendered rows (category headers + blank separators), not
    // just the item count, so the selected item is never clipped. Capped at 12.
    let rows = crate::widgets::build_completion_lines(completion, theme).len();
    u16::try_from(rows.clamp(1, 12)).unwrap_or(12)
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

/// Render the transcript, following the bottom when it overflows the viewport so
/// the newest turns and streaming output stay visible (tau auto-scrolls to the
/// tail on new content).
fn render_transcript_scrolled(
    frame: &mut Frame,
    area: Rect,
    state: &TuiState,
    theme: &TuiTheme,
    cache: &RefCell<crate::widgets::TranscriptCache>,
) {
    // A fresh, idle session shows the rho welcome splash instead of a blank pane.
    // Gated on `!running` so it never flashes during the first turn before the
    // user message / first delta lands in the transcript.
    if !state.running && crate::widgets::transcript_is_empty(state) {
        crate::widgets::render_splash(frame, area, theme);
        return;
    }
    let mut cache = cache.borrow_mut();
    let lines = cache.lines(state, theme, area.width);
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let offset = total.saturating_sub(area.height);
    let bg = crate::widgets::parse_color(&theme.transcript_background)
        .map_or_else(Style::default, |color| Style::default().bg(color));
    frame.render_widget(
        Paragraph::new(lines.to_vec()).scroll((offset, 0)).style(bg),
        area,
    );
}

fn seed_transcript(state: &mut TuiState, session: &CodingSession) {
    // tau rebuilds the transcript from the session on mount. `load_messages`
    // renders every message shape (tool calls/results, thinking, branch/compaction
    // summaries); the event adapter's `MessageEnd` path silently drops those.
    state.load_messages(&session.messages());
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

/// Best-effort terminal reset that works from just `stdout` (no `Terminal`
/// handle), so both the panic hook and the RAII guard can call it.
fn restore_terminal_stdout() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

/// Restores the terminal on drop — including during a panic unwind — so a crash
/// (e.g. in a render) can never leave raw mode / the alternate screen / mouse
/// capture on. tau gets this from Textual's try/finally; rho makes it explicit.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal_stdout();
    }
}

/// Run the interactive TUI to completion (the `rho` no-`-p` entry point).
pub async fn run_tui(
    session: CodingSession,
    settings: TuiSettings,
    startup_message: Option<String>,
) -> io::Result<()> {
    let mut app = App::new(session, settings, startup_message);
    let mut terminal = init_terminal()?;

    // Chain a panic hook that restores the terminal FIRST (so the panic message
    // is legible on a cooked terminal), then defers to the previous hook.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal_stdout();
        previous_hook(info);
    }));

    // The guard restores on normal return AND on any unwind through here.
    let result = {
        let _guard = TerminalGuard;
        app.event_loop(&mut terminal).await
    };

    let _ = terminal.show_cursor();
    // Re-lower to the default panic hook now that the TUI owns the terminal no more.
    let _ = std::panic::take_hook();
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
            match events.next().await {
                Some(Ok(Event::Key(key))) => {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    self.handle_key_idle(key, terminal, &mut events).await?;
                }
                Some(Ok(_)) => {} // resize / mouse / paste — redraw next iteration.
                Some(Err(err)) => return Err(err),
                // Terminal input closed: exit cleanly instead of spinning on a
                // now-always-ready `None`.
                None => return Ok(()),
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
        if matches_binding(&key, &kb.completion_previous) {
            if !self.completion.items.is_empty() {
                self.completion = self.completion.select_previous();
                return Ok(());
            }
            // No completions open: tau maps Up (`action_recall_previous_prompt`)
            // to recalling the last submitted prompt into an EMPTY composer,
            // before falling through to a plain cursor-up.
            if self.recall_previous_prompt() {
                return Ok(());
            }
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

    /// Recall the most recently submitted prompt into an EMPTY composer (tau
    /// `action_recall_previous_prompt`). Only fires when the composer is blank so
    /// an accidental Up never clobbers a prompt the user is still writing.
    /// Returns whether a prompt was recalled.
    fn recall_previous_prompt(&mut self) -> bool {
        if !self.prompt_text().trim().is_empty() {
            return false;
        }
        let Some(previous) = self.prompt_history.last().cloned() else {
            return false;
        };
        self.set_prompt_text(&previous);
        self.rebuild_completion();
        true
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
            ModalOutcome::Model(choice) => match self.session.set_model_choice(&choice) {
                Ok(()) => {
                    self.refresh_chrome();
                    self.modal = None;
                }
                Err(err) => {
                    self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                        "Model",
                        err.to_string(),
                    )));
                }
            },
            ModalOutcome::ScopedToggle(choice) => {
                // The picker already updated its own membership; surface a failure
                // to persist so the two don't silently drift.
                if let Err(err) = self.session.toggle_scoped_model(&choice) {
                    self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                        "Scoped models",
                        err.to_string(),
                    )));
                }
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
                match self
                    .session
                    .branch_to_entry(
                        &result.entry_id,
                        result.summarize,
                        result.custom_instructions.as_deref(),
                        replace_instructions,
                    )
                    .await
                {
                    Ok(_) => {
                        // Only rebuild the transcript once the branch succeeded —
                        // a failed branch must leave the current session intact.
                        self.state = TuiState::new();
                        seed_transcript(&mut self.state, &self.session);
                        self.refresh_chrome();
                        self.modal = None;
                    }
                    Err(err) => {
                        self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                            "Branch",
                            err.to_string(),
                        )));
                    }
                }
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

        // Terminal command (`!cmd` / `!!cmd`): run it locally instead of prompting
        // the model (tau routes these before the agent turn; print mode too).
        if let Some(request) = rho_coding::session::parse_terminal_command(&text) {
            match self
                .session
                .run_terminal_command(&request.command, request.add_to_context)
                .await
            {
                Ok(result) => {
                    let title = format!("$ {}", result.command);
                    self.modal = Some(Modal::CommandOutput(
                        crate::modals::CommandOutputModal::new(title, result.output),
                    ));
                }
                Err(err) => {
                    self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
                        "Terminal command",
                        err.to_string(),
                    )));
                }
            }
            self.refresh_chrome();
            return Ok(());
        }

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

        // Running: queue as a steering message; idle: start a turn. Take the
        // control handle from the current harness (never a stale cached one).
        if self.session.is_running() {
            let control = self.session.control();
            control.steer(&text);
            self.state
                .update_queue(queue_texts(&control, true), queue_texts(&control, false));
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
        // Acquire the control handle from the CURRENT harness: a model/provider
        // switch or a branch can rebuild the harness, so a handle captured earlier
        // would target a stale run's cancel token + queues (CR).
        let control = self.session.control();
        let keybindings = self.settings.keybindings.clone();
        // Scope the disjoint field borrows (and the `session`-borrowing stream)
        // so they all drop before we touch `self` again after the turn. The block
        // yields whether the user asked to quit mid-turn.
        let quit_requested = {
            let App {
                session,
                state,
                textarea,
                completion,
                theme,
                chrome,
                settings,
                activity_frame,
                transcript_cache,
                ..
            } = &mut *self;
            let stream = session.prompt(text, Some(StreamingBehavior::Steer));
            futures::pin_mut!(stream);
            let mut ticker = tokio::time::interval(Duration::from_millis(150));
            let mut quit_requested = false;

            'turn: loop {
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
                        transcript_cache,
                    };
                    terminal.draw(|f| render(f, &ctx))?;
                }

                tokio::select! {
                    maybe_event = stream.next() => {
                        match maybe_event {
                            Some(event) => {
                                TuiEventAdapter::new(state).apply(&event);
                                // Coalesce a burst of stream deltas into ONE redraw:
                                // drain every event that is already ready this frame
                                // (`now_or_never` polls without awaiting) before the
                                // next `terminal.draw`. A fast token stream would
                                // otherwise redraw once per delta; now the whole
                                // batch costs a single (cache-fingerprinted) render.
                                loop {
                                    match stream.next().now_or_never() {
                                        Some(Some(event)) => {
                                            TuiEventAdapter::new(state).apply(&event);
                                        }
                                        // Stream ended mid-drain: finish the turn.
                                        Some(None) => break 'turn,
                                        // Nothing more ready right now: draw the batch.
                                        None => break,
                                    }
                                }
                            }
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
                        match maybe_key {
                            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                                if let RunningKeyOutcome::Quit =
                                    handle_running_key(key, &keybindings, &control, textarea, state)
                                {
                                    quit_requested = true;
                                    break;
                                }
                            }
                            Some(Ok(_)) => {}
                            // Input closed / errored mid-turn: cancel the run and
                            // stop draining events so we don't spin on a ready `None`.
                            Some(Err(_)) | None => {
                                control.cancel();
                                break;
                            }
                        }
                    }
                }
            }
            quit_requested
        };
        if quit_requested {
            self.should_quit = true;
        }
        self.state.tool_spinner = None;
        // Surface a run error the session recorded (tau shows the failure in the
        // transcript after the turn settles).
        if let Some(error) = self.session.take_run_error() {
            self.state.error = Some(error.clone());
            self.state
                .add_item(crate::state::ChatItemRole::Error, format!("Error: {error}"));
        }
        self.refresh_chrome();
        Ok(())
    }
}

/// What a key handled mid-turn asks the turn loop to do next.
enum RunningKeyOutcome {
    /// Keep streaming.
    Continue,
    /// Quit the app (cancel the run first).
    Quit,
}

/// Handle a key while a turn streams: steer / follow-up / cancel via the control
/// handle, the pure-UI toggles that don't touch the borrowed session, and the
/// always-live quit / command-palette bindings tau keeps active during a run
/// (via priority bindings) so the user is never stranded while the agent works.
fn handle_running_key(
    key: KeyEvent,
    kb: &TuiKeybindings,
    control: &HarnessControl,
    textarea: &mut TextArea<'static>,
    state: &mut TuiState,
) -> RunningKeyOutcome {
    // Quit stays live during a run (tau keeps `quit` as a priority binding): cancel
    // the in-flight run, then let the loop exit the app.
    if matches_binding(&key, &kb.quit) {
        control.cancel();
        return RunningKeyOutcome::Quit;
    }
    // Command palette stays live: seed the composer with `/` (completions rebuild on
    // the next keystroke, once the turn releases the session borrow).
    if matches_binding(&key, &kb.command_palette) {
        *textarea = fresh_textarea();
        textarea.insert_str("/");
        return RunningKeyOutcome::Continue;
    }
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
        return RunningKeyOutcome::Continue;
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
        return RunningKeyOutcome::Continue;
    }
    if matches_binding(&key, &kb.cancel) {
        control.cancel();
        return RunningKeyOutcome::Continue;
    }
    if matches_binding(&key, &kb.toggle_tool_results) {
        state.show_tool_results = !state.show_tool_results;
        return RunningKeyOutcome::Continue;
    }
    if matches_binding(&key, &kb.toggle_thinking) {
        state.show_thinking = !state.show_thinking;
        return RunningKeyOutcome::Continue;
    }
    // Otherwise let the user keep composing the next steering message.
    textarea.input(Event::Key(key));
    RunningKeyOutcome::Continue
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
    let has_shift = mods.contains(KeyModifiers::SHIFT);
    match key.code {
        // Letters carry Shift in their case, so don't enforce the Shift modifier.
        KeyCode::Char(c) => {
            base.len() == 1 && c.eq_ignore_ascii_case(&base.chars().next().unwrap())
        }
        // `BackTab` IS Shift+Tab: it must match a `shift+tab` spec and NEVER a plain
        // `tab` spec (else Shift+Tab would fire `accept_completion`).
        KeyCode::BackTab => base == "tab" && wants_shift,
        KeyCode::Tab => base == "tab" && !wants_shift && !has_shift,
        KeyCode::Esc => (base == "escape" || base == "esc") && wants_shift == has_shift,
        KeyCode::Enter => base == "enter" && wants_shift == has_shift,
        KeyCode::Up => base == "up" && wants_shift == has_shift,
        KeyCode::Down => base == "down" && wants_shift == has_shift,
        KeyCode::Left => base == "left" && wants_shift == has_shift,
        KeyCode::Right => base == "right" && wants_shift == has_shift,
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
    fn raii_guard_restores_on_panic() {
        // C2 mechanism: a Drop guard (like TerminalGuard) runs during an unwind,
        // so a panic in the render/event loop still restores the terminal.
        use std::sync::atomic::{AtomicBool, Ordering};
        static RESTORED: AtomicBool = AtomicBool::new(false);
        struct Spy;
        impl Drop for Spy {
            fn drop(&mut self) {
                RESTORED.store(true, Ordering::SeqCst);
            }
        }
        let result = std::panic::catch_unwind(|| {
            let _spy = Spy;
            panic!("boom in render");
        });
        assert!(result.is_err());
        assert!(
            RESTORED.load(Ordering::SeqCst),
            "guard must restore on unwind"
        );
        // The real guard's Drop must also be panic-safe on a non-tty (no double panic).
        let guard = TerminalGuard;
        drop(guard);
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
        // BackTab arrives with or without the SHIFT modifier depending on the
        // terminal; either way it is Shift+Tab.
        assert!(matches_binding(
            &key(KeyCode::BackTab, KeyModifiers::empty()),
            "shift+tab"
        ));
    }

    #[test]
    fn shift_tab_never_matches_plain_tab() {
        // Regression: Shift+Tab (BackTab) must NOT fire the plain `tab` binding
        // (accept_completion), so it can reach thinking_cycle / completion_previous.
        assert!(!matches_binding(
            &key(KeyCode::BackTab, KeyModifiers::SHIFT),
            "tab"
        ));
        assert!(!matches_binding(
            &key(KeyCode::BackTab, KeyModifiers::empty()),
            "tab"
        ));
        // Plain Tab still matches the plain `tab` binding, and not `shift+tab`.
        assert!(matches_binding(
            &key(KeyCode::Tab, KeyModifiers::empty()),
            "tab"
        ));
        assert!(!matches_binding(
            &key(KeyCode::Tab, KeyModifiers::empty()),
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

    /// Build a minimal loaded session whose live provider is the login-required
    /// placeholder — the shape the CLI hands the TUI when no credential exists.
    async fn login_required_session(dir: &std::path::Path) -> CodingSession {
        use rho_agent::provider::ModelProvider;
        use rho_coding::login_required::LoginRequiredProvider;
        use rho_coding::session::{CodingSessionConfig, jsonl_session_storage};

        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(LoginRequiredProvider::new("placeholder"));
        let storage = jsonl_session_storage(dir.join("session.jsonl"));
        let config = CodingSessionConfig::new(provider, "gpt-4", storage, dir.to_path_buf());
        CodingSession::load(config).await.expect("session loads")
    }

    #[tokio::test]
    async fn startup_message_is_seeded_as_a_status_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let session = login_required_session(tmp.path()).await;
        let message = "Login required. Run /login to choose a provider, \
                       or /login openai to continue with the current provider.";
        let app = App::new(session, TuiSettings::default(), Some(message.to_string()));
        assert!(
            app.state.items.iter().any(|item| {
                item.role == crate::state::ChatItemRole::Status && item.text == message
            }),
            "the startup message is seeded as a status notice: {:?}",
            app.state.items
        );
    }

    #[tokio::test]
    async fn up_recalls_previous_prompt_only_into_empty_composer() {
        let tmp = tempfile::tempdir().unwrap();
        let session = login_required_session(tmp.path()).await;
        let mut app = App::new(session, TuiSettings::default(), None);

        // No history yet → nothing to recall.
        assert!(!app.recall_previous_prompt());

        app.prompt_history.push("first prompt".to_string());
        app.prompt_history.push("second prompt".to_string());

        // Empty composer → recalls the most recent submission.
        assert!(app.recall_previous_prompt());
        assert_eq!(app.prompt_text(), "second prompt");

        // Non-empty composer → never clobbers what the user is writing.
        app.set_prompt_text("half-written");
        assert!(!app.recall_previous_prompt());
        assert_eq!(app.prompt_text(), "half-written");
    }

    #[tokio::test]
    async fn without_a_startup_message_no_status_notice_is_added() {
        let tmp = tempfile::tempdir().unwrap();
        let session = login_required_session(tmp.path()).await;
        let app = App::new(session, TuiSettings::default(), None);
        assert!(
            !app.state
                .items
                .iter()
                .any(|item| item.role == crate::state::ChatItemRole::Status),
            "a credentialed startup adds no status notice: {:?}",
            app.state.items
        );
    }
}
