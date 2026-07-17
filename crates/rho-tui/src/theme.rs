//! Durable TUI configuration: themes, keybindings, settings (port of tau
//! `tau_coding/tui/config.py`).
//!
//! Theme *names* stay `tau-dark` / `tau-light` / `high-contrast` — they are the
//! durable `tui.json` values and the `/theme` command vocabulary, which
//! `rho-coding` already commits to (`BUILTIN_TUI_THEME_NAMES`). Colors are kept
//! as tau's exact hex strings; the render layer parses them into ratatui styles,
//! so the palette matches tau per theme.

use std::path::{Path, PathBuf};

use rho_coding::paths::RhoPaths;
use serde_json::{Map, Value};

/// Raised when TUI configuration is invalid (tau `TuiConfigError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TuiConfigError(pub String);

/// The built-in theme names, in order (mirrors `rho_coding::BUILTIN_TUI_THEME_NAMES`).
pub const BUILTIN_TUI_THEME_NAMES: [&str; 3] = ["tau-dark", "tau-light", "high-contrast"];

/// A built-in theme name (tau `TuiThemeName`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiThemeName {
    /// The default dark theme.
    TauDark,
    /// The light theme.
    TauLight,
    /// The high-contrast theme.
    HighContrast,
}

impl TuiThemeName {
    /// The durable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TauDark => "tau-dark",
            Self::TauLight => "tau-light",
            Self::HighContrast => "high-contrast",
        }
    }

    /// Parse a theme name, or `None` if unknown.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "tau-dark" => Some(Self::TauDark),
            "tau-light" => Some(Self::TauLight),
            "high-contrast" => Some(Self::HighContrast),
            _ => None,
        }
    }
}

impl Default for TuiThemeName {
    fn default() -> Self {
        Self::TauDark
    }
}

/// The 13 configurable keybinding action names, in tau's `to_json` order.
pub const KEYBINDING_FIELDS: [&str; 13] = [
    "cancel",
    "command_palette",
    "session_picker",
    "queue_follow_up",
    "accept_completion",
    "completion_next",
    "completion_previous",
    "thinking_cycle",
    "model_cycle",
    "toggle_thinking",
    "toggle_tool_results",
    "copy_message",
    "quit",
];

const LEGACY_KEYBINDING_FIELDS: [&str; 2] = ["message_previous", "message_next"];

/// Configurable keys for rho's built-in frontend (tau `TuiKeybindings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiKeybindings {
    /// Cancel / dismiss.
    pub cancel: String,
    /// Open the command palette.
    pub command_palette: String,
    /// Open the session picker.
    pub session_picker: String,
    /// Queue a follow-up message.
    pub queue_follow_up: String,
    /// Accept the current completion.
    pub accept_completion: String,
    /// Move to the next completion.
    pub completion_next: String,
    /// Move to the previous completion.
    pub completion_previous: String,
    /// Cycle the thinking level.
    pub thinking_cycle: String,
    /// Cycle the model.
    pub model_cycle: String,
    /// Toggle thinking display.
    pub toggle_thinking: String,
    /// Toggle tool-result expansion.
    pub toggle_tool_results: String,
    /// Copy the focused message.
    pub copy_message: String,
    /// Quit the app.
    pub quit: String,
}

impl Default for TuiKeybindings {
    fn default() -> Self {
        Self {
            cancel: "escape".into(),
            command_palette: "ctrl+k".into(),
            session_picker: "ctrl+r".into(),
            queue_follow_up: "alt+enter".into(),
            accept_completion: "tab".into(),
            completion_next: "down".into(),
            completion_previous: "up".into(),
            thinking_cycle: "shift+tab".into(),
            model_cycle: "ctrl+p".into(),
            toggle_thinking: "ctrl+t".into(),
            toggle_tool_results: "ctrl+o".into(),
            copy_message: "ctrl+c".into(),
            quit: "ctrl+d".into(),
        }
    }
}

impl TuiKeybindings {
    fn get(&self, field: &str) -> &str {
        match field {
            "cancel" => &self.cancel,
            "command_palette" => &self.command_palette,
            "session_picker" => &self.session_picker,
            "queue_follow_up" => &self.queue_follow_up,
            "accept_completion" => &self.accept_completion,
            "completion_next" => &self.completion_next,
            "completion_previous" => &self.completion_previous,
            "thinking_cycle" => &self.thinking_cycle,
            "model_cycle" => &self.model_cycle,
            "toggle_thinking" => &self.toggle_thinking,
            "toggle_tool_results" => &self.toggle_tool_results,
            "copy_message" => &self.copy_message,
            "quit" => &self.quit,
            _ => "",
        }
    }

