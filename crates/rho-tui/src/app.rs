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
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use tui_textarea::TextArea;

use rho_agent::harness::HarnessControl;
use rho_coding::commands::{CommandRegistry, CommandSession, create_default_command_registry};
use rho_coding::credentials::FileCredentialStore;
use rho_coding::oauth_registry::oauth_provider_ids;
use rho_coding::provider_catalog::{
    BUILTIN_PROVIDER_CATALOG, ProviderCatalogEntry, builtin_provider_entry,
};
use rho_coding::session::{CodingSession, StreamingBehavior};
use rho_coding::session_manager::SessionManager;

use crate::adapter::TuiEventAdapter;
use crate::autocomplete::{CompletionInputs, CompletionState, build_completion_state};
use crate::ext_ui::{ExtensionUiChannel, UiRequest};
use crate::file_drop::{normalize_dropped_paths, pad_dropped_insertion};
use crate::login::{
    OAuthUpdate, logout_provider as remove_credentials, oauth_login_kind, persist_api_key_login,
    persist_custom_provider, persist_oauth_login, spawn_oauth_login, stored_credential_providers,
};
use crate::modals::{
    ApiKeyLoginModal, CustomProviderDraft, CustomProviderLoginModal, ExtensionConfirmModal,
    ExtensionInputModal, ExtensionSelectModal, LoginMethod, LoginMethodPickerModal,
    LoginProviderItem, LoginProviderPickerModal, Modal, ModalOutcome, ModelPickerKind,
    ModelPickerModal, OAuthLoginModal, ProviderPickerPurpose, SessionPickerModal, ThemePickerModal,
};
use crate::motion::{self, MotionCaps};
use crate::state::TuiState;
use crate::theme::{SidebarPosition, TuiKeybindings, TuiSettings, TuiTheme};
use crate::widgets::status::git_branch;
use crate::widgets::{
    FooterMode, SidebarInfo, StatusInfo, render_compact_session_info, render_completion_popup,
    render_footer, render_prompt_prefix, render_queued_messages, render_sidebar,
    render_working_status,
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
    /// Terminal motion capabilities (truecolor + reduced-motion), resolved once.
    motion: MotionCaps,
    should_quit: bool,
    cwd: PathBuf,
    /// The one-shot responder for an in-flight extension UI dialog, if any.
    pending_ext_ui: Option<PendingUiResponder>,
    /// The channel of extension UI requests (set by the host integration).
    ext_ui_channel: Option<ExtensionUiChannel>,
    /// The provider an open API-key login modal targets (tau's per-screen entry).
    login_target: Option<String>,
    /// Memoized transcript render (rebuilt only when its fingerprint changes).
    /// `RefCell` so the immutable-borrow render path can refresh it in place.
    transcript_cache: RefCell<crate::widgets::TranscriptCache>,
}

/// Rotating empty-composer placeholder suggestions (Codex-style), cycled while
/// idle on a motion-capable terminal. The first entry is the durable default so a
/// plain terminal (and the very first frame) reads the same as before.
const PLACEHOLDER_PROMPTS: [&str; 4] = [
    "Type a message, /command, or !shell",
    "Explain this repo",
    "Add a test for this function",
    "Fix this stack trace",
];
/// Frames each placeholder suggestion holds before rotating (~6 s at 150 ms).
const PLACEHOLDER_STEP_FRAMES: usize = 40;
/// Transcript lines a single mouse-wheel notch scrolls (Claude Code feel).
const WHEEL_SCROLL_LINES: u16 = 3;

/// The responder awaiting the result of the open extension UI modal.
enum PendingUiResponder {
    Select(tokio::sync::oneshot::Sender<Option<String>>),
    Confirm(tokio::sync::oneshot::Sender<bool>),
    Input(tokio::sync::oneshot::Sender<Option<String>>),
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
            motion: MotionCaps::from_env(),
            should_quit: false,
            cwd,
            session,
            pending_ext_ui: None,
            ext_ui_channel: None,
            login_target: None,
            transcript_cache: RefCell::new(crate::widgets::TranscriptCache::default()),
        }
    }

    /// Wire the extension-host UI channel so `context.ui.*` calls render as TUI
    /// modals. The coding-runtime cluster creates the channel via
    /// [`crate::ext_ui::extension_ui_pair`], hands the handle to the extension
    /// runtime's `HostBridge`, and passes the channel here.
    pub fn set_extension_ui_channel(&mut self, channel: ExtensionUiChannel) {
        self.ext_ui_channel = Some(channel);
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

    /// Apply the per-frame idle motion to the composer: a breathing oxide cursor
    /// (quiet throb while typing) and a slowly-rotating placeholder suggestion.
    /// A no-op on plain / reduced-motion terminals, where the composer keeps a
    /// static cursor and its default placeholder.
    fn apply_idle_motion(&mut self) {
        if !self.motion.animated() {
            return;
        }
        // On an EMPTY composer (and thus the welcome splash) the throbbing oxide
        // BLOCK cursor would glare over the placeholder with no text behind it —
        // reading as a stray floating block. Use the soft resting underline there;
        // keep the ember-throb block only once the user is actually typing.
        let cursor_style = if self.prompt_text().is_empty() {
            motion::cursor_rest_style(self.motion, self.activity_frame)
        } else {
            motion::cursor_throb_style(self.motion, self.activity_frame)
        };
        self.textarea.set_cursor_style(cursor_style);
        let idx = (self.activity_frame / PLACEHOLDER_STEP_FRAMES) % PLACEHOLDER_PROMPTS.len();
        self.textarea.set_placeholder_text(PLACEHOLDER_PROMPTS[idx]);
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
            sidebar_position: self.settings.sidebar_position,
            keybindings: &self.settings.keybindings,
            modal: self.modal.as_ref(),
            activity_frame: self.activity_frame,
            motion: self.motion,
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
    sidebar_position: SidebarPosition,
    keybindings: &'a TuiKeybindings,
    modal: Option<&'a Modal>,
    activity_frame: usize,
    motion: MotionCaps,
    footer_mode: FooterMode,
    transcript_cache: &'a RefCell<crate::widgets::TranscriptCache>,
}

/// Render one frame from a [`RenderCtx`] (immediate-mode; called every tick).
fn render(frame: &mut Frame, ctx: &RenderCtx) {
    let area = frame.area();
    let show_sidebar = area.width >= 80 && ctx.sidebar_position != SidebarPosition::Off;
    // Vertical split: workspace (flex) + footer (1).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let workspace = outer[0];
    let footer_area = outer[1];

    // tau dd49d9d: honor the configured sidebar side (right by default), or hide
    // it entirely when `off`/too narrow.
    let (sidebar_area, main_area) = if show_sidebar {
        if ctx.sidebar_position == SidebarPosition::Right {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(32)])
                .split(workspace);
            (Some(cols[1]), cols[0])
        } else {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(32), Constraint::Min(1)])
                .split(workspace);
            (Some(cols[0]), cols[1])
        }
    } else {
        (None, workspace)
    };

    if let Some(sidebar_area) = sidebar_area {
        render_sidebar(frame, sidebar_area, ctx.sidebar, ctx.theme);
    }

    // Main pane vertical: transcript (flex) / queued / status / prompt-row / popup.
    // The working-state signature sits ABOVE the composer (the Claude Code layout;
    // tau's `#above-prompt-slot`), so the eye reads `Tempering… · 2m 14s · esc to
    // interrupt` and then the composer's `ρ ▍` line directly below it.
    let queued_lines = ctx.state.queued_steering.len() + ctx.state.queued_follow_up.len();
    let queued_h = u16::try_from(queued_lines.min(8)).unwrap_or(8);
    // The composer is a bordered box (top + bottom border add two rows) so the
    // cursor reads as *inside* the input, never as a stray floating cell.
    let prompt_text_h = u16::try_from(ctx.textarea.lines().len().clamp(1, 8)).unwrap_or(8);
    let prompt_h = prompt_text_h.saturating_add(2);
    let completion_h = completion_popup_height(ctx.completion, ctx.theme);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(queued_h),
            Constraint::Length(1),
            Constraint::Length(prompt_h),
            Constraint::Length(completion_h),
        ])
        .split(main_area);

    render_transcript_scrolled(
        frame,
        rows[0],
        ctx.state,
        ctx.theme,
        ctx.keybindings,
        ctx.activity_frame,
        ctx.motion,
        ctx.transcript_cache,
    );
    if queued_h > 0 {
        render_queued_messages(
            frame,
            rows[1],
            &ctx.state.queued_steering,
            &ctx.state.queued_follow_up,
            ctx.theme,
        );
    }
    // The status row sits ABOVE the composer. While a turn runs it hosts the
    // working-state signature (the shimmering forge-verb + elapsed timer +
    // interrupt hint); when idle it shows the compact session info. The composer's
    // throbbing ρ prefix sits directly below, so the eye reads `Tempering… · 2m
    // 14s · esc to interrupt` and then the `ρ ▍` input line top-to-bottom.
    if ctx.state.running {
        render_working_status(
            frame,
            rows[2],
            motion::working_verb(ctx.state.turn_index),
            ctx.state.working_elapsed_secs(),
            ctx.activity_frame,
            &interrupt_key_label(ctx.keybindings),
            ctx.motion,
            ctx.theme,
        );
    } else {
        render_compact_session_info(frame, rows[2], ctx.status, ctx.theme);
    }
    render_prompt_row(frame, rows[3], ctx);
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

