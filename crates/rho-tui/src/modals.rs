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
use ratatui::style::Modifier;
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

/// Which authentication method the user picked (tau's `LoginMethodPickerScreen`
/// dismiss values `"subscription"`/`"api-key"`/`"custom"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// OAuth subscription login.
    Subscription,
    /// Built-in-provider API-key login.
    ApiKey,
    /// Custom OpenAI-compatible provider.
    Custom,
}

impl LoginMethod {
    /// The tau wire string for this method.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::ApiKey => "api-key",
            Self::Custom => "custom",
        }
    }
}

/// Provider details collected by the custom-provider login flow (tau
/// `CustomProviderLoginResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProviderDraft {
    /// Stable provider id.
    pub provider_name: String,
    /// Human-facing display name.
    pub display_name: String,
    /// Base URL (trailing slash trimmed by the consumer).
    pub base_url: String,
    /// Environment variable holding the API key.
    pub api_key_env: String,
    /// Declared model ids, in order.
    pub models: Vec<String>,
    /// Default model id.
    pub default_model: String,
    /// The API key to store.
    pub api_key: String,
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
    /// A login method was chosen (login-method picker).
    LoginMethod(LoginMethod),
    /// A provider was chosen for login (provider picker), with the method that
    /// led here so the app can pick the OAuth vs API-key screen.
    LoginProvider {
        /// The chosen provider name.
        name: String,
        /// The method that led to this picker (`None` for a direct `/login foo`).
        method: Option<LoginMethod>,
    },
    /// A provider was chosen for logout (provider picker in logout mode).
    Logout(String),
    /// Navigate back one screen in the login flow (tau `_LoginFlowAction.BACK`).
    LoginBack,
    /// An API key was entered (api-key login modal).
    ApiKey(String),
    /// The user submitted a manual OAuth code / prompt response (OAuth modal).
    OAuthManualCode(String),
    /// A custom provider was fully described (custom-provider login modal).
    CustomProvider(CustomProviderDraft),
    /// An extension `ui.select` resolved (`None` on cancel).
    ExtensionSelect(Option<String>),
    /// An extension `ui.confirm` resolved.
    ExtensionConfirm(bool),
    /// An extension `ui.input` resolved (`None` on cancel).
    ExtensionInput(Option<String>),
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
    /// A deferred/informational notice.
    Notice(NoticeModal),
    /// Pick a login method (subscription / api-key / custom).
    LoginMethodPicker(LoginMethodPickerModal),
    /// Pick a provider to log in to or out of (searchable).
    LoginProviderPicker(LoginProviderPickerModal),
    /// Enter a provider API key.
    ApiKeyLogin(ApiKeyLoginModal),
    /// Drive an interactive OAuth login (browser / device flow).
    OAuthLogin(OAuthLoginModal),
    /// Describe a custom OpenAI-compatible provider.
    CustomProviderLogin(CustomProviderLoginModal),
    /// An extension `ui.select` picker.
    ExtensionSelect(ExtensionSelectModal),
    /// An extension `ui.confirm` dialog.
    ExtensionConfirm(ExtensionConfirmModal),
    /// An extension `ui.input` text prompt.
    ExtensionInput(ExtensionInputModal),
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
            Modal::LoginMethodPicker(m) => m.handle_key(key),
            Modal::LoginProviderPicker(m) => m.handle_key(key),
            Modal::ApiKeyLogin(m) => m.handle_key(key),
            Modal::OAuthLogin(m) => m.handle_key(key),
            Modal::CustomProviderLogin(m) => m.handle_key(key),
            Modal::ExtensionSelect(m) => m.handle_key(key),
            Modal::ExtensionConfirm(m) => m.handle_key(key),
            Modal::ExtensionInput(m) => m.handle_key(key),
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
            Modal::LoginMethodPicker(m) => m.render(frame, area, theme),
            Modal::LoginProviderPicker(m) => m.render(frame, area, theme),
            Modal::ApiKeyLogin(m) => m.render(frame, area, theme),
            Modal::OAuthLogin(m) => m.render(frame, area, theme),
            Modal::CustomProviderLogin(m) => m.render(frame, area, theme),
            Modal::ExtensionSelect(m) => m.render(frame, area, theme),
            Modal::ExtensionConfirm(m) => m.render(frame, area, theme),
            Modal::ExtensionInput(m) => m.render(frame, area, theme),
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
    let step = delta.unsigned_abs() as usize;
    let next = if delta < 0 {
        index.saturating_sub(step)
    } else {
        index.saturating_add(step)
    };
    next.min(max)
}

// --- session picker ---------------------------------------------------------

/// Pick an indexed session to resume (tau `SessionPickerScreen`).
///
/// Mirrors the model/provider pickers' live-search UX: typing filters the list
/// by session title or model name. Working-directory paths are deliberately
/// excluded from matching (tau `_filter_session_records`, tightened in tau
/// `08e2bfd`) so a query like `main` doesn't match every session under a
/// `.../main/...` path.
pub struct SessionPickerModal {
    records: Vec<CodingSessionRecord>,
    search: String,
    index: usize,
}

impl SessionPickerModal {
    /// Build from indexed session records.
    #[must_use]
    pub fn new(records: Vec<CodingSessionRecord>) -> Self {
        Self {
            records,
            search: String::new(),
            index: 0,
        }
    }

