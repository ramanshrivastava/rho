//! Prompt autocomplete (port of tau `tau_coding/tui/autocomplete.py`).
//!
//! Pure logic over the current prompt text: slash-command / skill / prompt-template
//! completion, command-argument value completion (model, login, theme, resume),
//! `@file` references, and `!`/`!!` shell-path completion. Ported 1:1 with tau's
//! trigger and selection semantics; covered by the transferred
//! `test_tui_autocomplete.py`.
//!
//! Offsets are byte indices into the prompt string. tau uses Python codepoint
//! indices, but every token boundary the port keys on (`/`, `@`, `:`, space,
//! `!`) is ASCII, so byte and codepoint offsets coincide at those positions and
//! [`CompletionItem::apply`] stays consistent with how they were computed.

use std::path::{Path, PathBuf};

use rho_coding::commands::{CommandRegistry, SlashCommand};
use rho_coding::prompt_templates::PromptTemplate;
use rho_coding::skills::Skill;

/// Directories excluded from file/shell path completion (tau
/// `IGNORED_FILE_COMPLETION_DIRS`).
pub const IGNORED_FILE_COMPLETION_DIRS: [&str; 12] = [
    ".git",
    ".hg",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tau",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
];
/// Maximum file/shell completions returned.
pub const MAX_FILE_COMPLETIONS: usize = 50;

/// A possible argument completion value with optional metadata (tau
/// `CompletionOption`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionOption {
    /// The completion value.
    pub value: String,
    /// An optional description.
    pub description: Option<String>,
}

impl CompletionOption {
    /// Build a completion option.
    pub fn new(value: impl Into<String>, description: Option<String>) -> Self {
        Self {
            value: value.into(),
            description,
        }
    }
}

/// One selectable prompt completion (tau `CompletionItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// Display text.
    pub display: String,
    /// The replacement inserted on accept.
    pub replacement: String,
    /// Start byte offset of the replaced span.
    pub start: usize,
    /// End byte offset of the replaced span.
    pub end: usize,
    /// Optional description.
    pub description: Option<String>,
    /// Optional grouping category.
    pub category: Option<String>,
}

impl CompletionItem {
    /// Apply this completion to input text (tau `apply`).
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        format!("{}{}{}", &text[..self.start], self.replacement, &text[self.end..])
    }
}

/// Current autocomplete state for the prompt input (tau `CompletionState`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionState {
    /// The available items.
    pub items: Vec<CompletionItem>,
    /// The selected index.
    pub selected_index: usize,
}

impl CompletionState {
    /// A state wrapping the given items (selection at 0).
    #[must_use]
    pub fn new(items: Vec<CompletionItem>) -> Self {
        Self {
            items,
            selected_index: 0,
        }
    }

    /// The currently selected item, or `None`.
    #[must_use]
    pub fn selected(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected_index)
    }

    /// A state with the next item selected (wrapping).
    #[must_use]
    pub fn select_next(&self) -> Self {
        if self.items.is_empty() {
            return self.clone();
        }
        Self {
            items: self.items.clone(),
            selected_index: (self.selected_index + 1) % self.items.len(),
        }
    }

    /// A state with the previous item selected (wrapping).
    #[must_use]
    pub fn select_previous(&self) -> Self {
        if self.items.is_empty() {
            return self.clone();
        }
        let len = self.items.len();
        Self {
            items: self.items.clone(),
            selected_index: (self.selected_index + len - 1) % len,
        }
    }
}

/// Inputs to [`build_completion_state`] beyond the prompt text and registry.
#[derive(Default, Clone)]
pub struct CompletionInputs<'a> {
    /// Loaded skills.
    pub skills: &'a [Skill],
    /// Loaded prompt templates.
    pub prompt_templates: &'a [PromptTemplate],
    /// Model names for `/model`.
    pub model_names: &'a [String],
    /// Provider names for `/login` / `/logout`.
    pub provider_names: &'a [String],
    /// Thinking levels for `/thinking`.
    pub thinking_levels: &'a [String],
    /// Theme names for `/theme`.
    pub theme_names: &'a [String],
    /// Session ids for `/resume`.
    pub session_ids: &'a [String],
    /// Session options (with descriptions) for `/resume`.
    pub session_options: &'a [CompletionOption],
    /// The working directory for `@file` / shell completion.
    pub cwd: Option<&'a Path>,
}