/// Render the composer: a bordered box (tau's `:focus` prompt border) wrapping
/// the prompt-prefix cell + the editable text area. The border makes the cursor
/// read as *inside* the input rather than as a stray floating cell on the splash.
fn render_prompt_row(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    // Oxide/accent border (tau's focused `$tau-prompt-border`); the composer is
    // always the focused region in rho, so the box always wears the accent frame.
    let border_style = crate::widgets::parse_style(&ctx.theme.prompt_border);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);
    render_prompt_prefix(
        frame,
        cols[0],
        ctx.state.running,
        ctx.activity_frame,
        ctx.motion,
    );
    frame.render_widget(ctx.textarea, cols[1]);
}

/// Route a mouse wheel event to transcript scrollback: wheel-up opts out of
/// follow and scrolls back through history; wheel-down scrolls toward (and
/// re-arms follow at) the tail. Other mouse events are ignored.
fn scroll_transcript_on_mouse(state: &TuiState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => state.scroll_transcript_up(WHEEL_SCROLL_LINES),
        MouseEventKind::ScrollDown => state.scroll_transcript_down(WHEEL_SCROLL_LINES),
        _ => {}
    }
}

/// Route a scrollback key to the transcript. Returns whether the key was handled
/// (so the caller stops before feeding it to the editor). PageUp/PageDown page
/// through history; Shift+Home jumps to the top, Shift+End (or any downward jump)
/// re-arms follow at the tail.
fn scroll_transcript_on_key(state: &TuiState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::PageUp => {
            state.scroll_transcript_page_up();
            true
        }
        KeyCode::PageDown => {
            state.scroll_transcript_page_down();
            true
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.follow_transcript_tail();
            true
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.scroll_transcript_up(u16::MAX);
            true
        }
        _ => false,
    }
}

/// The interrupt-hint key label for the working-state line: `esc` for the
/// default escape cancel, else the capitalized binding.
fn interrupt_key_label(kb: &TuiKeybindings) -> String {
    if kb.cancel == "escape" {
        "esc".to_string()
    } else {
        crate::widgets::footer::key_hint(&kb.cancel)
    }
}