    fn label(record: &CodingSessionRecord) -> String {
        let title = record.title.as_deref().unwrap_or("Untitled");
        format!("{}  {}  {}", record.id, title, record.model)
    }

    /// Case-insensitive substring match on session title or model, ignoring the
    /// working directory (tau `_filter_session_records`).
    fn visible(&self) -> Vec<&CodingSessionRecord> {
        let query = self.search.trim().to_lowercase();
        self.records
            .iter()
            .filter(|record| {
                query.is_empty()
                    || record
                        .title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                    || record.model.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn clamp_index(&mut self) {
        let len = self.visible().len();
        self.index = if len == 0 {
            0
        } else {
            self.index.min(len - 1)
        };
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Up => {
                let len = self.visible().len();
                self.index = move_cursor(self.index, len, -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                let len = self.visible().len();
                self.index = move_cursor(self.index, len, 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match self.visible().get(self.index) {
                Some(record) => ModalOutcome::Session(record.id.clone()),
                None => ModalOutcome::Cancelled,
            },
            KeyCode::Backspace => {
                self.search.pop();
                self.clamp_index();
                ModalOutcome::Consumed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(c);
                self.clamp_index();
                ModalOutcome::Consumed
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 60, area);
        let mut rows: Vec<Line<'static>> = Vec::new();
        rows.push(Line::from(Span::styled(
            format!("search: {}", self.search),
            parse_style(&theme.muted_text),
        )));
        rows.push(Line::default());
        let visible = self.visible();
        if self.records.is_empty() {
            rows.push(Line::from(Span::styled(
                "  No sessions found".to_string(),
                parse_style(&theme.completion_description),
            )));
        } else if visible.is_empty() {
            rows.push(Line::from(Span::styled(
                "  No matching sessions".to_string(),
                parse_style(&theme.completion_description),
            )));
        } else {
            for (i, r) in visible.iter().enumerate() {
                rows.push(list_row(Self::label(r), i == self.index, theme));
            }
        }
        render_list_modal(
            frame,
            area,
            "Sessions",
            &rows,
            "Type to search · Enter selects · Escape closes",
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
        // tau opens BOTH kinds on the "all models" list (`self.mode = "all"`): the
        // scoped editor must show every model so unscoped ones can be ADDED, not
        // just removed. Only the model kind can Tab to the scoped-only view.
        let mode = ModelPickerMode::All;
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
    /// The last-rendered body viewport height, so `handle_key` can clamp the
    /// scroll offset to the final page instead of drifting into blank space
    /// (tau's `CommandOutputScreen` uses a scroll container that clamps).
    viewport_height: std::cell::Cell<u16>,
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
            viewport_height: std::cell::Cell::new(0),
        }
    }

    /// The largest scroll offset that still keeps content on screen: the line
    /// count minus the visible viewport height (0 when everything fits).
    fn max_scroll(&self) -> u16 {
        let lines = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        lines.saturating_sub(self.viewport_height.get())
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => ModalOutcome::Cancelled,
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll());
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
        self.viewport_height.set(chunks[0].height);
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

    // `&mut self` is required to match the uniform `Modal::handle_key` dispatch,
    // even though this notice modal ignores its own state.
    #[allow(clippy::unused_self)]
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

// --- shared single-line text field -----------------------------------------

/// A minimal single-line text field (tau's `Input`), optionally masked.
#[derive(Debug, Default, Clone)]
struct TextField {
    value: String,
    password: bool,
}

impl TextField {
    fn masked(password: bool) -> Self {
        Self {
            value: String::new(),
            password,
        }
    }

    /// Feed a key; returns `true` if it mutated the field.
    fn input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.push(c);
                true
            }
            KeyCode::Backspace => {
                self.value.pop();
                true
            }
            _ => false,
        }
    }

    fn display(&self) -> String {
        if self.password {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }
}

// --- login method picker ----------------------------------------------------

/// The three login methods, in tau's list order.
const LOGIN_METHODS: [(LoginMethod, &str); 3] = [
    (LoginMethod::Subscription, "Subscription — OAuth account"),
    (LoginMethod::ApiKey, "API key — built-in provider"),
    (LoginMethod::Custom, "Custom provider — OpenAI-compatible"),
];

/// Pick how to authenticate (tau `LoginMethodPickerScreen`). Arrow keys wrap.
pub struct LoginMethodPickerModal {
    index: usize,
}

impl Default for LoginMethodPickerModal {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginMethodPickerModal {
    /// Build the picker with the first (subscription) method selected.
    #[must_use]
    pub fn new() -> Self {
        Self { index: 0 }
    }

    fn wrap(&mut self, forward: bool) {
        let len = LOGIN_METHODS.len();
        self.index = if forward {
            (self.index + 1) % len
        } else {
            (self.index + len - 1) % len
        };
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Cancelled,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Up => {
                self.wrap(false);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.wrap(true);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => ModalOutcome::LoginMethod(LOGIN_METHODS[self.index].0),
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 40, area);
        let mut rows: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                "Choose how to authenticate.".to_string(),
                parse_style(&theme.completion_description),
            )),
            Line::default(),
        ];
        for (i, (_, label)) in LOGIN_METHODS.iter().enumerate() {
            rows.push(list_row((*label).to_string(), i == self.index, theme));
        }
        render_list_modal(
            frame,
            area,
            "Login",
            &rows,
            "Enter selects · Escape/Ctrl+D closes",
            theme,
        );
    }
}

// --- login provider picker --------------------------------------------------

/// A provider offered in the login/logout picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginProviderItem {
    /// Stable provider id.
    pub name: String,
    /// Human-facing display name.
    pub display_name: String,
}

impl LoginProviderItem {
    /// Build an item.
    #[must_use]
    pub fn new(name: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            display_name: display_name.into(),
        }
    }

    fn label(&self) -> String {
        format!("{} — {}", self.display_name, self.name)
    }
}

/// What the provider picker is being used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPickerPurpose {
    /// Choosing a provider to log in to (Escape navigates back).
    Login {
        /// The method that led here (carried into the outcome).
        method: LoginMethod,
    },
    /// Choosing a provider to log out of (Escape closes).
    Logout,
}