    fn set(&mut self, field: &str, value: String) {
        match field {
            "cancel" => self.cancel = value,
            "command_palette" => self.command_palette = value,
            "session_picker" => self.session_picker = value,
            "queue_follow_up" => self.queue_follow_up = value,
            "accept_completion" => self.accept_completion = value,
            "completion_next" => self.completion_next = value,
            "completion_previous" => self.completion_previous = value,
            "thinking_cycle" => self.thinking_cycle = value,
            "model_cycle" => self.model_cycle = value,
            "toggle_thinking" => self.toggle_thinking = value,
            "toggle_tool_results" => self.toggle_tool_results = value,
            "copy_message" => self.copy_message = value,
            "quit" => self.quit = value,
            _ => {}
        }
    }

    /// Serialize to a JSON object in tau's field order (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        for field in KEYBINDING_FIELDS {
            map.insert(field.to_string(), Value::String(self.get(field).to_string()));
        }
        map
    }
}

/// Colors for one transcript role block (tau `TuiRoleStyle`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiRoleStyle {
    /// The block border color.
    pub border: String,
    /// The block body style (`fg` or `fg on bg`).
    pub body: String,
}

impl TuiRoleStyle {
    fn new(border: &str, body: &str) -> Self {
        Self {
            border: border.to_string(),
            body: body.to_string(),
        }
    }
}

/// A resolved visual theme (tau `TuiTheme`). Fields mirror tau 1:1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTheme {
    /// The theme name.
    pub name: TuiThemeName,
    /// Screen background.
    pub screen_background: String,
    /// Screen foreground.
    pub screen_text: String,
    /// Chrome (bars) background.
    pub chrome_background: String,
    /// Chrome text.
    pub chrome_text: String,
    /// Muted/secondary text.
    pub muted_text: String,
    /// Sidebar background.
    pub sidebar_background: String,
    /// Default border.
    pub border: String,
    /// Transcript background.
    pub transcript_background: String,
    /// Prompt background.
    pub prompt_background: String,
    /// Prompt text.
    pub prompt_text: String,
    /// Prompt border.
    pub prompt_border: String,
    /// Autocomplete background.
    pub autocomplete_background: String,
    /// Accent color.
    pub accent: String,
    /// Highlight background.
    pub highlight_background: String,
    /// Highlight text.
    pub highlight_text: String,
    /// Markdown heading color.
    pub markdown_heading: String,
    /// Markdown table header color.
    pub markdown_table_header: String,
    /// Markdown table border color.
    pub markdown_table_border: String,
    /// Markdown inline code color.
    pub markdown_inline_code: String,
    /// Markdown code-block background.
    pub markdown_code_block_background: String,
    /// Markdown link color.
    pub markdown_link: String,
    /// Markdown bullet color.
    pub markdown_bullet: String,
    /// Selected completion style.
    pub completion_selected: String,
    /// Selected completion description style.
    pub completion_selected_description: String,
    /// Completion description style.
    pub completion_description: String,
    /// Syntax highlighting theme name.
    pub syntax_theme: String,
    /// Per-role border/body styles, in tau's declared order.
    pub role_styles: Vec<(&'static str, TuiRoleStyle)>,
}

impl TuiTheme {
    /// The role style for a transcript role, or `None`.
    #[must_use]
    pub fn role_style(&self, role: &str) -> Option<&TuiRoleStyle> {
        self.role_styles
            .iter()
            .find(|(name, _)| *name == role)
            .map(|(_, style)| style)
    }
}

