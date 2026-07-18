//! Modal overlays (port of tau's `ModalScreen` subclasses in `app.py`).
//!
//! tau pushes each modal as its own Textual `ModalScreen`; rho models the whole
//! set as one [`Modal`] overlay enum the app renders on top of the main frame and
//! routes keys to first. Each variant owns its own cursor/search/mode state and
//! reports the user's choice through [`ModalOutcome`] from [`Modal::handle_key`].
//!
//! In-scope for M5 (ported here): session picker, theme picker, model picker
//! (with all/scoped tabs + live search), tree picker (+ its branch-summary
//! instructions sub-screen), and the scrollable command-output view. The login /
//! OAuth / extension-UI screens are deferred to M7 and surface through the
//! [`Modal::Notice`] stub with a clear "lands in M7" message.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use rho_coding::session::{ModelChoice, SessionTreeChoice};
use rho_coding::session_manager::CodingSessionRecord;

use crate::theme::{BUILTIN_TUI_THEME_NAMES, TuiTheme, TuiThemeName};
use crate::widgets::style::parse_style;

/// A tree-picker selection (tau `TreePickerResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreePickerResult {
    /// The entry to branch from.
    pub entry_id: String,
    /// Whether to summarize the branched-off tail.
    pub summarize: bool,
    /// Custom summarization instructions, if the user supplied them.
    pub custom_instructions: Option<String>,
}

/// What a modal reports back after handling a key (tau's `dismiss(value)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalOutcome {
    /// The key was consumed; the modal stays open.
    Consumed,
    /// The modal was cancelled/closed with no result.
    Cancelled,
    /// A session id was chosen (session picker).
    Session(String),
    /// A theme was chosen (theme picker).
    Theme(TuiThemeName),
    /// A model was chosen (model picker).
    Model(ModelChoice),
    /// The scoped-model set changed (model picker, scoped mode); app refreshes.
    ScopedToggle(ModelChoice),
    /// A branch was requested (tree picker / branch-summary instructions).
    Branch(TreePickerResult),
    /// The tree picker requested the custom-summary sub-screen for `entry_id`.
    OpenBranchSummary(String),
}

/// The active modal overlay.
pub enum Modal {
    /// Pick an indexed session to resume.
    SessionPicker(SessionPickerModal),
    /// Pick a built-in theme.
    ThemePicker(ThemePickerModal),
    /// Pick a model (with all/scoped tabs + search).
    ModelPicker(ModelPickerModal),
    /// Pick a session-tree entry to branch from.
    TreePicker(TreePickerModal),
    /// Enter custom branch-summary instructions.
    BranchSummaryInstructions(BranchSummaryModal),
    /// A scrollable read-only command-output view.
    CommandOutput(CommandOutputModal),
    /// A deferred/informational notice (login + extension flows land in M7).
    Notice(NoticeModal),
}

impl Modal {
    /// Route a key to the active modal, returning its outcome.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match self {
            Modal::SessionPicker(m) => m.handle_key(key),
            Modal::ThemePicker(m) => m.handle_key(key),
            Modal::ModelPicker(m) => m.handle_key(key),
            Modal::TreePicker(m) => m.handle_key(key),
            Modal::BranchSummaryInstructions(m) => m.handle_key(key),
            Modal::CommandOutput(m) => m.handle_key(key),
            Modal::Notice(m) => m.handle_key(key),
        }
    }

    /// Render the modal centered over the current frame.
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        match self {
            Modal::SessionPicker(m) => m.render(frame, area, theme),
            Modal::ThemePicker(m) => m.render(frame, area, theme),
            Modal::ModelPicker(m) => m.render(frame, area, theme),
            Modal::TreePicker(m) => m.render(frame, area, theme),
            Modal::BranchSummaryInstructions(m) => m.render(frame, area, theme),
            Modal::CommandOutput(m) => m.render(frame, area, theme),
            Modal::Notice(m) => m.render(frame, area, theme),
        }
    }
}

// --- shared helpers ---------------------------------------------------------