/// Searchable provider picker (tau `LoginProviderPickerScreen`).
pub struct LoginProviderPickerModal {
    providers: Vec<LoginProviderItem>,
    purpose: ProviderPickerPurpose,
    title: String,
    search: String,
    index: usize,
}

impl LoginProviderPickerModal {
    /// Build a provider picker.
    #[must_use]
    pub fn new(
        providers: Vec<LoginProviderItem>,
        purpose: ProviderPickerPurpose,
        title: impl Into<String>,
    ) -> Self {
        Self {
            providers,
            purpose,
            title: title.into(),
            search: String::new(),
            index: 0,
        }
    }

    fn visible(&self) -> Vec<&LoginProviderItem> {
        let query = self.search.trim().to_lowercase();
        self.providers
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.name.to_lowercase().contains(&query)
                    || item.display_name.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn cancel_outcome(&self) -> ModalOutcome {
        match self.purpose {
            ProviderPickerPurpose::Login { .. } => ModalOutcome::LoginBack,
            ProviderPickerPurpose::Logout => ModalOutcome::Cancelled,
        }
    }

    fn select_outcome(&self, name: String) -> ModalOutcome {
        match self.purpose {
            ProviderPickerPurpose::Login { method } => ModalOutcome::LoginProvider {
                name,
                method: Some(method),
            },
            ProviderPickerPurpose::Logout => ModalOutcome::Logout(name),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Esc => self.cancel_outcome(),
            KeyCode::Up => {
                self.index = move_cursor(self.index, self.visible().len(), -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.index = move_cursor(self.index, self.visible().len(), 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match self.visible().get(self.index) {
                Some(item) => {
                    let name = item.name.clone();
                    self.select_outcome(name)
                }
                None => ModalOutcome::Consumed,
            },
            KeyCode::Backspace => {
                self.search.pop();
                self.index = 0;
                ModalOutcome::Consumed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(c);
                self.index = 0;
                ModalOutcome::Consumed
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 60, area);
        let visible = self.visible();
        let mut rows: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                format!("search: {}", self.search),
                parse_style(&theme.muted_text),
            )),
            Line::default(),
        ];
        if visible.is_empty() {
            rows.push(Line::from(Span::styled(
                "  No matching providers".to_string(),
                parse_style(&theme.completion_description),
            )));
        } else {
            for (i, item) in visible.iter().enumerate() {
                rows.push(list_row(item.label(), i == self.index, theme));
            }
        }
        let help = if visible.is_empty() {
            "No matching providers · Escape closes"
        } else {
            "Enter selects · Escape closes"
        };
        render_list_modal(frame, area, &self.title, &rows, help, theme);
    }
}

// --- API-key login ----------------------------------------------------------

/// Prompt for a provider API key (tau `LoginScreen`).
pub struct ApiKeyLoginModal {
    display_name: String,
    field: TextField,
}

impl ApiKeyLoginModal {
    /// Build for a provider display name.
    #[must_use]
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            field: TextField::masked(true),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Esc => ModalOutcome::LoginBack,
            KeyCode::Enter => {
                let value = self.field.value.trim().to_string();
                if value.is_empty() {
                    ModalOutcome::Consumed
                } else {
                    ModalOutcome::ApiKey(value)
                }
            }
            _ => {
                self.field.input(key);
                ModalOutcome::Consumed
            }
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 30, area);
        let rows = vec![
            Line::from(Span::styled(
                "Paste this provider's API key.".to_string(),
                parse_style(&theme.completion_description),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("API key: ".to_string(), parse_style(&theme.muted_text)),
                Span::styled(self.field.display(), parse_style(&theme.prompt_text)),
            ]),
        ];
        render_list_modal(
            frame,
            area,
            &format!("Login: {}", self.display_name),
            &rows,
            "Enter saves · Escape goes back · Ctrl+D closes",
            theme,
        );
    }
}

// --- OAuth login ------------------------------------------------------------

/// Drive an interactive OAuth login (tau `OAuthLoginScreen`). The background
/// login task updates this modal through the setters; the user's manual-code
/// entry flows back out via [`ModalOutcome::OAuthManualCode`].
pub struct OAuthLoginModal {
    display_name: String,
    help: String,
    detail: Option<String>,
    field: TextField,
    prompt_allows_empty: bool,
    /// The `(id, label)` options of a provider selection prompt; empty unless the
    /// modal is in selection mode.
    select_options: Vec<(String, String)>,
    /// The highlighted option while in selection mode.
    select_index: usize,
}