/// Build autocomplete suggestions for the current prompt text (tau
/// `build_completion_state`).
#[must_use]
pub fn build_completion_state(
    text: &str,
    registry: &CommandRegistry,
    inputs: &CompletionInputs,
) -> CompletionState {
    if !text.starts_with('/') || text.starts_with("//") {
        if let Some(cwd) = inputs.cwd {
            if let Some(shell_completions) = shell_path_completions(text, cwd) {
                return CompletionState::new(shell_completions);
            }
            return CompletionState::new(file_reference_completions(text, cwd));
        }
        return CompletionState::default();
    }

    let token_end = first_token_end(text);
    let token = &text[..token_end];
    let has_argument_text = token_end < text.len();
    if token.starts_with("/skill:") {
        if has_argument_text && matches_skill_command(token, inputs.skills) {
            return CompletionState::default();
        }
        return CompletionState::new(skill_completions(token, token_end, inputs.skills));
    }

    if token.contains(':') {
        return CompletionState::default();
    }

    if let Some(argument_completions) = command_argument_completions(text, token_end, inputs) {
        return CompletionState::new(argument_completions);
    }

    if has_argument_text
        && (matches_prompt_template_command(token, inputs.prompt_templates)
            || matches_registered_command(token, registry))
    {
        return CompletionState::default();
    }

    CompletionState::new(command_completions(
        token,
        token_end,
        registry,
        inputs.prompt_templates,
    ))
}

fn first_token_end(text: &str) -> usize {
    text.find(' ').unwrap_or(text.len())
}

fn argument_token_end(text: &str, start: usize) -> usize {
    text[start..].find(' ').map_or(text.len(), |i| start + i)
}

fn matches_skill_command(token: &str, skills: &[Skill]) -> bool {
    let command_name = token.trim_start_matches("/skill:").to_lowercase();
    skills.iter().any(|s| s.name.to_lowercase() == command_name)
}

fn matches_prompt_template_command(token: &str, prompt_templates: &[PromptTemplate]) -> bool {
    let command_name = token.trim_start_matches('/').to_lowercase();
    prompt_templates
        .iter()
        .any(|t| t.name.to_lowercase() == command_name)
}

fn matches_registered_command(token: &str, registry: &CommandRegistry) -> bool {
    let command_name = token.trim_start_matches('/').to_lowercase();
    registry.get(&command_name).is_some()
}

fn command_completion_sort_key(item: &CompletionItem, prefix: &str) -> (u8, String) {
    if prefix.is_empty() {
        return (0, item.display.clone());
    }
    let display_name = item
        .display
        .trim_start_matches('/')
        .trim_end_matches(':')
        .to_lowercase();
    let rank = u8::from(!display_name.starts_with(prefix));
    (rank, item.display.clone())
}

fn command_alias_completions(
    command: &SlashCommand,
    prefix: &str,
    token_end: usize,
) -> Vec<CompletionItem> {
    let names: Vec<&str> = if prefix.is_empty() {
        vec![command.name.as_str()]
    } else {
        let mut v = vec![command.name.as_str()];
        v.extend(command.aliases.iter().map(String::as_str));
        v.extend(command.search_terms.iter().map(String::as_str));
        v
    };
    let mut suggestions = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for name in names {
        if !name.starts_with(prefix) {
            continue;
        }
        let is_name_or_alias =
            name == command.name || command.aliases.iter().any(|a| a == name);
        let replacement_name = if is_name_or_alias { name } else { command.name.as_str() };
        let (mut display, mut replacement) =
            (format!("/{replacement_name}"), format!("/{replacement_name}"));
        if command.name == "skill" && replacement_name == command.name {
            display = "/skill:".to_string();
            replacement = "/skill:".to_string();
        }
        if seen.contains(&display) {
            continue;
        }
        seen.push(display.clone());
        suggestions.push(CompletionItem {
            display,
            replacement,
            start: 0,
            end: token_end,
            description: Some(command.description.clone()),
            category: Some("Commands".to_string()),
        });
    }
    suggestions
}

fn command_completions(
    token: &str,
    token_end: usize,
    registry: &CommandRegistry,
    prompt_templates: &[PromptTemplate],
) -> Vec<CompletionItem> {
    let prefix = token.trim_start_matches('/').to_lowercase();
    let mut command_suggestions: Vec<CompletionItem> = Vec::new();
    for command in registry.list_commands() {
        command_suggestions.extend(command_alias_completions(command, &prefix, token_end));
    }
    let mut prompt_suggestions: Vec<CompletionItem> = prompt_templates
        .iter()
        .filter(|t| t.name.to_lowercase().starts_with(&prefix))
        .map(|t| CompletionItem {
            display: format!("/{}", t.name),
            replacement: format!("/{}", t.name),
            start: 0,
            end: token_end,
            description: Some(
                t.description
                    .clone()
                    .unwrap_or_else(|| "Prompt template".to_string()),
            ),
            category: Some("Custom prompts".to_string()),
        })
        .collect();

    command_suggestions.sort_by_key(|item| command_completion_sort_key(item, &prefix));
    prompt_suggestions.sort_by_key(|item| command_completion_sort_key(item, &prefix));
    command_suggestions.extend(prompt_suggestions);
    command_suggestions
}

