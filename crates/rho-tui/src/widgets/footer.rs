//! The footer key-hint bar (port of tau's mode-driven `_prompt_bindings` +
//! `Footer()` rendering).
//!
//! tau's footer shows the *prompt* bindings for the current mode, not the raw
//! app bindings: `normal` while idle, `completion` when completions are open,
//! `running` while the agent works. Each mode lists a fixed set of `(key, label)`
//! hints (see `_prompt_bindings`, app.py:5489). The key display is tau's
//! `_key_hint`: capitalize each `+`-separated segment.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::{TuiKeybindings, TuiTheme};
use crate::widgets::style::parse_style;

/// Which footer hint set is active (tau `_prompt_footer_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterMode {
    /// Idle: submit / newline / commands / … / quit.
    Normal,
    /// Completions open: complete / choose / close.
    Completion,
    /// Agent running: steer / follow-up / cancel / thinking / tools.
    Running,
}

/// Format a key spec the way tau's `_key_hint` does: capitalize each
/// `+`-separated segment (`"shift+tab"` → `"Shift+Tab"`).
#[must_use]
pub fn key_hint(key: &str) -> String {
    key.split('+').map(capitalize).collect::<Vec<_>>().join("+")
}

fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// The ordered `(key-display, label)` hints for the given footer mode, exactly
/// matching tau's `_prompt_bindings` visible entries.
#[must_use]
pub fn footer_hints(mode: FooterMode, kb: &TuiKeybindings) -> Vec<(String, String)> {
    match mode {
        FooterMode::Completion => vec![
            (
                format!("{}/Enter", key_hint(&kb.accept_completion)),
                "Complete".to_string(),
            ),
            (
                format!(
                    "{}/{}",
                    key_hint(&kb.completion_previous),
                    key_hint(&kb.completion_next)
                ),
                "Choose".to_string(),
            ),
            (key_hint(&kb.cancel), "Close".to_string()),
        ],
        FooterMode::Running => vec![
            ("Enter".to_string(), "Steer".to_string()),
            (key_hint(&kb.queue_follow_up), "Follow-up".to_string()),
            (key_hint(&kb.cancel), "Cancel".to_string()),
            (key_hint(&kb.toggle_thinking), "Thinking".to_string()),
            (key_hint(&kb.toggle_tool_results), "Tools".to_string()),
        ],
        FooterMode::Normal => vec![
            ("Enter".to_string(), "Submit".to_string()),
            ("Shift+Enter".to_string(), "Newline".to_string()),
            (key_hint(&kb.command_palette), "Commands".to_string()),
            (key_hint(&kb.session_picker), "Sessions".to_string()),
            (key_hint(&kb.thinking_cycle), "Thinking".to_string()),
            (key_hint(&kb.model_cycle), "Model".to_string()),
            (key_hint(&kb.copy_message), "Clear".to_string()),
            (key_hint(&kb.quit), "Quit".to_string()),
        ],
    }
}

/// Render the footer hint bar into `area`.
///
/// Each hint renders as `⟨key⟩ label` with the key in the accent color and the
/// label in muted text, separated by two spaces — a flat, single-line analogue
/// of Textual's `Footer`.
pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    mode: FooterMode,
    kb: &TuiKeybindings,
    theme: &TuiTheme,
) {
    let key_style = parse_style(&theme.accent);
    let label_style = parse_style(&theme.chrome_text);
    let bg = parse_style(&theme.chrome_background);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, (key, label)) in footer_hints(mode, kb).into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", label_style));
        }
        spans.push(Span::styled(key, key_style));
        spans.push(Span::styled(" ", label_style));
        spans.push(Span::styled(label, label_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hint_capitalizes_segments() {
        assert_eq!(key_hint("shift+tab"), "Shift+Tab");
        assert_eq!(key_hint("ctrl+k"), "Ctrl+K");
        assert_eq!(key_hint("alt+enter"), "Alt+Enter");
        assert_eq!(key_hint("escape"), "Escape");
    }

    #[test]
    fn normal_mode_hints_match_tau() {
        let kb = TuiKeybindings::default();
        let hints = footer_hints(FooterMode::Normal, &kb);
        let labels: Vec<&str> = hints.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Submit", "Newline", "Commands", "Sessions", "Thinking", "Model", "Clear", "Quit"
            ]
        );
        assert_eq!(hints[0].0, "Enter");
        assert_eq!(hints[1].0, "Shift+Enter");
    }

    #[test]
    fn running_mode_hints_match_tau() {
        let kb = TuiKeybindings::default();
        let hints = footer_hints(FooterMode::Running, &kb);
        let labels: Vec<&str> = hints.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Steer", "Follow-up", "Cancel", "Thinking", "Tools"]
        );
        assert_eq!(hints[0].0, "Enter");
    }

    #[test]
    fn completion_mode_hints_match_tau() {
        let kb = TuiKeybindings::default();
        let hints = footer_hints(FooterMode::Completion, &kb);
        let labels: Vec<&str> = hints.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, vec!["Complete", "Choose", "Close"]);
        // accept_completion default "tab" -> "Tab/Enter"
        assert_eq!(hints[0].0, "Tab/Enter");
        // completion_previous "up" / completion_next "down" -> "Up/Down"
        assert_eq!(hints[1].0, "Up/Down");
    }
}