impl OAuthLoginModal {
    /// Build for a provider display name.
    #[must_use]
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            help: "Follow the provider instructions to complete login.".to_string(),
            detail: None,
            field: TextField::masked(false),
            prompt_allows_empty: false,
            select_options: Vec::new(),
            select_index: 0,
        }
    }

    /// Show a browser authorization URL and optional instructions.
    pub fn set_auth(&mut self, url: String, instructions: Option<String>) {
        self.detail = Some(url);
        if let Some(instructions) = instructions {
            self.help = instructions;
        }
    }

    /// Show a device-code verification URL + user code.
    pub fn set_device_code(&mut self, verification_uri: String, user_code: &str) {
        self.detail = Some(verification_uri);
        self.help = format!("Open the URL and enter code: {user_code}");
    }

    /// Update the help/progress line.
    pub fn set_help(&mut self, message: String) {
        self.help = message;
    }

    /// Show a provider prompt message (and whether an empty response is allowed).
    pub fn set_prompt(&mut self, message: String, allow_empty: bool) {
        self.help = message;
        self.prompt_allows_empty = allow_empty;
    }

    /// Enter selection mode: show `options` (`(id, label)`) for the user to choose
    /// the account/organization to authenticate. The chosen id flows back out via
    /// [`ModalOutcome::OAuthManualCode`], reusing the code channel.
    pub fn set_select(&mut self, message: String, options: Vec<(String, String)>) {
        self.help = message;
        self.select_options = options;
        self.select_index = 0;
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        // Selection mode: arrow keys move the highlight, Enter returns the chosen id.
        if !self.select_options.is_empty() {
            return self.handle_select_key(key);
        }
        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Esc => ModalOutcome::LoginBack,
            KeyCode::Enter => {
                let value = self.field.value.trim().to_string();
                if value.is_empty() && !self.prompt_allows_empty {
                    return ModalOutcome::Consumed;
                }
                self.field.value.clear();
                let allowed_empty = self.prompt_allows_empty;
                self.prompt_allows_empty = false;
                if value.is_empty() && !allowed_empty {
                    ModalOutcome::Consumed
                } else {
                    ModalOutcome::OAuthManualCode(value)
                }
            }
            _ => {
                self.field.input(key);
                ModalOutcome::Consumed
            }
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Esc => ModalOutcome::LoginBack,
            KeyCode::Up => {
                if self.select_index == 0 {
                    self.select_index = self.select_options.len() - 1;
                } else {
                    self.select_index -= 1;
                }
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.select_index = (self.select_index + 1) % self.select_options.len();
                ModalOutcome::Consumed
            }
            KeyCode::Enter => {
                let id = self.select_options[self.select_index].0.clone();
                // Leave selection mode; the id travels the code channel like a
                // manual-code entry so the awaiting login flow resumes.
                self.select_options.clear();
                self.select_index = 0;
                ModalOutcome::OAuthManualCode(id)
            }
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 40, area);
        let mut rows = vec![Line::from(Span::styled(
            self.help.clone(),
            parse_style(&theme.completion_description),
        ))];
        if let Some(detail) = &self.detail {
            rows.push(Line::default());
            rows.push(Line::from(Span::styled(
                detail.clone(),
                parse_style(&theme.accent),
            )));
        }
        rows.push(Line::default());
        if self.select_options.is_empty() {
            // Neutral prompt caret (matching rho's composer/list `› ` style)
            // rather than a hardcoded "code:" label: the same input line serves
            // the domain prompt (GitHub Enterprise URL), the manual code-paste
            // prompts, and any other `set_prompt` help — the `help` line above
            // carries the real prompt, so the caret must not contradict it.
            rows.push(Line::from(vec![
                Span::styled("› ".to_string(), parse_style(&theme.muted_text)),
                Span::styled(self.field.display(), parse_style(&theme.prompt_text)),
            ]));
            render_list_modal(
                frame,
                area,
                &format!("Login: {}", self.display_name),
                &rows,
                "Enter submits · Escape goes back · Ctrl+D closes",
                theme,
            );
        } else {
            for (index, (_, label)) in self.select_options.iter().enumerate() {
                let (marker, style) = if index == self.select_index {
                    ("> ", parse_style(&theme.accent))
                } else {
                    ("  ", parse_style(&theme.prompt_text))
                };
                rows.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
            }
            render_list_modal(
                frame,
                area,
                &format!("Login: {}", self.display_name),
                &rows,
                "↑/↓ selects · Enter confirms · Escape goes back · Ctrl+D closes",
                theme,
            );
        }
    }
}

// --- custom provider login --------------------------------------------------

/// The ordered fields of the custom-provider form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomField {
    ProviderName,
    DisplayName,
    BaseUrl,
    Models,
    DefaultModel,
    ApiKeyEnv,
    ApiKey,
}

const CUSTOM_FIELDS: [(CustomField, &str); 7] = [
    (CustomField::ProviderName, "Provider id"),
    (CustomField::DisplayName, "Display name"),
    (CustomField::BaseUrl, "Base URL"),
    (CustomField::Models, "Models (comma-separated)"),
    (CustomField::DefaultModel, "Default model (blank = first)"),
    (CustomField::ApiKeyEnv, "API key env (blank = derived)"),
    (CustomField::ApiKey, "API key"),
];

/// Collect a custom OpenAI-compatible provider (tau `CustomProviderLoginScreen`).
pub struct CustomProviderLoginModal {
    fields: Vec<TextField>,
    index: usize,
    error: Option<String>,
}