/// Centered rectangle `pct_x` × `pct_y` percent of `area` (modal framing).
#[must_use]
pub fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn modal_block(title: &str, theme: &TuiTheme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(parse_style(&theme.prompt_border))
        .title(Span::styled(
            format!(" {title} "),
            parse_style(&theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(parse_style(&theme.autocomplete_background))
}

/// Render a title + selectable list + help line into a bordered modal.
fn render_list_modal(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[Line<'static>],
    help: &str,
    theme: &TuiTheme,
) {
    frame.render_widget(Clear, area);
    let block = modal_block(title, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(rows.to_vec()), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help.to_string(),
            parse_style(&theme.completion_description),
        ))),
        chunks[1],
    );
}

/// Style a list row with the selected marker (tau's `› `/`  ` convention).
fn list_row(text: String, selected: bool, theme: &TuiTheme) -> Line<'static> {
    let (prefix, style) = if selected {
        ("› ", parse_style(&theme.completion_selected))
    } else {
        ("  ", parse_style(&theme.prompt_text))
    };
    Line::from(vec![
        Span::styled(prefix.to_string(), style),
        Span::styled(text, style),
    ])
}

/// Move `index` within `[0, len)`, saturating at the ends (tau list nav).
fn move_cursor(index: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    let next = index as i32 + delta;
    next.clamp(0, max as i32) as usize
}

// --- session picker ---------------------------------------------------------

/// Pick an indexed session to resume (tau `SessionPickerScreen`).
pub struct SessionPickerModal {
    records: Vec<CodingSessionRecord>,
    index: usize,
}

impl SessionPickerModal {
    /// Build from indexed session records.
    #[must_use]
    pub fn new(records: Vec<CodingSessionRecord>) -> Self {
        Self { records, index: 0 }
    }

    fn label(record: &CodingSessionRecord) -> String {
        let title = record.title.as_deref().unwrap_or("Untitled");
        format!("{}  {}  {}", record.id, title, record.model)
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Up => {
                self.index = move_cursor(self.index, self.records.len(), -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.index = move_cursor(self.index, self.records.len(), 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match self.records.get(self.index) {
                Some(record) => ModalOutcome::Session(record.id.clone()),
                None => ModalOutcome::Cancelled,
            },
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 60, area);
        let rows: Vec<Line<'static>> = if self.records.is_empty() {
            vec![Line::from(Span::styled(
                "  No sessions found".to_string(),
                parse_style(&theme.completion_description),
            ))]
        } else {
            self.records
                .iter()
                .enumerate()
                .map(|(i, r)| list_row(Self::label(r), i == self.index, theme))
                .collect()
        };
        render_list_modal(
            frame,
            area,
            "Sessions",
            &rows,
            "Enter selects · Escape closes",
            theme,
        );
    }
}

// --- theme picker -----------------------------------------------------------

/// Pick a built-in theme (tau `ThemePickerScreen`).
pub struct ThemePickerModal {
    current: TuiThemeName,
    index: usize,
}

impl ThemePickerModal {
    /// Build with the currently active theme pre-selected.
    #[must_use]
    pub fn new(current: TuiThemeName) -> Self {
        let index = BUILTIN_TUI_THEME_NAMES
            .iter()
            .position(|name| *name == current.as_str())
            .unwrap_or(0);
        Self { current, index }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Up => {
                self.index = move_cursor(self.index, BUILTIN_TUI_THEME_NAMES.len(), -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.index = move_cursor(self.index, BUILTIN_TUI_THEME_NAMES.len(), 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match BUILTIN_TUI_THEME_NAMES.get(self.index) {
                Some(name) => match TuiThemeName::parse(name) {
                    Some(theme) => ModalOutcome::Theme(theme),
                    None => ModalOutcome::Cancelled,
                },
                None => ModalOutcome::Cancelled,
            },
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(50, 40, area);
        let rows: Vec<Line<'static>> = BUILTIN_TUI_THEME_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let marker = if *name == self.current.as_str() {
                    " ✓"
                } else {
                    ""
                };
                list_row(format!("{name}{marker}"), i == self.index, theme)
            })
            .collect();
        render_list_modal(
            frame,
            area,
            "Theme",
            &rows,
            "Enter selects · Escape closes",
            theme,
        );
    }
}

// --- model picker -----------------------------------------------------------

/// Which tab the model picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelPickerMode {
    All,
    Scoped,
}