fn role_styles(entries: &[(&'static str, &str, &str)]) -> Vec<(&'static str, TuiRoleStyle)> {
    entries
        .iter()
        .map(|(role, border, body)| (*role, TuiRoleStyle::new(border, body)))
        .collect()
}

/// The `tau-dark` theme.
#[must_use]
pub fn tau_dark_theme() -> TuiTheme {
    TuiTheme {
        name: TuiThemeName::TauDark,
        screen_background: "#000000".into(),
        screen_text: "#d8dee9".into(),
        chrome_background: "#000000".into(),
        chrome_text: "#d8dee9".into(),
        muted_text: "#667085".into(),
        sidebar_background: "#000000".into(),
        border: "#141922".into(),
        transcript_background: "#000000".into(),
        prompt_background: "#101419".into(),
        prompt_text: "#e5e7eb".into(),
        prompt_border: "#2d3748".into(),
        autocomplete_background: "#000000".into(),
        accent: "#db945a".into(),
        highlight_background: "#a7f3f0".into(),
        highlight_text: "#061a1a".into(),
        markdown_heading: "#db945a".into(),
        markdown_table_header: "#7b7b7b".into(),
        markdown_table_border: "#7b7b7b".into(),
        markdown_inline_code: "#759e95".into(),
        markdown_code_block_background: "#161b21".into(),
        markdown_link: "#93c5fd".into(),
        markdown_bullet: "#db945a".into(),
        completion_selected: "bold #061a1a on #a7f3f0".into(),
        completion_selected_description: "#123333 on #a7f3f0".into(),
        completion_description: "#667085".into(),
        syntax_theme: "ansi_dark".into(),
        role_styles: role_styles(&[
            ("user", "#7c8ea6", "#d8dee9 on #000000"),
            ("assistant", "#6ea6a0", "#d8dee9 on #000000"),
            ("tool", "#8a7a52", "#cbd5e1 on #000000"),
            ("error", "#ff4f4f", "#ffb4b4 on #000000"),
            ("status", "#526070", "#aab4c2 on #000000"),
            ("thinking", "#4b5563", "#9ca3af on #000000"),
            ("skill", "#b48ead", "#e5d4ef on #000000"),
            ("custom", "#6ea6a0", "#d8dee9 on #000000"),
            ("branch_summary", "#c084fc", "#e9d5ff on #000000"),
            ("compaction_summary", "#c084fc", "#e9d5ff on #000000"),
        ]),
    }
}

/// The `high-contrast` theme.
#[must_use]
pub fn high_contrast_theme() -> TuiTheme {
    TuiTheme {
        name: TuiThemeName::HighContrast,
        screen_background: "#000000".into(),
        screen_text: "#ffffff".into(),
        chrome_background: "#111111".into(),
        chrome_text: "#ffffff".into(),
        muted_text: "#d0d0d0".into(),
        sidebar_background: "#111111".into(),
        border: "#888888".into(),
        transcript_background: "#000000".into(),
        prompt_background: "#1a1a1a".into(),
        prompt_text: "#ffffff".into(),
        prompt_border: "#00ff66".into(),
        autocomplete_background: "#111111".into(),
        accent: "#ffb454".into(),
        highlight_background: "#7fffd4".into(),
        highlight_text: "#000000".into(),
        markdown_heading: "#ffb454".into(),
        markdown_table_header: "#d0d0d0".into(),
        markdown_table_border: "#d0d0d0".into(),
        markdown_inline_code: "#7fffd4".into(),
        markdown_code_block_background: "#161b21".into(),
        markdown_link: "#80d8ff".into(),
        markdown_bullet: "#ffb454".into(),
        completion_selected: "bold black on #7fffd4".into(),
        completion_selected_description: "black on #7fffd4".into(),
        completion_description: "white".into(),
        syntax_theme: "ansi_dark".into(),
        role_styles: role_styles(&[
            ("user", "#00b7ff", "white on #001626"),
            ("assistant", "#00ff66", "white on #001a0b"),
            ("tool", "#ffd000", "white on #211900"),
            ("error", "#ff4f4f", "white on #260000"),
            ("status", "#ffffff", "white on #111111"),
            ("thinking", "#00b7ff", "white on #001626"),
            ("skill", "#ff8cff", "white on #260026"),
            ("custom", "#00ffcc", "white on #001a17"),
            ("branch_summary", "#d8b4fe", "white on #260026"),
            ("compaction_summary", "#d8b4fe", "white on #260026"),
        ]),
    }
}

/// The `tau-light` theme.
#[must_use]
pub fn tau_light_theme() -> TuiTheme {
    TuiTheme {
        name: TuiThemeName::TauLight,
        screen_background: "#ffffff".into(),
        screen_text: "#111827".into(),
        chrome_background: "#f3f4f6".into(),
        chrome_text: "#111827".into(),
        muted_text: "#475569".into(),
        sidebar_background: "#f8fafc".into(),
        border: "#cbd5e1".into(),
        transcript_background: "#ffffff".into(),
        prompt_background: "#f8fafc".into(),
        prompt_text: "#111827".into(),
        prompt_border: "#2563eb".into(),
        autocomplete_background: "#ffffff".into(),
        accent: "#0f766e".into(),
        highlight_background: "#dbeafe".into(),
        highlight_text: "#1d4ed8".into(),
        markdown_heading: "#b45309".into(),
        markdown_table_header: "#64748b".into(),
        markdown_table_border: "#cbd5e1".into(),
        markdown_inline_code: "#0f766e".into(),
        markdown_code_block_background: "#f1f5f9".into(),
        markdown_link: "#2563eb".into(),
        markdown_bullet: "#b45309".into(),
        completion_selected: "bold #0f172a on #dbeafe".into(),
        completion_selected_description: "#334155 on #dbeafe".into(),
        completion_description: "#667085".into(),
        syntax_theme: "ansi_light".into(),
        role_styles: role_styles(&[
            ("user", "#2563eb", "#111827"),
            ("assistant", "#0f766e", "#111827"),
            ("tool", "#a16207", "#1f2937"),
            ("error", "#b91c1c", "#7f1d1d"),
            ("status", "#64748b", "#334155"),
            ("thinking", "#6b7280", "#4b5563"),
            ("skill", "#7c3aed", "#4c1d95"),
            ("custom", "#0f766e", "#111827"),
            ("branch_summary", "#9333ea", "#581c87"),
            ("compaction_summary", "#9333ea", "#581c87"),
        ]),
    }
}

/// Return a built-in theme by name (tau `get_tui_theme`).
#[must_use]
pub fn get_tui_theme(name: TuiThemeName) -> TuiTheme {
    match name {
        TuiThemeName::TauDark => tau_dark_theme(),
        TuiThemeName::TauLight => tau_light_theme(),
        TuiThemeName::HighContrast => high_contrast_theme(),
    }
}

/// A sidebar position (tau `Literal["left", "right", "off"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarPosition {
    /// Sidebar on the left.
    #[default]
    Left,
    /// Sidebar on the right.
    Right,
    /// No sidebar.
    Off,
}

impl SidebarPosition {
    /// The durable string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Off => "off",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// TUI settings loaded from rho home (tau `TuiSettings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiSettings {
    /// Configurable keybindings.
    pub keybindings: TuiKeybindings,
    /// The selected theme.
    pub theme: TuiThemeName,
    /// Whether to auto-copy the selection.
    pub auto_copy_selection: bool,
    /// The sidebar position.
    pub sidebar_position: SidebarPosition,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            keybindings: TuiKeybindings::default(),
            theme: TuiThemeName::TauDark,
            auto_copy_selection: false,
            sidebar_position: SidebarPosition::Left,
        }
    }
}