impl Default for CustomProviderLoginModal {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomProviderLoginModal {
    /// Build an empty form.
    #[must_use]
    pub fn new() -> Self {
        let fields = CUSTOM_FIELDS
            .iter()
            .map(|(field, _)| TextField::masked(*field == CustomField::ApiKey))
            .collect();
        Self {
            fields,
            index: 0,
            error: None,
        }
    }

    fn value(&self, field: CustomField) -> String {
        let pos = CUSTOM_FIELDS
            .iter()
            .position(|(f, _)| *f == field)
            .unwrap_or(0);
        self.fields[pos].value.trim().to_string()
    }

    fn build(&self) -> Result<CustomProviderDraft, String> {
        let provider_name = self.value(CustomField::ProviderName);
        if provider_name.is_empty() {
            return Err("Provider id is required.".to_string());
        }
        let base_url = self.value(CustomField::BaseUrl);
        if base_url.is_empty() {
            return Err("Base URL is required.".to_string());
        }
        let api_key = self.value(CustomField::ApiKey);
        if api_key.is_empty() {
            return Err("API key is required.".to_string());
        }
        let models: Vec<String> = self
            .value(CustomField::Models)
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .collect();
        if models.is_empty() {
            return Err("At least one model is required.".to_string());
        }
        let default_model = {
            let explicit = self.value(CustomField::DefaultModel);
            if explicit.is_empty() {
                models[0].clone()
            } else if models.contains(&explicit) {
                explicit
            } else {
                return Err("Default model must be one of the listed models.".to_string());
            }
        };
        let display_name = {
            let explicit = self.value(CustomField::DisplayName);
            if explicit.is_empty() {
                provider_name.clone()
            } else {
                explicit
            }
        };
        let api_key_env = {
            let explicit = self.value(CustomField::ApiKeyEnv);
            if explicit.is_empty() {
                format!(
                    "{}_API_KEY",
                    provider_name.to_uppercase().replace(['-', ' '], "_")
                )
            } else {
                explicit
            }
        };
        Ok(CustomProviderDraft {
            provider_name,
            display_name,
            base_url,
            api_key_env,
            models,
            default_model,
            api_key,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ModalOutcome::Cancelled
            }
            KeyCode::Esc => ModalOutcome::LoginBack,
            KeyCode::Up => {
                self.index = self.index.saturating_sub(1);
                ModalOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Tab => {
                self.index = (self.index + 1).min(self.fields.len() - 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => {
                // Enter on any field but the last advances; on the last it submits.
                if self.index + 1 < self.fields.len() {
                    self.index += 1;
                    ModalOutcome::Consumed
                } else {
                    match self.build() {
                        Ok(draft) => ModalOutcome::CustomProvider(draft),
                        Err(message) => {
                            self.error = Some(message);
                            ModalOutcome::Consumed
                        }
                    }
                }
            }
            _ => {
                self.fields[self.index].input(key);
                ModalOutcome::Consumed
            }
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(70, 70, area);
        let mut rows: Vec<Line<'static>> = vec![Line::from(Span::styled(
            "Describe a custom OpenAI-compatible provider.".to_string(),
            parse_style(&theme.completion_description),
        ))];
        rows.push(Line::default());
        for (i, (_, label)) in CUSTOM_FIELDS.iter().enumerate() {
            let selected = i == self.index;
            let (prefix, style) = if selected {
                ("› ", parse_style(&theme.completion_selected))
            } else {
                ("  ", parse_style(&theme.prompt_text))
            };
            rows.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(format!("{label}: "), parse_style(&theme.muted_text)),
                Span::styled(self.fields[i].display(), style),
            ]));
        }
        if let Some(error) = &self.error {
            rows.push(Line::default());
            rows.push(Line::from(Span::styled(
                error.clone(),
                parse_style(&theme.completion_description),
            )));
        }
        render_list_modal(
            frame,
            area,
            "Custom provider",
            &rows,
            "Enter advances / submits · ↑↓ move · Escape back · Ctrl+D closes",
            theme,
        );
    }
}

// --- extension UI modals ----------------------------------------------------

/// An extension `context.ui.select` picker (tau `ExtensionUi.select`).
pub struct ExtensionSelectModal {
    title: String,
    options: Vec<String>,
    index: usize,
}

impl ExtensionSelectModal {
    /// Build from a title and option labels.
    #[must_use]
    pub fn new(title: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            options,
            index: 0,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::ExtensionSelect(None),
            KeyCode::Up => {
                self.index = move_cursor(self.index, self.options.len(), -1);
                ModalOutcome::Consumed
            }
            KeyCode::Down => {
                self.index = move_cursor(self.index, self.options.len(), 1);
                ModalOutcome::Consumed
            }
            KeyCode::Enter => match self.options.get(self.index) {
                Some(option) => ModalOutcome::ExtensionSelect(Some(option.clone())),
                None => ModalOutcome::ExtensionSelect(None),
            },
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 50, area);
        let rows: Vec<Line<'static>> = if self.options.is_empty() {
            vec![Line::from(Span::styled(
                "  No options".to_string(),
                parse_style(&theme.completion_description),
            ))]
        } else {
            self.options
                .iter()
                .enumerate()
                .map(|(i, option)| list_row(option.clone(), i == self.index, theme))
                .collect()
        };
        render_list_modal(
            frame,
            area,
            &self.title,
            &rows,
            "Enter selects · Escape cancels",
            theme,
        );
    }
}