/// Render the transcript, following the tail while pinned (so streaming output
/// and new turns stay visible, tau's auto-scroll) but honoring the user's
/// scrollback offset once they scroll up — the primary Claude-Code-parity fix
/// (history was previously unrecoverable: every frame re-pinned to the bottom).
#[allow(clippy::too_many_arguments)]
fn render_transcript_scrolled(
    frame: &mut Frame,
    area: Rect,
    state: &TuiState,
    theme: &TuiTheme,
    keybindings: &TuiKeybindings,
    activity_frame: usize,
    motion: MotionCaps,
    cache: &RefCell<crate::widgets::TranscriptCache>,
) {
    // A fresh, idle session shows the rho welcome splash instead of a blank pane
    // (suppressed the moment a turn is pending — see `should_show_splash`).
    if crate::widgets::should_show_splash(state) {
        crate::widgets::render_splash(frame, area, theme, keybindings, activity_frame, motion);
        return;
    }
    let mut cache = cache.borrow_mut();
    let lines = cache.lines(state, theme, area.width);
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    // Resolve the scroll offset from the follow-output state: the tail while
    // following, else the stored top line clamped to this frame's geometry (which
    // also re-arms follow if the user scrolled back to the bottom).
    let offset = state.resolve_transcript_scroll(total, area.height);
    let bg = crate::widgets::parse_color(&theme.transcript_background)
        .map_or_else(Style::default, |color| Style::default().bg(color));
    // Bottom-anchor short conversations (Claude Code / shell feel): when the
    // transcript doesn't fill the pane, pad the TOP with blank rows so the messages
    // hug the composer and the conversation grows upward. Once it fills/overflows
    // (total >= area.height) the scroll/follow path takes over unchanged — pad is 0,
    // so scrollback is entirely unaffected.
    let (render_lines, render_offset) = if total < area.height {
        let pad = usize::from(area.height - total);
        let mut padded = Vec::with_capacity(pad + lines.len());
        padded.extend(std::iter::repeat_with(ratatui::text::Line::default).take(pad));
        padded.extend_from_slice(lines);
        (padded, 0)
    } else {
        (lines.to_vec(), offset)
    };
    frame.render_widget(
        Paragraph::new(render_lines)
            .scroll((render_offset, 0))
            .style(bg),
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
        .map(|f| crate::widgets::context_file_label(std::path::Path::new(&f.path), cwd))
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
    let insights = session.session_stats();
    let extension_names = session.extension_names();
    let session_title = CommandSession::session_title(session);
    let sidebar = SidebarInfo {
        session_title,
        provider_name,
        model,
        thinking_display,
        tools_count,
        skills_count,
        turn_count: insights.turn_count,
        tool_call_count: insights.tool_call_count,
        input_tokens: insights.input_tokens,
        output_tokens: insights.output_tokens,
        estimated_cost: insights.estimated_cost,
        context_labels,
        tool_names,
        skill_names,
        prompt_names,
        extension_names,
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
    // Bracketed paste lets the terminal deliver a paste (and terminal-generated
    // drag-and-drop path drops) as a single `Event::Paste` unit, which the file-
    // drop normalizer inspects before insertion.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Best-effort terminal reset that works from just `stdout` (no `Terminal`
/// handle), so both the panic hook and the RAII guard can call it.
fn restore_terminal_stdout() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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
        // Own the extension-UI channel for the loop's lifetime so the `select!`
        // below borrows a local (not `self`, which the event branch also touches).
        let mut ext_ui = self.ext_ui_channel.take();
        // Idle animation heartbeat: on a truecolor, motion-on terminal we advance
        // the frame counter ~every 150 ms so the splash lineage, the breathing
        // composer cursor, and the rotating placeholder animate while idle. On a
        // plain / reduced-motion terminal the interval is effectively dormant, so
        // an idle rho never wakes the CPU to redraw an unchanging frame.
        let idle_period = if self.motion.animated() {
            Duration::from_millis(150)
        } else {
            Duration::from_secs(3600)
        };
        let mut idle_ticker = tokio::time::interval(idle_period);
        idle_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            self.apply_idle_motion();
            terminal.draw(|f| render(f, &self.render_ctx()))?;
            if self.should_quit {
                return Ok(());
            }
            // Only accept a new extension dialog when nothing else is on screen
            // (a modal or an already-pending dialog owns the overlay first).
            let accept_ext_ui = self.modal.is_none() && self.pending_ext_ui.is_none();
            tokio::select! {
                _ = idle_ticker.tick() => {
                    if self.motion.animated() {
                        self.activity_frame = self.activity_frame.wrapping_add(1);
                    }
                }
                maybe_request = recv_ext_ui(&mut ext_ui), if accept_ext_ui => {
                    match maybe_request {
                        Some(request) => self.handle_ext_ui_request(request),
                        // All handles dropped: stop polling the (now-dead) channel.
                        None => ext_ui = None,
                    }
                }
                maybe_event = events.next() => {
                    match maybe_event {
                        Some(Ok(Event::Key(key))) => {
                            if key.kind == KeyEventKind::Release {
                                continue;
                            }
                            self.handle_key_idle(key, terminal, &mut events, &mut ext_ui).await?;
                        }
                        Some(Ok(Event::Mouse(mouse))) => {
                            scroll_transcript_on_mouse(&self.state, mouse);
                        }
                        Some(Ok(Event::Paste(text))) => {
                            self.handle_paste_idle(&text, terminal, &mut events).await?;
                        }
                        Some(Ok(_)) => {} // resize — redraw next iteration.
                        Some(Err(err)) => return Err(err),
                        // Terminal input closed: exit cleanly instead of spinning on a
                        // now-always-ready `None`.
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    /// Handle a key while idle (no run in progress).
    async fn handle_key_idle(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
        ext_ui: &mut Option<ExtensionUiChannel>,
    ) -> io::Result<()> {
        // Modal overlay gets keys first.
        if self.modal.is_some() {
            self.handle_modal_key(key, terminal, events).await?;
            return Ok(());
        }
        let kb = self.settings.keybindings.clone();
        // Enter submits (unless a completion is pending); Shift+Enter is newline.
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
            self.submit_prompt(terminal, events, ext_ui).await?;
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
        // Transcript scrollback (PageUp/PageDown, and Shift+Home/End to jump). The
        // editor never needs these (the composer is at most a handful of lines), so
        // routing them to the transcript matches Claude Code without stealing text
        // navigation.
        if scroll_transcript_on_key(&self.state, key) {
            return Ok(());
        }
        // Otherwise feed the editor and recompute completions.
        self.textarea.input(Event::Key(key));
        self.rebuild_completion();
        Ok(())
    }

    /// Handle a bracketed-paste while idle. A paste into an open modal is
    /// replayed as individual character keys (so the modal's text fields keep
    /// working, e.g. pasting an OAuth code or API key); otherwise the paste goes
    /// to the composer, where a terminal-generated file drop is normalized.
    async fn handle_paste_idle(
        &mut self,
        text: &str,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) -> io::Result<()> {
        if self.modal.is_some() {
            for key in paste_as_key_events(text) {
                self.handle_modal_key(key, terminal, events).await?;
                if self.modal.is_none() {
                    break;
                }
            }
            return Ok(());
        }
        self.paste_into_composer(text);
        Ok(())
    }

    /// Insert pasted text into the composer, normalizing terminal-generated file
    /// drops to clean paths (tau `PromptInput.on_paste` + `_insert_dropped_paths`).
    fn paste_into_composer(&mut self, text: &str) {
        insert_paste(&mut self.textarea, text);
        self.rebuild_completion();
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

    async fn handle_modal_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) -> io::Result<()> {
        let Some(modal) = self.modal.as_mut() else {
            return Ok(());
        };
        let outcome = modal.handle_key(key);
        match outcome {
            // `OAuthManualCode` only has a home in `drive_oauth_login`; a stray one
            // from the idle handler has nowhere to go, so it joins the no-ops.
            ModalOutcome::Consumed | ModalOutcome::OAuthManualCode(_) => {}
            ModalOutcome::Cancelled => self.cancel_modal(),
            ModalOutcome::LoginMethod(method) => self.handle_login_method(method),
            ModalOutcome::LoginProvider { name, method } => {
                self.open_login(&name, method, terminal, events).await;
            }
            ModalOutcome::Logout(name) => self.handle_logout(&name),
            ModalOutcome::LoginBack => self.open_login_method_picker(),
            ModalOutcome::ApiKey(api_key) => self.handle_api_key_login(&api_key),
            ModalOutcome::CustomProvider(draft) => self.handle_custom_provider(&draft),
            ModalOutcome::ExtensionSelect(_)
            | ModalOutcome::ExtensionConfirm(_)
            | ModalOutcome::ExtensionInput(_) => self.resolve_ext_ui_outcome(outcome),
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
        Ok(())
    }

    /// Close the active modal, resolving any pending extension dialog as a cancel
    /// so the awaiting `context.ui.*` call is never left hanging.
    fn cancel_modal(&mut self) {
        match self.pending_ext_ui.take() {
            Some(PendingUiResponder::Select(responder) | PendingUiResponder::Input(responder)) => {
                let _ = responder.send(None);
            }
            Some(PendingUiResponder::Confirm(responder)) => {
                let _ = responder.send(false);
            }
            None => {}
        }
        self.modal = None;
    }

    /// Answer the awaiting `context.ui.*` call with a resolved modal outcome and
    /// close the modal.
    fn resolve_ext_ui_outcome(&mut self, outcome: ModalOutcome) {
        match (self.pending_ext_ui.take(), outcome) {
            (
                Some(PendingUiResponder::Select(responder)),
                ModalOutcome::ExtensionSelect(choice),
            ) => {
                let _ = responder.send(choice);
            }
            (Some(PendingUiResponder::Confirm(responder)), ModalOutcome::ExtensionConfirm(ok)) => {
                let _ = responder.send(ok);
            }
            (Some(PendingUiResponder::Input(responder)), ModalOutcome::ExtensionInput(value)) => {
                let _ = responder.send(value);
            }
            _ => {}
        }
        self.modal = None;
    }

    /// Handle an extension UI request by opening the matching modal (or, for a
    /// notification, appending a transcript status line).
    fn handle_ext_ui_request(&mut self, request: UiRequest) {
        match request {
            UiRequest::Notify { message, level } => {
                let role = if level == "error" {
                    crate::state::ChatItemRole::Error
                } else {
                    crate::state::ChatItemRole::Status
                };
                self.state.add_item(role, message);
            }
            UiRequest::Select {
                title,
                options,
                responder,
            } => {
                self.modal = Some(Modal::ExtensionSelect(ExtensionSelectModal::new(
                    title, options,
                )));
                self.pending_ext_ui = Some(PendingUiResponder::Select(responder));
            }
            UiRequest::Confirm {
                title,
                message,
                responder,
            } => {
                self.modal = Some(Modal::ExtensionConfirm(ExtensionConfirmModal::new(
                    title, message,
                )));
                self.pending_ext_ui = Some(PendingUiResponder::Confirm(responder));
            }
            UiRequest::Input {
                title,
                placeholder,
                responder,
            } => {
                self.modal = Some(Modal::ExtensionInput(ExtensionInputModal::new(
                    title,
                    placeholder,
                )));
                self.pending_ext_ui = Some(PendingUiResponder::Input(responder));
            }
        }
    }

    /// Submit the current prompt: accept a pending completion, run a terminal
    /// command, dispatch a slash command, or start/steer a real prompt turn.
    async fn submit_prompt(
        &mut self,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
        ext_ui: &mut Option<ExtensionUiChannel>,
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
        // A user-driven submission jumps back to the tail (tau's `follow_output`),
        // so the new message + response are visible even if the user had scrolled
        // up to read history.
        self.state.follow_transcript_tail();

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
                self.apply_command_result(result, terminal, events).await;
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
        // Optimistic echo: render the user's message on the very next frame, before
        // `prompt()`'s stream echoes it back (on the first turn the stream does
        // lazy session-file creation + indexing + turn assembly *before* emitting
        // the user message, which made the first message appear to lag). The
        // adapter reconciles the real user `MessageEnd` against this marker so the
        // line is never double-rendered.
        self.state.add_optimistic_user_echo(&text);
        self.run_turn(text, terminal, events, ext_ui).await
    }

    /// Apply a handled slash-command result: the picker/toggle/exit effects that
    /// have an M5 home, the login/logout flows (M7), plus any user-facing message.
    /// Effects that need the resume launcher (new-session / resume-id) surface a
    /// notice; that deferral is journaled in `phase-5.md`.
    async fn apply_command_result(
        &mut self,
        result: rho_coding::commands::CommandResult,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) {
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
        if result.login_picker_requested {
            self.open_login_method_picker();
            return;
        }
        if result.custom_provider_login_requested {
            self.modal = Some(Modal::CustomProviderLogin(CustomProviderLoginModal::new()));
            return;
        }
        if let Some(provider) = &result.login_provider {
            let method = login_method_from_str(result.login_method.as_deref());
            self.open_login(provider, method, terminal, events).await;
            return;
        }
        if result.logout_picker_requested {
            self.open_logout_picker();
            return;
        }
        if let Some(provider) = &result.logout_provider {
            self.handle_logout(provider);
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

    // --- login / logout flow (tau's `_open_login*` / `_handle_*` helpers) ----

    /// Open the login method picker (tau `_open_login_picker`).
    fn open_login_method_picker(&mut self) {
        self.login_target = None;
        self.modal = Some(Modal::LoginMethodPicker(LoginMethodPickerModal::new()));
    }

    /// Route a chosen login method to the right provider picker or the custom
    /// flow (tau `_handle_login_method_result`).
    fn handle_login_method(&mut self, method: LoginMethod) {
        match method {
            LoginMethod::Custom => {
                self.modal = Some(Modal::CustomProviderLogin(CustomProviderLoginModal::new()));
            }
            LoginMethod::Subscription | LoginMethod::ApiKey => {
                let providers = login_provider_items(method);
                if providers.is_empty() {
                    self.notice("Login", "No login providers are available for that method.");
                    return;
                }
                self.modal = Some(Modal::LoginProviderPicker(LoginProviderPickerModal::new(
                    providers,
                    ProviderPickerPurpose::Login { method },
                    "Login",
                )));
            }
        }
    }

    /// Open the login screen for `provider` (tau `_open_login`): the OAuth flow if
    /// the method/provider is subscription-backed, else the API-key prompt.
    async fn open_login(
        &mut self,
        provider: &str,
        method: Option<LoginMethod>,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) {
        let Some(entry) = builtin_provider_entry(provider) else {
            self.notice("Login", format!("Unknown provider: {provider}"));
            return;
        };
        let use_oauth = matches!(method, Some(LoginMethod::Subscription))
            || (method.is_none() && !entry.auth_methods.iter().any(|m| m == "api_key"));
        if use_oauth {
            if let Some(kind) = oauth_login_kind(&entry.name) {
                self.drive_oauth_login(entry, kind, terminal, events).await;
                return;
            }
        }
        // API-key screen: remember which provider it targets for the result.
        self.login_target = Some(entry.name.clone());
        self.modal = Some(Modal::ApiKeyLogin(ApiKeyLoginModal::new(
            entry.display_name.clone(),
        )));
    }

    /// Persist an entered API key and swap the session provider (tau
    /// `_handle_login_result`).
    fn handle_api_key_login(&mut self, api_key: &str) {
        let Some(name) = self.login_target.take() else {
            self.modal = None;
            return;
        };
        let Some(entry) = builtin_provider_entry(&name) else {
            self.notice("Login", format!("Unknown provider: {name}"));
            return;
        };
        let store = FileCredentialStore::at_default();
        match persist_api_key_login(&store, &entry, api_key, None) {
            Ok(display) => self.finish_login_swap(&entry.name, &display),
            Err(err) => self.notice("Login", format!("Could not save login: {err}")),
        }
    }

    /// Persist a custom provider and swap the session provider (tau
    /// `_handle_custom_provider_login_result`).
    fn handle_custom_provider(&mut self, draft: &CustomProviderDraft) {
        let store = FileCredentialStore::at_default();
        match persist_custom_provider(&store, draft, None) {
            Ok(name) => {
                let display = draft.display_name.clone();
                self.finish_login_swap(&name, &display);
            }
            Err(err) => self.notice("Login", format!("Could not save custom provider: {err}")),
        }
    }

    /// Open the logout provider picker (tau `_open_logout_picker`).
    fn open_logout_picker(&mut self) {
        let providers = stored_credential_provider_items();
        if providers.is_empty() {
            self.notice("Logout", NO_STORED_CREDENTIALS_MESSAGE);
            return;
        }
        self.modal = Some(Modal::LoginProviderPicker(LoginProviderPickerModal::new(
            providers,
            ProviderPickerPurpose::Logout,
            "Logout",
        )));
    }

    /// Remove a provider's stored credentials (tau `_logout`). Handles both
    /// built-in providers and saved custom providers (whose credential is keyed
    /// by the provider id, not the built-in catalog).
    fn handle_logout(&mut self, provider: &str) {
        let store = FileCredentialStore::at_default();
        match remove_credentials(&store, provider, None) {
            Ok(false) => self.notice("Logout", NO_STORED_CREDENTIALS_MESSAGE),
            Ok(true) => {
                let _ = self.session.reload_provider_settings();
                // Built-ins carry a display name + a codex-specific message; a custom
                // provider falls back to its id and the generic API-key wording.
                let message = match builtin_provider_entry(provider) {
                    Some(entry) if entry.kind == "openai-codex" => {
                        format!("Logged out of {}.", entry.display_name)
                    }
                    Some(entry) => format!(
                        "Removed stored API key for {}. Environment variables and \
                         providers.json config are unchanged.",
                        entry.display_name
                    ),
                    None => format!(
                        "Removed stored API key for {provider}. Environment variables and \
                         providers.json config are unchanged."
                    ),
                };
                self.notice("Logout", message);
                self.refresh_chrome();
            }
            Err(err) => self.notice("Logout", format!("Could not log out: {err}")),
        }
    }

    /// Reload provider settings and switch the live provider after a successful
    /// login (tau's tail of `_handle_*_login_result`).
    fn finish_login_swap(&mut self, provider: &str, display_name: &str) {
        if let Err(err) = self.session.reload_provider_settings() {
            self.notice("Login", format!("Could not save login: {err}"));
            return;
        }
        if let Err(err) = self.session.set_provider(provider, false) {
            self.notice("Login", format!("Could not save login: {err}"));
            return;
        }
        self.notice("Login", format!("Saved login for {display_name}."));
        self.refresh_chrome();
    }

    /// Show a dismissible notice and add a transcript status line (tau `_notify`
    /// + the modal fallback).
    fn notice(&mut self, title: &str, message: impl Into<String>) {
        let message = message.into();
        self.state
            .add_item(crate::state::ChatItemRole::Status, message.clone());
        self.modal = Some(Modal::Notice(crate::modals::NoticeModal::new(
            title.to_string(),
            message,
        )));
    }

    /// Drive an interactive OAuth login on a background task while reflecting its
    /// progress in the [`OAuthLoginModal`] (tau `OAuthLoginScreen._run_login`).
    async fn drive_oauth_login(
        &mut self,
        entry: ProviderCatalogEntry,
        kind: crate::login::OAuthLoginKind,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
    ) {
        let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<OAuthUpdate>();
        let (code_tx, code_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let code_rx = std::sync::Arc::new(tokio::sync::Mutex::new(code_rx));
        let mut task = spawn_oauth_login(kind, update_tx, code_rx);

        self.modal = Some(Modal::OAuthLogin(OAuthLoginModal::new(
            entry.display_name.clone(),
        )));
        let mut outcome: Option<Result<rho_coding::credentials::OAuthCredential, String>> = None;
        // Escape/Ctrl+D cancel: abort the task and either go back or close.
        let mut navigate_back = false;
        let mut cancelled = false;

        loop {
            terminal.draw(|f| render(f, &self.render_ctx())).ok();

            tokio::select! {
                update = update_rx.recv() => {
                    if let Some(update) = update {
                        if let Some(Modal::OAuthLogin(m)) = self.modal.as_mut() {
                            apply_oauth_update(m, update);
                        }
                    }
                }
                joined = &mut task => {
                    outcome = Some(joined.unwrap_or_else(|err| Err(format!("login task failed: {err}"))));
                    break;
                }
                maybe_event = events.next() => {
                    // A paste (e.g. the manual OAuth code) is replayed as
                    // individual character keys so the modal's code field fills in.
                    let keys: Vec<KeyEvent> = match maybe_event {
                        Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => vec![key],
                        Some(Ok(Event::Paste(text))) => paste_as_key_events(&text),
                        Some(Ok(_)) => Vec::new(),
                        Some(Err(_)) | None => {
                            cancelled = true;
                            break;
                        }
                    };
                    let mut should_break = false;
                    for key in keys {
                        let action = self
                            .modal
                            .as_mut()
                            .map_or(ModalOutcome::Consumed, |m| m.handle_key(key));
                        match action {
                            ModalOutcome::OAuthManualCode(code) => {
                                let _ = code_tx.send(code);
                            }
                            ModalOutcome::LoginBack => {
                                navigate_back = true;
                                should_break = true;
                                break;
                            }
                            ModalOutcome::Cancelled => {
                                cancelled = true;
                                should_break = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    if should_break {
                        break;
                    }
                }
            }
        }

        // Stop the flow if the user bailed before it finished.
        if navigate_back || cancelled {
            task.abort();
        }

        match outcome {
            Some(Ok(credential)) => {
                let store = FileCredentialStore::at_default();
                match persist_oauth_login(&store, &entry, &credential, None) {
                    Ok(display) => self.finish_login_swap(&entry.name, &display),
                    Err(err) => self.notice("Login", format!("Could not save login: {err}")),
                }
            }
            Some(Err(err)) => self.notice("Login", format!("OAuth failed: {err}")),
            None => {
                if navigate_back {
                    self.open_login_method_picker();
                } else {
                    self.modal = None;
                }
            }
        }
    }

    /// Drive one prompt turn: stream session events into the transcript while
    /// still polling the keyboard for steer / follow-up / cancel / toggles.
    async fn run_turn(
        &mut self,
        text: String,
        terminal: &mut Terminal<Backend>,
        events: &mut EventStream,
        ext_ui: &mut Option<ExtensionUiChannel>,
    ) -> io::Result<()> {
        // Acquire the control handle from the CURRENT harness: a model/provider
        // switch or a branch can rebuild the harness, so a handle captured earlier
        // would target a stale run's cancel token + queues (CR).
        let control = self.session.control();
        let keybindings = self.settings.keybindings.clone();
        // Mark the turn running BEFORE the first frame. The adapter only flips
        // `running` true once the first stream event lands, but the loop draws a
        // frame before that; without this, a slow provider (or pre-prompt work
        // like auto-compaction) would keep showing the empty-session splash after
        // the user already submitted, as if the submit were ignored.
        self.state.running = true;
        // Stamp the turn start so the working-state line's elapsed timer runs from
        // the moment of submit (cleared in `finish_turn`).
        self.state.turn_started_at = Some(std::time::Instant::now());
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
                motion,
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
                        sidebar_position: settings.sidebar_position,
                        keybindings: &settings.keybindings,
                        modal: None,
                        activity_frame: *activity_frame,
                        motion: *motion,
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
                    // Drain the extension-UI channel during the turn. A hook that
                    // calls `context.ui.*` blocks the agent stream (and thus this
                    // loop) until its one-shot is answered; without this branch that
                    // is a deadlock. Interactive dialogs can't open mid-turn (the
                    // running footer owns the overlay), so answer them with a cancel
                    // and surface a status line — the idle loop handles full dialogs.
                    maybe_ui = recv_ext_ui(ext_ui) => {
                        match maybe_ui {
                            Some(request) => answer_ext_ui_during_turn(state, request),
                            // All handles dropped: stop polling the dead channel.
                            None => *ext_ui = None,
                        }
                    }
                    _ = ticker.tick() => {
                        // Advance the activity frame so the prompt-area ember pulse and
                        // working-state motion keep animating; a still-executing tool row
                        // re-renders via the whole-second elapsed timer, not a spinner.
                        *activity_frame = activity_frame.wrapping_add(1);
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
                            Some(Ok(Event::Mouse(mouse))) => scroll_transcript_on_mouse(state, mouse),
                            Some(Ok(Event::Paste(text))) => insert_paste(textarea, &text),
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
        self.finish_turn(quit_requested);
        Ok(())
    }

    /// Post-turn cleanup: quit if requested, clear the running state, and
    /// surface any run error the session recorded (tau shows the failure in the
    /// transcript after the turn settles).
    fn finish_turn(&mut self, quit_requested: bool) {
        if quit_requested {
            self.should_quit = true;
        }
        // Clear the running flag we set before the first frame, in case the
        // stream ended without a settle event (the adapter clears it on settle /
        // error, but a bare stream close would otherwise leave it stuck true).
        self.state.running = false;
        // Retire the working-state timer and advance the turn counter so the next
        // turn rotates to the next forge-verb. Withdraw any still-pending optimistic
        // echo: if the turn ended without a matching user MessageEnd (an `input`
        // hook handled the prompt with no agent run), the provisional item is stale
        // and must not linger as an orphaned raw directive.
        self.state.turn_started_at = None;
        self.state.drop_optimistic_echo();
        self.state.turn_index = self.state.turn_index.wrapping_add(1);
        if let Some(error) = self.session.take_run_error() {
            self.state.error = Some(error.clone());
            self.state
                .add_item(crate::state::ChatItemRole::Error, format!("Error: {error}"));
        }
        self.refresh_chrome();
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
            // A mid-turn steering submission jumps back to the tail too, so a user
            // who had scrolled up still sees their message + the response.
            state.follow_transcript_tail();
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
            // Same for a queued follow-up.
            state.follow_transcript_tail();
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
    // Transcript scrollback stays live during a turn so the user can read history
    // while the agent streams (following re-arms when they scroll back down).
    if scroll_transcript_on_key(state, key) {
        return RunningKeyOutcome::Continue;
    }
    // Otherwise let the user keep composing the next steering message.
    textarea.input(Event::Key(key));
    RunningKeyOutcome::Continue
}

/// Replay pasted `text` as individual character key events so widgets that only
/// consume `Event::Key` (the login/OAuth modals) keep accepting pastes once
/// bracketed paste is enabled. Line breaks are dropped (those fields are
/// single-line); control characters are ignored.
fn paste_as_key_events(text: &str) -> Vec<KeyEvent> {
    text.chars()
        .filter(|c| !c.is_control())
        .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .collect()
}

/// Insert pasted `text` into `textarea`, normalizing a terminal-generated file
/// drop to clean, spaced paths and otherwise inserting the raw paste (tau
/// `PromptInput.on_paste` + `_insert_dropped_paths`).
fn insert_paste(textarea: &mut TextArea<'static>, text: &str) {
    let insertion = match normalize_dropped_paths(text) {
        Some(paths) => {
            let (before, after) = composer_cursor_context(textarea);
            pad_dropped_insertion(&paths, &before, &after)
        }
        None => text.to_string(),
    };
    textarea.insert_str(insertion);
}

/// The text immediately before and after the composer cursor on its current
/// line — enough for [`pad_dropped_insertion`] to decide on separating spaces.
fn composer_cursor_context(textarea: &TextArea<'static>) -> (String, String) {
    let (row, col) = textarea.cursor();
    let line = textarea.lines().get(row).cloned().unwrap_or_default();
    let split = line
        .char_indices()
        .nth(col)
        .map_or(line.len(), |(idx, _)| idx);
    let (before, after) = line.split_at(split.min(line.len()));
    (before.to_string(), after.to_string())
}

fn fresh_textarea() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_placeholder_text("Type a message, /command, or !shell");
    textarea
}

/// tau `NO_STORED_CREDENTIALS_MESSAGE`.
const NO_STORED_CREDENTIALS_MESSAGE: &str = "No stored credentials to remove. /logout only removes credentials saved by /login; \
     environment variables and providers.json config are unchanged.";

/// Map a `CommandResult::login_method` string to a [`LoginMethod`] (tau compares
/// the `"subscription"` / `"api-key"` literals).
fn login_method_from_str(method: Option<&str>) -> Option<LoginMethod> {
    match method {
        Some("subscription") => Some(LoginMethod::Subscription),
        Some("api-key") => Some(LoginMethod::ApiKey),
        _ => None,
    }
}

/// The providers offered for a login method (tau `_subscription_login_providers`
/// / `_api_key_login_providers`).
fn login_provider_items(method: LoginMethod) -> Vec<LoginProviderItem> {
    let ids = oauth_provider_ids();
    BUILTIN_PROVIDER_CATALOG
        .iter()
        .filter(|entry| match method {
            LoginMethod::Subscription => ids.contains(&entry.name),
            LoginMethod::ApiKey => entry.auth_methods.iter().any(|m| m == "api_key"),
            LoginMethod::Custom => false,
        })
        .map(|entry| LoginProviderItem::new(entry.name.clone(), entry.display_name.clone()))
        .collect()
}

/// The providers with stored credentials (tau `_stored_credential_providers`),
/// including saved custom providers so custom logins can be logged out.
fn stored_credential_provider_items() -> Vec<LoginProviderItem> {
    let store = FileCredentialStore::at_default();
    stored_credential_providers(&store, None)
        .into_iter()
        .map(|provider| LoginProviderItem::new(provider.name, provider.display_name))
        .collect()
}

/// Fold a background-task OAuth update into the modal's display state.
fn apply_oauth_update(modal: &mut OAuthLoginModal, update: OAuthUpdate) {
    match update {
        OAuthUpdate::Auth { url, instructions } => modal.set_auth(url, instructions),
        OAuthUpdate::DeviceCode {
            verification_uri,
            user_code,
        } => modal.set_device_code(verification_uri, &user_code),
        OAuthUpdate::Progress(message) => modal.set_help(message),
        OAuthUpdate::Prompt {
            message,
            allow_empty,
        } => modal.set_prompt(message, allow_empty),
        OAuthUpdate::Select { message, options } => modal.set_select(message, options),
    }
}

/// Await the next extension UI request, or never resolve when no channel is
/// wired (so the `select!` branch is inert until the host connects one).
async fn recv_ext_ui(channel: &mut Option<ExtensionUiChannel>) -> Option<UiRequest> {
    match channel {
        Some(chan) => chan.recv().await,
        None => std::future::pending().await,
    }
}

/// Answer an extension UI request received while a turn is streaming. A running
/// turn owns the terminal overlay (the footer, not a modal), so an interactive
/// dialog can't be shown here; instead the awaiting `context.ui.*` call is
/// resolved with a cancel so the agent stream is never deadlocked, and a status
/// line records what happened. Full interactive dialogs are handled by the idle
/// loop ([`App::handle_ext_ui_request`]) between turns.
fn answer_ext_ui_during_turn(state: &mut TuiState, request: UiRequest) {
    match request {
        UiRequest::Notify { message, level } => {
            let role = if level == "error" {
                crate::state::ChatItemRole::Error
            } else {
                crate::state::ChatItemRole::Status
            };
            state.add_item(role, message);
        }
        UiRequest::Select {
            title, responder, ..
        } => {
            let _ = responder.send(None);
            state.add_item(
                crate::state::ChatItemRole::Status,
                format!("Extension dialog \"{title}\" was dismissed during an active turn."),
            );
        }
        UiRequest::Input {
            title, responder, ..
        } => {
            let _ = responder.send(None);
            state.add_item(
                crate::state::ChatItemRole::Status,
                format!("Extension prompt \"{title}\" was dismissed during an active turn."),
            );
        }
        UiRequest::Confirm {
            title, responder, ..
        } => {
            let _ = responder.send(false);
            state.add_item(
                crate::state::ChatItemRole::Status,
                format!("Extension dialog \"{title}\" was declined during an active turn."),
            );
        }
    }
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
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn paste_as_key_events_maps_chars_and_drops_control() {
        let keys = paste_as_key_events("ab\n c");
        let codes: Vec<KeyCode> = keys.iter().map(|k| k.code).collect();
        assert_eq!(
            codes,
            vec![
                KeyCode::Char('a'),
                KeyCode::Char('b'),
                KeyCode::Char(' '),
                KeyCode::Char('c'),
            ]
        );
    }

    #[test]
    fn composer_cursor_context_splits_current_line_at_cursor() {
        let mut textarea = fresh_textarea();
        textarea.insert_str("hello world");
        // Cursor is at end after insert.
        let (before, after) = composer_cursor_context(&textarea);
        assert_eq!(before, "hello world");
        assert_eq!(after, "");
        // Move the cursor to the start of the line.
        textarea.move_cursor(tui_textarea::CursorMove::Head);
        let (before, after) = composer_cursor_context(&textarea);
        assert_eq!(before, "");
        assert_eq!(after, "hello world");
    }

    // --- full-frame layout snapshots (drive the real `render`) ---------------

    fn test_status() -> StatusInfo {
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

    fn test_sidebar() -> SidebarInfo {
        SidebarInfo {
            session_title: None,
            provider_name: "anthropic".to_string(),
            model: "claude".to_string(),
            thinking_display: "medium".to_string(),
            tools_count: 3,
            skills_count: 0,
            turn_count: 0,
            tool_call_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost: None,
            context_labels: Vec::new(),
            tool_names: vec!["read".to_string()],
            skill_names: Vec::new(),
            prompt_names: Vec::new(),
            extension_names: Vec::new(),
        }
    }

    /// Render a full frame through the real `render` and return the text grid
    /// (trailing whitespace trimmed per line), the same extraction the snapshot
    /// tests use. `MotionCaps::plain()` keeps the frame deterministic.
    fn render_app_frame(
        state: &TuiState,
        composer_text: &str,
        running: bool,
        width: u16,
        height: u16,
    ) -> String {
        let theme = TuiSettings::default().resolved_theme();
        let status = test_status();
        let sidebar = test_sidebar();
        let keybindings = TuiSettings::default().keybindings;
        let mut textarea = TextArea::from(
            composer_text
                .split('\n')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        textarea.set_placeholder_text("Type a message, /command, or !shell");
        let completion = CompletionState::default();
        let cache = RefCell::new(crate::widgets::TranscriptCache::default());
        let ctx = RenderCtx {
            state,
            textarea: &textarea,
            completion: &completion,
            theme: &theme,
            status: &status,
            sidebar: &sidebar,
            sidebar_position: TuiSettings::default().sidebar_position,
            keybindings: &keybindings,
            modal: None,
            activity_frame: 0,
            motion: MotionCaps::plain(),
            footer_mode: if running {
                FooterMode::Running
            } else {
                FooterMode::Normal
            },
            transcript_cache: &cache,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &ctx)).expect("draw");
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

    #[test]
    fn splash_shows_a_bordered_empty_composer() {
        // A fresh, idle session: the transcript pane hosts the splash, and the
        // composer at the bottom is a bordered box (rounded corners) — no stray
        // floating cursor block.
        let state = TuiState::new();
        let rendered = render_app_frame(&state, "", false, 60, 20);
        // The splash identity is present in the transcript pane.
        assert!(rendered.contains('ρ'), "splash mark present:\n{rendered}");
        // The composer wears a rounded border box (top + bottom border rows).
        assert!(
            rendered.contains('╭') && rendered.contains('╰'),
            "composer must be a bordered box:\n{rendered}"
        );
    }

    #[test]
    fn working_status_renders_above_the_composer() {
        // Mid-turn: the working-state signature (forge-verb + timer + interrupt
        // hint) must sit ABOVE the composer border, matching the Claude Code layout.
        let mut state = TuiState::new();
        state.add_item(crate::state::ChatItemRole::User, "hello");
        state.running = true;
        state.turn_started_at = Some(std::time::Instant::now());
        let rendered = render_app_frame(&state, "", true, 60, 20);
        let lines: Vec<&str> = rendered.lines().collect();
        let status_row = lines
            .iter()
            .position(|l| l.contains("to interrupt"))
            .expect("working status line is present");
        let top_border_row = lines
            .iter()
            .position(|l| l.contains('╭'))
            .expect("composer top border is present");
        assert!(
            status_row < top_border_row,
            "working status must be above the composer border:\n{rendered}"
        );
    }

    #[test]
    fn transcript_short_conversation_bottom_anchors() {
        // A SHORT conversation (fewer lines than the viewport) hugs the composer at
        // the BOTTOM with empty space ABOVE — the Claude Code / terminal-shell feel.
        // The transcript region's TOP rows are blank and the message text sits in the
        // LOWER rows, just above the status/composer.
        let mut state = TuiState::new();
        state.add_item(crate::state::ChatItemRole::User, "first message");
        state.add_item(crate::state::ChatItemRole::Assistant, "second message");
        let rendered = render_app_frame(&state, "", false, 50, 16);
        let lines: Vec<&str> = rendered.lines().collect();

        // The composer border marks the bottom of the transcript region.
        let composer_top = lines
            .iter()
            .position(|l| l.contains('╭'))
            .expect("composer top border is present");
        // The messages must land in the LOWER rows, just above the composer.
        let first_row = lines
            .iter()
            .position(|l| l.contains("first message"))
            .expect("first message is rendered");
        let second_row = lines
            .iter()
            .position(|l| l.contains("second message"))
            .expect("second message is rendered");
        assert!(
            first_row < second_row,
            "message order preserved:\n{rendered}"
        );
        assert!(
            second_row < composer_top,
            "messages sit above the composer:\n{rendered}"
        );
        // Bottom-anchored: the messages hug the composer, so the last message is in
        // the lower half of the transcript region (empty space is ABOVE, not below).
        assert!(
            first_row > composer_top / 2,
            "short conversation must be bottom-anchored (blank space above):\n{rendered}"
        );
        // The TOP rows of the transcript region are blank (the pad).
        assert!(
            lines[0].is_empty() && lines[1].is_empty(),
            "top of the transcript region must be blank padding:\n{rendered}"
        );
    }

    #[test]
    fn transcript_scrollback_reveals_history_then_rearms() {
        // Fill the transcript past the viewport, then drive scrollback and assert
        // the visible top line changes (history is reachable) and that returning to
        // the bottom re-arms follow so the tail shows again.
        let mut state = TuiState::new();
        for i in 1..=40 {
            state.add_item(
                crate::state::ChatItemRole::User,
                format!("history line {i}"),
            );
        }
        // Following → the tail is visible, the head is not.
        let tail = render_app_frame(&state, "", false, 50, 16);
        assert!(tail.contains("history line 40"), "tail visible:\n{tail}");
        assert!(!tail.contains("history line 1 "), "head scrolled off");

        // A single PageUp opts out of follow and moves the top line up (wheel-up
        // and PageUp share `scroll_transcript_up`, so this covers both paths).
        let tail_offset = state.transcript_scroll.get().offset;
        state.scroll_transcript_page_up();
        let scroll = state.transcript_scroll.get();
        assert!(!scroll.following, "PageUp opts out of follow");
        assert!(scroll.offset < tail_offset, "PageUp moves the top line up");
        // Scroll all the way to the top: the very first line becomes reachable and
        // the tail scrolls out of view — history is no longer lost.
        state.scroll_transcript_up(u16::MAX);
        let scrolled = render_app_frame(&state, "", false, 50, 16);
        assert!(
            scrolled.contains("history line 1\n"),
            "scrollback must reach the oldest history:\n{scrolled}"
        );
        assert!(
            !scrolled.contains("history line 40"),
            "the tail must have scrolled out of view:\n{scrolled}"
        );

        // Jump back to the bottom: follow re-arms and the tail shows again.
        state.follow_transcript_tail();
        let back = render_app_frame(&state, "", false, 50, 16);
        assert!(state.transcript_scroll.get().following, "follow re-armed");
        assert!(
            back.contains("history line 40"),
            "tail visible again:\n{back}"
        );
    }

    #[tokio::test]
    async fn running_turn_submission_rearms_transcript_follow() {
        // A steering / follow-up message submitted mid-turn (through
        // `handle_running_key`, not `submit_prompt`) must also jump back to the tail
        // so a scrolled-up user still sees their message + the response.
        let tmp = tempfile::tempdir().unwrap();
        let session = login_required_session(tmp.path()).await;
        let control = session.control();
        let kb = TuiSettings::default().keybindings;

        let detached = |state: &TuiState| {
            state.transcript_scroll.set(crate::state::TranscriptScroll {
                offset: 0,
                following: false,
                viewport_height: 6,
                total_lines: 60,
            });
        };

        let mut state = TuiState::new();
        for i in 1..=30 {
            state.add_item(crate::state::ChatItemRole::User, format!("line {i}"));
        }

        // Enter submits a steering message during the run → follow re-arms.
        detached(&state);
        let mut textarea = fresh_textarea();
        textarea.insert_str("steer this");
        let outcome = handle_running_key(
            key(KeyCode::Enter, KeyModifiers::empty()),
            &kb,
            &control,
            &mut textarea,
            &mut state,
        );
        assert!(matches!(outcome, RunningKeyOutcome::Continue));
        assert!(
            state.transcript_scroll.get().following,
            "a mid-turn steering submission must re-arm follow"
        );

        // The queue-follow-up binding queues a follow-up during the run → follow
        // re-arms too. (Default `alt+enter` is caught by the plain-Enter steer
        // branch above, so bind it to a distinct key to reach the follow-up path.)
        let mut kb_followup = kb.clone();
        kb_followup.queue_follow_up = "ctrl+g".to_string();
        detached(&state);
        let mut textarea = fresh_textarea();
        textarea.insert_str("and then this");
        let outcome = handle_running_key(
            key(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &kb_followup,
            &control,
            &mut textarea,
            &mut state,
        );
        assert!(matches!(outcome, RunningKeyOutcome::Continue));
        assert!(
            state.transcript_scroll.get().following,
            "a mid-turn follow-up submission must re-arm follow"
        );
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

    #[tokio::test]
    async fn ext_ui_dialog_during_turn_is_answered_not_deadlocked() {
        use crate::ext_ui::extension_ui_pair;
        // A dialog request arriving mid-turn must be answered (with a cancel) so the
        // extension's awaiting call resolves instead of blocking the agent stream.
        let (handle, mut channel) = extension_ui_pair();
        let mut state = TuiState::new();
        let confirm = tokio::spawn(async move { handle.confirm("Delete?", "Sure?").await });
        let request = channel.recv().await.expect("request");
        answer_ext_ui_during_turn(&mut state, request);
        // The confirm resolves to `false` (cancel) rather than hanging forever.
        assert!(!confirm.await.expect("join"));
        assert!(
            state
                .items
                .iter()
                .any(|item| item.role == crate::state::ChatItemRole::Status)
        );
    }

    #[tokio::test]
    async fn ext_ui_notify_during_turn_appends_status_line() {
        use crate::ext_ui::extension_ui_pair;
        let (handle, mut channel) = extension_ui_pair();
        let mut state = TuiState::new();
        handle.notify("build finished", "info");
        let request = channel.recv().await.expect("request");
        answer_ext_ui_during_turn(&mut state, request);
        assert!(state.items.iter().any(|item| item.text == "build finished"));
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