impl TuiSettings {
    /// The resolved built-in theme (tau `resolved_theme`).
    #[must_use]
    pub fn resolved_theme(&self) -> TuiTheme {
        get_tui_theme(self.theme)
    }

    /// Serialize to JSON in tau's field order (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("auto_copy_selection".into(), Value::Bool(self.auto_copy_selection));
        map.insert("keybindings".into(), Value::Object(self.keybindings.to_json()));
        map.insert(
            "sidebar_position".into(),
            Value::String(self.sidebar_position.as_str().to_string()),
        );
        map.insert("theme".into(), Value::String(self.theme.as_str().to_string()));
        map
    }
}

/// The durable TUI settings path (tau `tui_settings_path`).
#[must_use]
pub fn tui_settings_path(paths: &RhoPaths) -> PathBuf {
    paths.home.join("tui.json")
}

/// Load durable TUI settings, falling back to defaults (tau `load_tui_settings`).
pub fn load_tui_settings(paths: &RhoPaths) -> Result<TuiSettings, TuiConfigError> {
    let path = tui_settings_path(paths);
    if !path.exists() {
        return Ok(TuiSettings::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| TuiConfigError(format!("Failed to read TUI settings: {err}")))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|err| TuiConfigError(format!("TUI settings must be valid JSON: {err}")))?;
    let Value::Object(map) = raw else {
        return Err(TuiConfigError("TUI settings must be a JSON object".into()));
    };
    tui_settings_from_json(&map)
}

/// Persist durable TUI settings and return the written path (tau `save_tui_settings`).
pub fn save_tui_settings(settings: &TuiSettings, paths: &RhoPaths) -> Result<PathBuf, TuiConfigError> {
    let path = tui_settings_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| TuiConfigError(format!("Failed to create TUI home: {err}")))?;
    }
    let body = serde_json::to_string_pretty(&Value::Object(settings.to_json()))
        .map_err(|err| TuiConfigError(format!("Failed to serialize TUI settings: {err}")))?;
    std::fs::write(&path, format!("{body}\n"))
        .map_err(|err| TuiConfigError(format!("Failed to write TUI settings: {err}")))?;
    Ok(path)
}