/// What the model picker is being used for (tau `picker_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerKind {
    /// Choose the active model (selection dismisses with a `ModelChoice`).
    Model,
    /// Edit the scoped-model set (selection toggles membership, stays open).
    Scoped,
}

/// Pick a model, with all/scoped tabs and live search (tau `ModelPickerScreen`).
pub struct ModelPickerModal {
    choices: Vec<ModelChoice>,
    scoped: Vec<ModelChoice>,
    current_model: String,
    provider_name: String,
    kind: ModelPickerKind,
    mode: ModelPickerMode,
    search: String,
    index: usize,
}

impl ModelPickerModal {
    /// Build a model picker.
    #[must_use]
    pub fn new(
        choices: Vec<ModelChoice>,
        scoped: Vec<ModelChoice>,
        current_model: String,
        provider_name: String,
        kind: ModelPickerKind,
    ) -> Self {
        let mode = match kind {
            ModelPickerKind::Model => ModelPickerMode::All,
            ModelPickerKind::Scoped => ModelPickerMode::Scoped,
        };
        let mut modal = Self {
            choices,
            scoped,
            current_model,
            provider_name,
            kind,
            mode,
            search: String::new(),
            index: 0,
        };
        modal.reset_index();
        modal
    }

    fn base(&self) -> &[ModelChoice] {
        match self.mode {
            ModelPickerMode::All => &self.choices,
            ModelPickerMode::Scoped => &self.scoped,
        }
    }