fn skill_completions(token: &str, token_end: usize, skills: &[Skill]) -> Vec<CompletionItem> {
    let prefix = token.trim_start_matches("/skill:").to_lowercase();
    let mut ordered: Vec<&Skill> = skills.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    ordered
        .into_iter()
        .filter(|s| s.name.to_lowercase().starts_with(&prefix))
        .map(|s| CompletionItem {
            display: format!("/skill:{}", s.name),
            replacement: format!("/skill:{}", s.name),
            start: 0,
            end: token_end,
            description: s.description.clone(),
            category: None,
        })
        .collect()
}

fn command_argument_completions(
    text: &str,
    token_end: usize,
    inputs: &CompletionInputs,
) -> Option<Vec<CompletionItem>> {
    if token_end >= text.len() {
        return None;
    }
    let command_name = text[..token_end].trim_start_matches('/').to_lowercase();
    match command_name.as_str() {
        "model" | "scoped-models" => Some(value_completions(
            text,
            token_end + 1,
            &completion_options(inputs.model_names, "Switch model"),
            true,
        )),
        "login" | "logout" => Some(value_completions(
            text,
            token_end + 1,
            &completion_options(inputs.provider_names, "Switch provider"),
            true,
        )),
        "resume" => {
            let options = if inputs.session_options.is_empty() {
                completion_options(inputs.session_ids, "Resume session")
            } else {
                inputs.session_options.to_vec()
            };
            Some(value_completions(text, token_end + 1, &options, false))
        }
        "theme" => Some(value_completions(
            text,
            token_end + 1,
            &completion_options(inputs.theme_names, "Set TUI theme"),
            false,
        )),
        _ => None,
    }
}

fn value_completions(
    text: &str,
    start: usize,
    options: &[CompletionOption],
    sort: bool,
) -> Vec<CompletionItem> {
    let end = argument_token_end(text, start);
    let prefix = text[start..end].to_lowercase();
    let mut ordered: Vec<&CompletionOption> = options.iter().collect();
    if sort {
        ordered.sort_by(|a, b| a.value.cmp(&b.value));
    }
    ordered
        .into_iter()
        .filter(|opt| opt.value.to_lowercase().starts_with(&prefix))
        .map(|opt| CompletionItem {
            display: opt.value.clone(),
            replacement: opt.value.clone(),
            start,
            end,
            description: opt.description.clone(),
            category: None,
        })
        .collect()
}

fn completion_options(values: &[String], description: &str) -> Vec<CompletionOption> {
    values
        .iter()
        .map(|value| CompletionOption::new(value.clone(), Some(description.to_string())))
        .collect()
}

// ---------------------------------------------------------------------------
// File-reference (`@path`) completion
// ---------------------------------------------------------------------------

fn file_reference_completions(text: &str, cwd: &Path) -> Vec<CompletionItem> {
    let Some((start, end)) = active_file_reference_token(text) else {
        return Vec::new();
    };
    let prefix = text[start + 1..end].to_lowercase();
    let mut suggestions = Vec::new();
    for (path, is_dir) in iter_file_reference_paths(cwd) {
        let relative = relative_posix(&path, cwd);
        if !relative.to_lowercase().contains(&prefix) {
            continue;
        }
        let display = format!("@{relative}{}", if is_dir { "/" } else { "" });
        suggestions.push(CompletionItem {
            display: display.clone(),
            replacement: display,
            start,
            end,
            description: Some("File reference".to_string()),
            category: None,
        });
        if suggestions.len() >= MAX_FILE_COMPLETIONS {
            break;
        }
    }
    suggestions
}

fn active_file_reference_token(text: &str) -> Option<(usize, usize)> {
    let cursor = text.len();
    let token_start = text[..cursor]
        .rfind([' ', '\n'])
        .map_or(0, |i| i + 1);
    let at_index = text[token_start..cursor].rfind('@').map(|i| token_start + i)?;
    Some((at_index, cursor))
}

fn iter_file_reference_paths(cwd: &Path) -> Vec<(PathBuf, bool)> {
    if !cwd.exists() || !cwd.is_dir() {
        return Vec::new();
    }
    let mut paths = Vec::new();
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut children: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort_by_key(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });
        for child in children {
            if is_ignored_file_completion_path(&child, cwd) {
                continue;
            }
            let is_dir = child.is_dir();
            paths.push((child.clone(), is_dir));
            if is_dir {
                stack.push(child);
            }
        }
    }
    paths
}