/// An extension `context.ui.confirm` dialog (tau `ExtensionUi.confirm`).
pub struct ExtensionConfirmModal {
    title: String,
    message: String,
    yes: bool,
}

impl ExtensionConfirmModal {
    /// Build from a title and message; defaults to "No".
    #[must_use]
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            yes: false,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n' | 'N') => ModalOutcome::ExtensionConfirm(false),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.yes = !self.yes;
                ModalOutcome::Consumed
            }
            KeyCode::Char('y' | 'Y') => ModalOutcome::ExtensionConfirm(true),
            KeyCode::Enter => ModalOutcome::ExtensionConfirm(self.yes),
            _ => ModalOutcome::Consumed,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 30, area);
        let choice = if self.yes {
            "  [ Yes ]   No  "
        } else {
            "   Yes   [ No ]  "
        };
        let rows = vec![
            Line::from(Span::styled(
                self.message.clone(),
                parse_style(&theme.prompt_text),
            )),
            Line::default(),
            Line::from(Span::styled(
                choice.to_string(),
                parse_style(&theme.completion_selected),
            )),
        ];
        render_list_modal(
            frame,
            area,
            &self.title,
            &rows,
            "Y/N or ←→ then Enter · Escape cancels",
            theme,
        );
    }
}

/// An extension `context.ui.input` text prompt (tau `ExtensionUi.input`).
pub struct ExtensionInputModal {
    title: String,
    placeholder: String,
    field: TextField,
}