/// Parse TUI settings from JSON-compatible data (tau `tui_settings_from_json`).
pub fn tui_settings_from_json(data: &Map<String, Value>) -> Result<TuiSettings, TuiConfigError> {
    let allowed = ["auto_copy_selection", "keybindings", "sidebar_position", "theme"];
    if let Some(field) = first_unknown(data.keys(), &allowed, &[]) {
        return Err(TuiConfigError(format!("Unknown TUI settings field: {field}")));
    }

    let keybindings_data = match data.get("keybindings") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(TuiConfigError("TUI keybindings must be a JSON object".into())),
    };

    let raw_sidebar = data
        .get("sidebar_position")
        .cloned()
        .unwrap_or_else(|| Value::String("left".into()));
    let sidebar_position = raw_sidebar
        .as_str()
        .and_then(SidebarPosition::parse)
        .ok_or_else(|| {
            TuiConfigError("sidebar_position must be 'left', 'right', or 'off'".into())
        })?;

    let theme = theme_name(data.get("theme"))?;
    let auto_copy_selection = bool_setting(data.get("auto_copy_selection"), "auto_copy_selection")?;

    Ok(TuiSettings {
        keybindings: keybindings_from_json(&keybindings_data)?,
        theme,
        auto_copy_selection,
        sidebar_position,
    })
}

fn bool_setting(value: Option<&Value>, field: &str) -> Result<bool, TuiConfigError> {
    match value {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(TuiConfigError(format!("TUI setting must be a boolean: {field}"))),
    }
}

fn keybindings_from_json(data: &Map<String, Value>) -> Result<TuiKeybindings, TuiConfigError> {
    if let Some(field) = first_unknown(data.keys(), &KEYBINDING_FIELDS, &LEGACY_KEYBINDING_FIELDS) {
        return Err(TuiConfigError(format!("Unknown TUI keybinding: {field}")));
    }
    let mut keybindings = TuiKeybindings::default();
    for field in KEYBINDING_FIELDS {
        if let Some(value) = data.get(field) {
            keybindings.set(field, key_string(value, field)?);
        }
    }
    reject_duplicate_keys(&keybindings)?;
    Ok(keybindings)
}

fn key_string(value: &Value, field: &str) -> Result<String, TuiConfigError> {
    match value.as_str() {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(TuiConfigError(format!(
            "TUI keybinding must be a non-empty string: {field}"
        ))),
    }
}

fn theme_name(value: Option<&Value>) -> Result<TuiThemeName, TuiConfigError> {
    let raw = match value {
        None => return Ok(TuiThemeName::TauDark),
        Some(v) => v,
    };
    let name = match raw.as_str() {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return Err(TuiConfigError("TUI theme must be a non-empty string".into())),
    };
    TuiThemeName::parse(name).ok_or_else(|| TuiConfigError(format!("Unknown TUI theme: {name}")))
}