    /// Case-insensitive substring match on provider or model (tau
    /// `_filter_model_choices`).
    fn visible(&self) -> Vec<ModelChoice> {
        let query = self.search.trim().to_lowercase();
        self.base()
            .iter()
            .filter(|choice| {
                query.is_empty()
                    || choice.model.to_lowercase().contains(&query)
                    || choice.provider_name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    fn reset_index(&mut self) {
        let visible = self.visible();
        self.index = visible
            .iter()
            .position(|c| c.model == self.current_model && c.provider_name == self.provider_name)
            .unwrap_or(0);
    }

    fn is_scoped(&self, choice: &ModelChoice) -> bool {
        self.scoped.contains(choice)
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => return ModalOutcome::Cancelled,
            KeyCode::Up => {
                let len = self.visible().len();
                self.index = move_cursor(self.index, len, -1);
                return ModalOutcome::Consumed;
            }
            KeyCode::Down => {
                let len = self.visible().len();
                self.index = move_cursor(self.index, len, 1);
                return ModalOutcome::Consumed;
            }
            KeyCode::Tab if self.kind == ModelPickerKind::Model => {
                self.mode = match self.mode {
                    ModelPickerMode::All => ModelPickerMode::Scoped,
                    ModelPickerMode::Scoped => ModelPickerMode::All,
                };
                self.reset_index();
                return ModalOutcome::Consumed;
            }
            KeyCode::Enter => {
                let visible = self.visible();
                if let Some(choice) = visible.get(self.index).cloned() {
                    return match self.kind {
                        ModelPickerKind::Model => ModalOutcome::Model(choice),
                        ModelPickerKind::Scoped => {
                            // Toggle membership; stay open (tau scoped behavior).
                            if let Some(pos) = self.scoped.iter().position(|c| c == &choice) {
                                self.scoped.remove(pos);
                            } else {
                                self.scoped.push(choice.clone());
                            }
                            ModalOutcome::ScopedToggle(choice)
                        }
                    };
                }
                return ModalOutcome::Cancelled;
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.reset_index();
                return ModalOutcome::Consumed;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(c);
                self.reset_index();
                return ModalOutcome::Consumed;
            }
            _ => {}
        }
        ModalOutcome::Consumed
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 70, area);
        let title = match self.kind {
            ModelPickerKind::Model => format!("Model: {}", self.provider_name),
            ModelPickerKind::Scoped => "Scoped models".to_string(),
        };
        let visible = self.visible();
        let mut rows: Vec<Line<'static>> = Vec::new();
        let tabs = match self.mode {
            ModelPickerMode::All => "Tabs: ● All models  ○ Scoped models",
            ModelPickerMode::Scoped => "Tabs: ○ All models  ● Scoped models",
        };
        rows.push(Line::from(Span::styled(
            tabs.to_string(),
            parse_style(&theme.completion_description),
        )));
        rows.push(Line::from(Span::styled(
            format!("search: {}", self.search),
            parse_style(&theme.muted_text),
        )));
        rows.push(Line::default());
        if visible.is_empty() {
            rows.push(Line::from(Span::styled(
                "  No matching models".to_string(),
                parse_style(&theme.completion_description),
            )));
        } else {
            for (i, choice) in visible.iter().enumerate() {
                let current = choice.model == self.current_model
                    && choice.provider_name == self.provider_name;
                let mut label = format!("{}:{}", choice.provider_name, choice.model);
                if current {
                    label.push_str(" (current)");
                }
                if self.is_scoped(choice) {
                    label.push_str(" ★");
                }
                rows.push(list_row(label, i == self.index, theme));
            }
        }
        let help = match self.kind {
            ModelPickerKind::Model => "Enter selects · Tab switches tabs · Escape closes",
            ModelPickerKind::Scoped => "Enter toggles scoped · Escape closes",
        };
        render_list_modal(frame, area, &title, &rows, help, theme);
    }
}

// --- tree picker ------------------------------------------------------------

/// Pick a session-tree entry to branch from (tau `TreePickerScreen`).
pub struct TreePickerModal {
    choices: Vec<SessionTreeChoice>,
    show_tool_calls: bool,
    index: usize,
}

impl TreePickerModal {
    /// Build from the session's tree choices, pre-selecting the active leaf.
    #[must_use]
    pub fn new(choices: Vec<SessionTreeChoice>) -> Self {
        let mut modal = Self {
            choices,
            show_tool_calls: true,
            index: 0,
        };
        modal.index = modal
            .visible_indices()
            .iter()
            .position(|i| modal.choices[*i].active)
            .unwrap_or(0);
        modal
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.choices
            .iter()
            .enumerate()
            .filter(|(_, c)| self.show_tool_calls || !c.is_tool_call)
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_entry(&self) -> Option<&SessionTreeChoice> {
        let visible = self.visible_indices();
        visible.get(self.index).map(|i| &self.choices[*i])
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let len = self.visible_indices().len();
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Up => {
                self.index = move_cursor(self.index, len, -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.index = move_cursor(self.index, len, 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match self.selected_entry() {
                Some(entry) => ModalOutcome::Branch(TreePickerResult {
                    entry_id: entry.entry_id.clone(),
                    summarize: false,
                    custom_instructions: None,
                }),
                None => ModalOutcome::Cancelled,
            },
            KeyCode::Char('s') => match self.selected_entry() {
                Some(entry) => ModalOutcome::Branch(TreePickerResult {
                    entry_id: entry.entry_id.clone(),
                    summarize: true,
                    custom_instructions: None,
                }),
                None => ModalOutcome::Consumed,
            },
            KeyCode::Char('c') => match self.selected_entry() {
                Some(entry) => ModalOutcome::OpenBranchSummary(entry.entry_id.clone()),
                None => ModalOutcome::Consumed,
            },
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let entry_id = self.selected_entry().map(|e| e.entry_id.clone());
                self.show_tool_calls = !self.show_tool_calls;
                // Preserve the selected entry across the filter change.
                if let Some(id) = entry_id {
                    let visible = self.visible_indices();
                    if let Some(pos) = visible.iter().position(|i| self.choices[*i].entry_id == id)
                    {
                        self.index = pos;
                    } else {
                        self.index = 0;
                    }
                }
                ModalOutcome::Consumed
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 70, area);
        let visible = self.visible_indices();
        let rows: Vec<Line<'static>> = if visible.is_empty() {
            vec![Line::from(Span::styled(
                "  No entries".to_string(),
                parse_style(&theme.completion_description),
            ))]
        } else {
            visible
                .iter()
                .enumerate()
                .map(|(pos, choice_idx)| {
                    let choice = &self.choices[*choice_idx];
                    let marker = if choice.active { "◉ " } else { "" };
                    list_row(
                        format!("{marker}{}", choice.label),
                        pos == self.index,
                        theme,
                    )
                })
                .collect()
        };
        let shown = if self.show_tool_calls {
            "shown"
        } else {
            "hidden"
        };
        let help = format!(
            "Enter branches · S summarizes · C custom summary · Ctrl+T tool calls {shown} · Escape closes"
        );
        render_list_modal(frame, area, "Session Tree", &rows, &help, theme);
    }
}

// --- branch-summary instructions --------------------------------------------

/// Enter custom branch-summary instructions (tau
/// `BranchSummaryInstructionsScreen`, launched from the tree picker's `c`).
pub struct BranchSummaryModal {
    entry_id: String,
    text: String,
}

impl BranchSummaryModal {
    /// Build for the given branch entry.
    #[must_use]
    pub fn new(entry_id: String) -> Self {
        Self {
            entry_id,
            text: String::new(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let trimmed = self.text.trim();
                ModalOutcome::Branch(TreePickerResult {
                    entry_id: self.entry_id.clone(),
                    summarize: true,
                    custom_instructions: if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    },
                })
            }
            KeyCode::Enter => {
                self.text.push('\n');
                ModalOutcome::Consumed
            }
            KeyCode::Backspace => {
                self.text.pop();
                ModalOutcome::Consumed
            }
            KeyCode::Char(c) => {
                self.text.push(c);
                ModalOutcome::Consumed
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 50, area);
        frame.render_widget(Clear, area);
        let block = modal_block("Custom summarization instructions", theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        frame.render_widget(
            Paragraph::new(self.text.clone())
                .wrap(Wrap { trim: false })
                .style(parse_style(&theme.prompt_text)),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Ctrl+Enter submits · Escape returns to tree".to_string(),
                parse_style(&theme.completion_description),
            ))),
            chunks[1],
        );
    }
}

// --- command output ---------------------------------------------------------

/// A scrollable read-only command-output view (tau `CommandOutputScreen`).
pub struct CommandOutputModal {
    title: String,
    lines: Vec<String>,
    scroll: u16,
}

impl CommandOutputModal {
    /// Build from a title and body text.
    #[must_use]
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        let body = body.into();
        Self {
            title: title.into(),
            lines: body.lines().map(str::to_string).collect(),
            scroll: 0,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => ModalOutcome::Cancelled,
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                ModalOutcome::Consumed
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(80, 70, area);
        frame.render_widget(Clear, area);
        let block = modal_block(&self.title, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let body: Vec<Line<'static>> = self
            .lines
            .iter()
            .map(|l| Line::from(Span::styled(l.clone(), parse_style(&theme.prompt_text))))
            .collect();
        frame.render_widget(Paragraph::new(body).scroll((self.scroll, 0)), chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Enter or Escape closes".to_string(),
                parse_style(&theme.completion_description),
            ))),
            chunks[1],
        );
    }
}