impl ExtensionInputModal {
    /// Build from a title and placeholder.
    #[must_use]
    pub fn new(title: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            placeholder: placeholder.into(),
            field: TextField::masked(false),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::ExtensionInput(None),
            KeyCode::Enter => ModalOutcome::ExtensionInput(Some(self.field.value.clone())),
            _ => {
                self.field.input(key);
                ModalOutcome::Consumed
            }
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &TuiTheme) {
        let area = centered_rect(60, 30, area);
        let shown = if self.field.value.is_empty() {
            Span::styled(self.placeholder.clone(), parse_style(&theme.muted_text))
        } else {
            Span::styled(self.field.display(), parse_style(&theme.prompt_text))
        };
        let rows = vec![
            Line::default(),
            Line::from(vec![
                Span::styled("> ".to_string(), parse_style(&theme.muted_text)),
                shown,
            ]),
        ];
        render_list_modal(
            frame,
            area,
            &self.title,
            &rows,
            "Enter submits · Escape cancels",
            theme,
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

    fn session_record(id: &str, cwd: &str, model: &str, title: Option<&str>) -> CodingSessionRecord {
        CodingSessionRecord {
            id: id.into(),
            path: format!("{cwd}/{id}.jsonl").into(),
            cwd: cwd.into(),
            model: model.into(),
            title: title.map(str::to_string),
            created_at: 0.0,
            updated_at: 0.0,
            provider_name: None,
        }
    }

    #[test]
    fn session_picker_filters_by_title_and_model() {
        let records = vec![
            session_record("a", "/x", "claude", Some("Refactor loop")),
            session_record("b", "/x", "gpt-4", Some("Fix parser")),
            session_record("c", "/x", "claude", None),
        ];
        let mut modal = SessionPickerModal::new(records);
        // Title match.
        for ch in "parser".chars() {
            modal.handle_key(key(KeyCode::Char(ch)));
        }
        let visible = modal.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "b");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Session("b".into())
        );
        // Model match.
        let records = vec![
            session_record("a", "/x", "claude", Some("Refactor loop")),
            session_record("b", "/x", "gpt-4", Some("Fix parser")),
        ];
        let mut modal = SessionPickerModal::new(records);
        for ch in "gpt".chars() {
            modal.handle_key(key(KeyCode::Char(ch)));
        }
        let visible = modal.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "b");
    }

    #[test]
    fn session_picker_search_excludes_workspace_paths() {
        // The query matches only the cwd path segment, not title or model, so no
        // records match (tau 08e2bfd: workspace paths excluded from matching).
        let records = vec![
            session_record("a", "/home/dev/main-repo", "claude", Some("Refactor")),
            session_record("b", "/home/dev/main-repo", "gpt-4", Some("Parser")),
        ];
        let mut modal = SessionPickerModal::new(records);
        for ch in "main-repo".chars() {
            modal.handle_key(key(KeyCode::Char(ch)));
        }
        assert!(modal.visible().is_empty());
    }

    #[test]
    fn theme_picker_preselects_current_and_returns_choice() {
        let mut modal = ThemePickerModal::new(TuiThemeName::HighContrast);
        // high-contrast is index 3 in BUILTIN_TUI_THEME_NAMES (["rho","tau-dark",
        // "tau-light","high-contrast"]).
        assert_eq!(modal.index, 3);
        modal.handle_key(key(KeyCode::Up));
        assert_eq!(modal.index, 2);
        match modal.handle_key(key(KeyCode::Enter)) {
            ModalOutcome::Theme(name) => assert_eq!(name.as_str(), BUILTIN_TUI_THEME_NAMES[2]),
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
    fn scoped_editor_shows_all_models_so_they_can_be_added() {
        // Regression: the scoped-model EDITOR must open on the full model list so
        // an unscoped model can be added (not just removed), and Tab must NOT
        // switch it to the scoped-only view.
        let mut modal = ModelPickerModal::new(
            vec![choice("a"), choice("b"), choice("c")],
            vec![choice("b")],
            "a".into(),
            "anthropic".into(),
            ModelPickerKind::Scoped,
        );
        assert_eq!(modal.mode, ModelPickerMode::All);
        assert_eq!(modal.visible().len(), 3);
        modal.handle_key(key(KeyCode::Tab)); // no-op for the scoped kind
        assert_eq!(modal.mode, ModelPickerMode::All);
        // Enter on an unscoped model toggles it INTO the scoped set (stays open).
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ScopedToggle(choice("a"))
        );
        assert!(modal.scoped.contains(&choice("a")));
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
    fn command_output_scroll_clamps_to_last_page() {
        // Over-scrolling must not drift past the content into blank space: with a
        // 2-row viewport over 5 lines, the max offset is 3 (lines - viewport).
        let mut modal = CommandOutputModal::new("Output", "l1\nl2\nl3\nl4\nl5");
        modal.viewport_height.set(2);
        for _ in 0..50 {
            modal.handle_key(key(KeyCode::Down));
        }
        assert_eq!(modal.scroll, 3, "scroll clamps to lines - viewport");
        // And scrolling back up still works from the clamped position.
        modal.handle_key(key(KeyCode::Up));
        assert_eq!(modal.scroll, 2);
    }

    #[test]
    fn notice_dismisses() {
        let mut modal = NoticeModal::m7("Login");
        assert!(modal.message.contains("M7"));
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Cancelled);
    }

    // --- login modal tests --------------------------------------------------

    fn typed(modal: &mut impl FnMut(KeyEvent) -> ModalOutcome, text: &str) {
        for c in text.chars() {
            modal(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn login_method_picker_wraps_and_selects() {
        let mut modal = LoginMethodPickerModal::new();
        // Down twice lands on Custom; Up from the top wraps to Custom too.
        modal.handle_key(key(KeyCode::Down));
        modal.handle_key(key(KeyCode::Down));
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::LoginMethod(LoginMethod::Custom)
        );
        let mut modal = LoginMethodPickerModal::new();
        assert_eq!(
            modal.handle_key(key(KeyCode::Up)),
            ModalOutcome::Consumed // wrapped to the last entry
        );
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::LoginMethod(LoginMethod::Custom)
        );
        // Escape and Ctrl+D both close.
        assert_eq!(
            LoginMethodPickerModal::new().handle_key(key(KeyCode::Esc)),
            ModalOutcome::Cancelled
        );
        assert_eq!(
            LoginMethodPickerModal::new().handle_key(ctrl(KeyCode::Char('d'))),
            ModalOutcome::Cancelled
        );
    }

    #[test]
    fn login_provider_picker_filters_and_reports_method() {
        let providers = vec![
            LoginProviderItem::new("anthropic", "Anthropic"),
            LoginProviderItem::new("openai-codex", "OpenAI Codex"),
        ];
        let mut modal = LoginProviderPickerModal::new(
            providers,
            ProviderPickerPurpose::Login {
                method: LoginMethod::Subscription,
            },
            "Login",
        );
        // Search narrows to the openai entry.
        typed(&mut |k| modal.handle_key(k), "codex");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::LoginProvider {
                name: "openai-codex".to_string(),
                method: Some(LoginMethod::Subscription),
            }
        );
        // Escape navigates back in login mode.
        let mut modal = LoginProviderPickerModal::new(
            vec![LoginProviderItem::new("anthropic", "Anthropic")],
            ProviderPickerPurpose::Login {
                method: LoginMethod::ApiKey,
            },
            "Login",
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::LoginBack);
    }

    #[test]
    fn logout_provider_picker_reports_logout_and_closes() {
        let mut modal = LoginProviderPickerModal::new(
            vec![LoginProviderItem::new("openai", "OpenAI")],
            ProviderPickerPurpose::Logout,
            "Logout",
        );
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Logout("openai".to_string())
        );
        // Escape closes (no back) in logout mode.
        let mut modal = LoginProviderPickerModal::new(
            vec![LoginProviderItem::new("openai", "OpenAI")],
            ProviderPickerPurpose::Logout,
            "Logout",
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::Cancelled);
    }

    #[test]
    fn api_key_login_collects_key() {
        let mut modal = ApiKeyLoginModal::new("OpenAI");
        // Empty submit is ignored.
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Consumed
        );
        typed(&mut |k| modal.handle_key(k), "sk-123");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ApiKey("sk-123".to_string())
        );
        assert_eq!(
            ApiKeyLoginModal::new("OpenAI").handle_key(key(KeyCode::Esc)),
            ModalOutcome::LoginBack
        );
    }

    #[test]
    fn oauth_login_submits_manual_code_and_navigates() {
        let mut modal = OAuthLoginModal::new("Anthropic");
        typed(&mut |k| modal.handle_key(k), "abc123");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::OAuthManualCode("abc123".to_string())
        );
        // After submit the field is cleared, so a bare Enter is ignored.
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Consumed
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::LoginBack);
        assert_eq!(
            OAuthLoginModal::new("Anthropic").handle_key(ctrl(KeyCode::Char('d'))),
            ModalOutcome::Cancelled
        );
    }

    #[test]
    fn oauth_login_prompt_allows_empty_response() {
        let mut modal = OAuthLoginModal::new("GitHub Copilot");
        modal.set_prompt("Enterprise domain?".to_string(), true);
        // Empty Enter is accepted because the prompt allows it.
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::OAuthManualCode(String::new())
        );
    }

    #[test]
    fn oauth_login_select_returns_highlighted_option_id() {
        let mut modal = OAuthLoginModal::new("Anthropic");
        modal.set_select(
            "Choose an organization".to_string(),
            vec![
                ("org-a".to_string(), "Org A".to_string()),
                ("org-b".to_string(), "Org B".to_string()),
                ("org-c".to_string(), "Org C".to_string()),
            ],
        );
        // Move down twice (to Org C), wrap up once (back to Org B).
        assert_eq!(modal.handle_key(key(KeyCode::Down)), ModalOutcome::Consumed);
        assert_eq!(modal.handle_key(key(KeyCode::Down)), ModalOutcome::Consumed);
        assert_eq!(modal.handle_key(key(KeyCode::Up)), ModalOutcome::Consumed);
        // Enter returns the highlighted option's id (not its label).
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::OAuthManualCode("org-b".to_string())
        );
        // Selection mode ends; a stray Enter is now treated as the text prompt.
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Consumed
        );
    }

    #[test]
    fn oauth_login_select_escape_navigates_back() {
        let mut modal = OAuthLoginModal::new("Anthropic");
        modal.set_select(
            "Choose".to_string(),
            vec![
                ("a".to_string(), "A".to_string()),
                ("b".to_string(), "B".to_string()),
            ],
        );
        assert_eq!(modal.handle_key(key(KeyCode::Esc)), ModalOutcome::LoginBack);
    }

    #[test]
    fn custom_provider_builds_draft_with_defaults() {
        let mut modal = CustomProviderLoginModal::new();
        // Fill provider id, skip display, base url, models, skip default/env, key.
        typed(&mut |k| modal.handle_key(k), "acme");
        modal.handle_key(key(KeyCode::Down)); // display name (blank)
        modal.handle_key(key(KeyCode::Down)); // base url
        typed(&mut |k| modal.handle_key(k), "https://api.acme.ai/v1");
        modal.handle_key(key(KeyCode::Down)); // models
        typed(&mut |k| modal.handle_key(k), "acme-1, acme-2");
        modal.handle_key(key(KeyCode::Down)); // default model (blank -> first)
        modal.handle_key(key(KeyCode::Down)); // api key env (blank -> derived)
        modal.handle_key(key(KeyCode::Down)); // api key
        typed(&mut |k| modal.handle_key(k), "sk-acme");
        match modal.handle_key(key(KeyCode::Enter)) {
            ModalOutcome::CustomProvider(draft) => {
                assert_eq!(draft.provider_name, "acme");
                assert_eq!(draft.display_name, "acme");
                assert_eq!(draft.base_url, "https://api.acme.ai/v1");
                assert_eq!(
                    draft.models,
                    vec!["acme-1".to_string(), "acme-2".to_string()]
                );
                assert_eq!(draft.default_model, "acme-1");
                assert_eq!(draft.api_key_env, "ACME_API_KEY");
                assert_eq!(draft.api_key, "sk-acme");
            }
            other => panic!("expected custom provider, got {other:?}"),
        }
    }

    #[test]
    fn custom_provider_requires_fields() {
        let mut modal = CustomProviderLoginModal::new();
        // Jump to the last field and submit with everything blank.
        for _ in 0..CUSTOM_FIELDS.len() {
            modal.handle_key(key(KeyCode::Down));
        }
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::Consumed
        );
        assert!(modal.error.is_some());
    }

    // --- extension modal tests ----------------------------------------------

    #[test]
    fn extension_select_navigates_and_cancels() {
        let mut modal = ExtensionSelectModal::new("Pick", vec!["a".to_string(), "b".to_string()]);
        modal.handle_key(key(KeyCode::Down));
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ExtensionSelect(Some("b".to_string()))
        );
        assert_eq!(
            ExtensionSelectModal::new("Pick", vec!["a".to_string()]).handle_key(key(KeyCode::Esc)),
            ModalOutcome::ExtensionSelect(None)
        );
    }

    #[test]
    fn extension_confirm_yes_no_and_toggle() {
        let mut modal = ExtensionConfirmModal::new("Sure?", "body");
        // Default is No; Enter reports false.
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ExtensionConfirm(false)
        );
        // Toggle to Yes then Enter.
        modal.handle_key(key(KeyCode::Right));
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ExtensionConfirm(true)
        );
        // Direct Y/N shortcuts.
        assert_eq!(
            ExtensionConfirmModal::new("t", "b").handle_key(key(KeyCode::Char('y'))),
            ModalOutcome::ExtensionConfirm(true)
        );
        assert_eq!(
            ExtensionConfirmModal::new("t", "b").handle_key(key(KeyCode::Esc)),
            ModalOutcome::ExtensionConfirm(false)
        );
    }

    #[test]
    fn extension_input_collects_and_cancels() {
        let mut modal = ExtensionInputModal::new("Name", "hint");
        typed(&mut |k| modal.handle_key(k), "hello");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter)),
            ModalOutcome::ExtensionInput(Some("hello".to_string()))
        );
        assert_eq!(
            ExtensionInputModal::new("Name", "hint").handle_key(key(KeyCode::Esc)),
            ModalOutcome::ExtensionInput(None)
        );
    }
}