fn reject_duplicate_keys(keybindings: &TuiKeybindings) -> Result<(), TuiConfigError> {
    let mut key_to_action: Vec<(String, &'static str)> = Vec::new();
    for action in KEYBINDING_FIELDS {
        let key = keybindings.get(action);
        if let Some((_, previous)) = key_to_action.iter().find(|(k, _)| k == key) {
            return Err(TuiConfigError(format!(
                "TUI keybinding '{key}' is assigned to both '{previous}' and '{action}'"
            )));
        }
        key_to_action.push((key.to_string(), action));
    }
    Ok(())
}

/// The first (sorted) key not in `allowed` or `legacy`, if any.
fn first_unknown<'a>(
    keys: impl Iterator<Item = &'a String>,
    allowed: &[&str],
    legacy: &[&str],
) -> Option<String> {
    let mut unknown: Vec<&str> = keys
        .filter(|k| !allowed.contains(&k.as_str()) && !legacy.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    unknown.sort_unstable();
    unknown.first().map(|s| (*s).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(json: Value) -> Map<String, Value> {
        match json {
            Value::Object(map) => map,
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn theme_names_match_rho_coding() {
        assert_eq!(BUILTIN_TUI_THEME_NAMES, rho_coding::BUILTIN_TUI_THEME_NAMES);
    }

    #[test]
    fn defaults_match_tau() {
        let settings = TuiSettings::default();
        assert_eq!(settings.keybindings.quit, "ctrl+d");
        assert_eq!(settings.sidebar_position, SidebarPosition::Left);
    }

    #[test]
    fn reads_keybindings_and_theme() {
        let settings = tui_settings_from_json(&obj(serde_json::json!({
            "keybindings": {
                "command_palette": "ctrl+j",
                "session_picker": "ctrl+y",
                "queue_follow_up": "f5",
                "accept_completion": "f2",
                "thinking_cycle": "f3",
                "model_cycle": "f6",
                "toggle_thinking": "f4",
                "copy_message": "ctrl+b"
            },
            "theme": "high-contrast"
        })))
        .unwrap();
        assert_eq!(settings.keybindings.command_palette, "ctrl+j");
        assert_eq!(settings.keybindings.toggle_tool_results, "ctrl+o");
        assert_eq!(settings.theme, TuiThemeName::HighContrast);
        assert_eq!(settings.resolved_theme(), high_contrast_theme());
    }

    #[test]
    fn ignores_removed_message_selection_keybindings() {
        let settings = tui_settings_from_json(&obj(serde_json::json!({
            "keybindings": {"message_previous": "alt+up", "message_next": "alt+down"}
        })))
        .unwrap();
        assert_eq!(settings, TuiSettings::default());
    }

    #[test]
    fn rejects_unknown_fields_and_theme() {
        assert!(tui_settings_from_json(&obj(serde_json::json!({"palette": {}})))
            .unwrap_err()
            .0
            .contains("Unknown TUI settings field"));
        assert!(tui_settings_from_json(&obj(serde_json::json!({"theme": "solarized"})))
            .unwrap_err()
            .0
            .contains("Unknown TUI theme"));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let err = tui_settings_from_json(&obj(serde_json::json!({
            "keybindings": {"cancel": "escape", "command_palette": "escape"}
        })))
        .unwrap_err();
        assert!(err.0.contains("assigned to both"));
    }

    #[test]
    fn accepts_light_theme() {
        let settings = tui_settings_from_json(&obj(serde_json::json!({"theme": "tau-light"}))).unwrap();
        assert_eq!(settings.theme, TuiThemeName::TauLight);
        assert_eq!(settings.resolved_theme().screen_background, "#ffffff");
        assert_eq!(settings.resolved_theme().syntax_theme, "ansi_light");
    }

    #[test]
    fn auto_copy_selection_roundtrips_and_validates() {
        let settings =
            tui_settings_from_json(&obj(serde_json::json!({"auto_copy_selection": true}))).unwrap();
        assert!(settings.auto_copy_selection);
        assert_eq!(settings.to_json()["auto_copy_selection"], Value::Bool(true));

        let err = tui_settings_from_json(&obj(serde_json::json!({"auto_copy_selection": "yes"})))
            .unwrap_err();
        assert!(err.0.contains("auto_copy_selection"));
    }

    #[test]
    fn keybindings_serialize_in_order() {
        let json = TuiSettings::default().to_json();
        let keys: Vec<&String> = json.keys().collect();
        assert_eq!(keys, vec!["auto_copy_selection", "keybindings", "sidebar_position", "theme"]);
        let kb = &json["keybindings"];
        assert_eq!(kb["command_palette"], Value::String("ctrl+k".into()));
        assert_eq!(kb["toggle_tool_results"], Value::String("ctrl+o".into()));
    }

    #[test]
    fn get_theme_returns_builtin() {
        assert_eq!(get_tui_theme(TuiThemeName::HighContrast).prompt_border, "#00ff66");
        assert_eq!(get_tui_theme(TuiThemeName::TauLight).prompt_border, "#2563eb");
        assert_eq!(get_tui_theme(TuiThemeName::TauDark).screen_background, "#000000");
    }

    #[test]
    fn sidebar_position_roundtrips_and_rejects_invalid() {
        for value in ["left", "right", "off"] {
            let settings =
                tui_settings_from_json(&obj(serde_json::json!({"sidebar_position": value}))).unwrap();
            assert_eq!(settings.sidebar_position.as_str(), value);
            assert_eq!(settings.to_json()["sidebar_position"], Value::String(value.into()));
        }
        assert!(tui_settings_from_json(&obj(serde_json::json!({"sidebar_position": "top"})))
            .unwrap_err()
            .0
            .contains("sidebar_position"));
        assert!(tui_settings_from_json(&obj(serde_json::json!({"sidebar_position": 123})))
            .unwrap_err()
            .0
            .contains("sidebar_position"));
    }
}