// --- notice (M7 stub) -------------------------------------------------------

/// A dismissible informational notice — the M5 stand-in for the login / OAuth /
/// extension-UI screens, which land in M7.
pub struct NoticeModal {
    title: String,
    message: String,
}

impl NoticeModal {
    /// Build a notice.
    #[must_use]
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
        }
    }

    /// The standard "this flow lands in M7" notice for a named feature.
    #[must_use]
    pub fn m7(feature: &str) -> Self {
        Self::new(
            feature.to_string(),
            format!(
                "{feature} lands in M7 (the WASM extension / login runtime). Press Escape to close."
            ),
        )
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => ModalOutcome::Cancelled,
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 30, area);
        frame.render_widget(Clear, area);
        let block = modal_block(&self.title, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(self.message.clone())
                .wrap(Wrap { trim: true })
                .style(parse_style(&theme.prompt_text)),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn choice(model: &str) -> ModelChoice {
        ModelChoice::new("anthropic", model)
    }

    #[test]
    fn session_picker_navigates_and_selects() {
        let records = vec![
            CodingSessionRecord {
                id: "a".into(),
                path: "/x/a.jsonl".into(),
                cwd: "/x".into(),
                model: "m".into(),
                title: Some("first".into()),
                created_at: 0.0,
                updated_at: 0.0,
                provider_name: None,
            },
            CodingSessionRecord {
                id: "b".into(),
                path: "/x/b.jsonl".into(),
                cwd: "/x".into(),
                model: "m".into(),
                title: None,
                created_at: 0.0,
                updated_at: 0.0,
                provider_name: None,
            },
        ];
        let mut modal = SessionPickerModal::new(records);
        assert_eq!(modal.handle_key(key(KeyCode::Down)), ModalOutcome::Consumed);
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Session("b".into())
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Cancelled);
    }

    #[test]
    fn theme_picker_preselects_current_and_returns_choice() {
        let mut modal = ThemePickerModal::new(TuiThemeName::HighContrast);
        // high-contrast is index 2 in BUILTIN_TUI_THEME_NAMES.
        assert_eq!(modal.index, 2);
        modal.handle_key(key(KeyCode::Up));
        assert_eq!(modal.index, 1);
        match modal.handle_key(key(KeyCode::Enter)) {
            ModalOutcome::Theme(name) => assert_eq!(name.as_str(), BUILTIN_TUI_THEME_NAMES[1]),
            other => panic!("expected theme, got {other:?}"),
        }
    }

    #[test]
    fn model_picker_filters_by_search() {
        let mut modal = ModelPickerModal::new(
            vec![
                choice("claude-opus"),
                choice("claude-sonnet"),
                choice("gpt-4"),
            ],
            vec![],
            "claude-opus".into(),
            "anthropic".into(),
            ModelPickerKind::Model,
        );
        modal.handle_key(key(KeyCode::Char('g')));
        let visible = modal.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].model, "gpt-4");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Model(choice("gpt-4"))
        );
    }

    #[test]
    fn model_picker_tab_switches_mode() {
        let mut modal = ModelPickerModal::new(
            vec![choice("a"), choice("b")],
            vec![choice("b")],
            "a".into(),
            "anthropic".into(),
            ModelPickerKind::Model,
        );
        assert_eq!(modal.mode, ModelPickerMode::All);
        modal.handle_key(key(KeyCode::Tab));
        assert_eq!(modal.mode, ModelPickerMode::Scoped);
        // scoped tab shows only scoped models
        assert_eq!(modal.visible().len(), 1);
    }

    #[test]
    fn tree_picker_s_and_c_and_toggle() {
        let choices = vec![
            SessionTreeChoice {
                entry_id: "e1".into(),
                label: "root".into(),
                active: false,
                is_tool_call: false,
            },
            SessionTreeChoice {
                entry_id: "e2".into(),
                label: "tool".into(),
                active: true,
                is_tool_call: true,
            },
        ];
        let mut modal = TreePickerModal::new(choices);
        // active leaf (e2) preselected.
        match modal.handle_key(key(KeyCode::Enter)) {
            ModalOutcome::Branch(r) => {
                assert_eq!(r.entry_id, "e2");
                assert!(!r.summarize);
            }
            other => panic!("expected branch, got {other:?}"),
        }
        // 's' summarizes.
        match modal.handle_key(key(KeyCode::Char('s'))) {
            ModalOutcome::Branch(r) => assert!(r.summarize),
            other => panic!("expected branch, got {other:?}"),
        }
        // 'c' opens the custom-summary sub-screen.
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('c'))),
            ModalOutcome::OpenBranchSummary("e2".into())
        );
        // ctrl+t hides tool calls -> only e1 visible.
        modal.handle_key(ctrl(KeyCode::Char('t')));
        assert_eq!(modal.visible_indices(), vec![0]);
    }

    #[test]
    fn branch_summary_ctrl_enter_submits() {
        let mut modal = BranchSummaryModal::new("e1".into());
        modal.handle_key(key(KeyCode::Char('h')));
        modal.handle_key(key(KeyCode::Char('i')));
        match modal.handle_key(ctrl(KeyCode::Enter)) {
            ModalOutcome::Branch(r) => {
                assert_eq!(r.entry_id, "e1");
                assert!(r.summarize);
                assert_eq!(r.custom_instructions.as_deref(), Some("hi"));
            }
            other => panic!("expected branch, got {other:?}"),
        }
    }

    #[test]
    fn command_output_scrolls_and_closes() {
        let mut modal = CommandOutputModal::new("Output", "line1\nline2\nline3");
        assert_eq!(modal.handle_key(key(KeyCode::Down)), ModalOutcome::Consumed);
        assert_eq!(modal.scroll, 1);
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Cancelled
        );
    }

    #[test]
    fn notice_dismisses() {
        let mut modal = NoticeModal::m7("Login");
        assert!(modal.message.contains("M7"));
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Cancelled);
    }
}