fn is_ignored_file_completion_path(path: &Path, cwd: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(cwd) else {
        return true;
    };
    relative.components().any(|component| {
        let part = component.as_os_str().to_string_lossy();
        part.starts_with('.') || IGNORED_FILE_COMPLETION_DIRS.contains(&part.as_ref())
    })
}

fn relative_posix(path: &Path, cwd: &Path) -> String {
    let relative = path.strip_prefix(cwd).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// Shell-path (`!`/`!!`) completion
// ---------------------------------------------------------------------------

fn shell_path_completions(text: &str, cwd: &Path) -> Option<Vec<CompletionItem>> {
    let (_, command_start) = shell_command_prefix_span(text)?;

    let (start, end) = active_shell_path_token(text, command_start);
    let token = &text[start..end];
    if token.is_empty() {
        return Some(Vec::new());
    }

    let Some((parent_text, name_prefix, replacement_prefix)) = parse_shell_path_token(token) else {
        return Some(Vec::new());
    };

    let parent_dir = if parent_text.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(&parent_text)
    };
    if !parent_dir.exists() || !parent_dir.is_dir() {
        return Some(Vec::new());
    }
    if parent_dir != cwd && is_ignored_file_completion_path(&parent_dir, cwd) {
        return Some(Vec::new());
    }

    let Ok(read_dir) = std::fs::read_dir(&parent_dir) else {
        return Some(Vec::new());
    };
    let mut children: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    let mut suggestions = Vec::new();
    let name_prefix_lower = name_prefix.to_lowercase();
    for child in children {
        if is_ignored_file_completion_path(&child, cwd) {
            continue;
        }
        let child_name = child
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !child_name.to_lowercase().starts_with(&name_prefix_lower) {
            continue;
        }
        let relative = relative_posix(&child, cwd);
        let replacement = format!(
            "{replacement_prefix}{relative}{}",
            if child.is_dir() { "/" } else { "" }
        );
        if replacement == token {
            continue;
        }
        suggestions.push(CompletionItem {
            display: replacement.clone(),
            replacement,
            start,
            end,
            description: Some(if child.is_dir() { "Directory" } else { "File" }.to_string()),
            category: None,
        });
        if suggestions.len() >= MAX_FILE_COMPLETIONS {
            break;
        }
    }
    Some(suggestions)
}

fn shell_command_prefix_span(text: &str) -> Option<(usize, usize)> {
    let leading_whitespace = text.len() - text.trim_start().len();
    let stripped = &text[leading_whitespace..];
    if stripped.starts_with("!!") {
        Some((leading_whitespace, leading_whitespace + 2))
    } else if stripped.starts_with('!') {
        Some((leading_whitespace, leading_whitespace + 1))
    } else {
        None
    }
}

fn active_shell_path_token(text: &str, command_start: usize) -> (usize, usize) {
    let cursor = text.len();
    let mut token_start = command_start;
    let mut escaped = false;
    let chars: Vec<(usize, char)> = text[command_start..cursor]
        .char_indices()
        .map(|(i, c)| (command_start + i, c))
        .collect();
    for (idx, ch) in chars.iter().rev() {
        if escaped {
            escaped = false;
            continue;
        }
        if *ch == '\\' {
            escaped = true;
            continue;
        }
        if ch.is_whitespace() {
            token_start = idx + ch.len_utf8();
            break;
        }
    }
    (token_start, cursor)
}

fn parse_shell_path_token(token: &str) -> Option<(String, String, String)> {
    let mut replacement_prefix = String::new();
    let mut path_text = token;
    if let Some(rest) = path_text.strip_prefix("./") {
        replacement_prefix = "./".to_string();
        path_text = rest;
    }
    if path_text.starts_with('/') || path_text.starts_with('~') {
        return None;
    }
    if path_text.contains(['"', '\'', '`', '$', '*', '?', '[', '{']) {
        return None;
    }
    let (parent_text, separator, name_prefix) = match path_text.rfind('/') {
        Some(i) => (&path_text[..i], true, &path_text[i + 1..]),
        None => ("", false, path_text),
    };
    if separator && parent_text.is_empty() {
        return None;
    }
    let parent_parts: Vec<&str> = if parent_text.is_empty() {
        Vec::new()
    } else {
        parent_text.split('/').collect()
    };
    if parent_parts.iter().any(|p| matches!(*p, "" | "." | "..")) {
        return None;
    }
    Some((
        parent_text.to_string(),
        name_prefix.to_string(),
        replacement_prefix,
    ))
}
