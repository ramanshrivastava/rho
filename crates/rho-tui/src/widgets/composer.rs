//! The prompt composer chrome: activity-indicator prefix, queued-message
//! banner, and the autocomplete popup (ports of tau's `#prompt-prefix`,
//! `_render_queued_messages`, and `render_completion_suggestions`).
//!
//! The editable prompt itself is a [`tui_textarea::TextArea`] owned by `app.rs`;
//! these functions render the surrounding chrome around it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::autocomplete::CompletionState;
use crate::state::TOOL_SPINNER_FRAMES;
use crate::theme::TuiTheme;
use crate::widgets::style::parse_style;

/// The prompt-prefix glyph shown left of the composer.
///
/// tau animates a 3-row bouncing square while running and shows `τ` idle. rho
/// collapses that to a single animated cell: the rho glyph `ρ` when idle, a
/// braille spinner frame while running (the same `TOOL_SPINNER_FRAMES` the
/// transcript uses), rebranded from tau's `τ`.
#[must_use]
pub fn prompt_prefix(running: bool, frame_idx: usize) -> String {
    if running {
        TOOL_SPINNER_FRAMES[frame_idx % TOOL_SPINNER_FRAMES.len()].to_string()
    } else {
        "ρ".to_string()
    }
}

/// Render the prompt-prefix cell into `area` (single column).
pub fn render_prompt_prefix(
    frame: &mut Frame,
    area: Rect,
    running: bool,
    frame_idx: usize,
    theme: &TuiTheme,
) {
    let style = if running {
        parse_style(&theme.accent).add_modifier(Modifier::BOLD)
    } else {
        parse_style(&theme.prompt_text).add_modifier(Modifier::BOLD)
    };
    let glyph = prompt_prefix(running, frame_idx);
    frame.render_widget(Paragraph::new(Line::from(Span::styled(glyph, style))), area);
}

/// The first line of a queued message (tau `_queued_message_preview`).
#[must_use]
pub fn queued_message_preview(message: &str) -> String {
    message.lines().next().unwrap_or("").to_string()
}

/// Build the queued-message banner lines: `↪ steering · queued: {preview}` per
/// steering message, `↳ follow-up · queued: {preview}` per follow-up message.
#[must_use]
pub fn build_queued_message_lines(
    steering: &[String],
    follow_up: &[String],
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let muted = parse_style(&theme.muted_text);
    let text = parse_style(&theme.prompt_text);
    let mut lines = Vec::new();
    for message in steering {
        lines.push(Line::from(vec![
            Span::styled("↪ steering · queued: ", muted),
            Span::styled(queued_message_preview(message), text),
        ]));
    }
    for message in follow_up {
        lines.push(Line::from(vec![
            Span::styled("↳ follow-up · queued: ", muted),
            Span::styled(queued_message_preview(message), text),
        ]));
    }
    lines
}

/// Render the queued-message banner into `area`. Renders nothing when both
/// queues are empty (the caller allots zero height in that case).
pub fn render_queued_messages(
    frame: &mut Frame,
    area: Rect,
    steering: &[String],
    follow_up: &[String],
    theme: &TuiTheme,
) {
    let lines = build_queued_message_lines(steering, follow_up, theme);
    if lines.is_empty() {
        return;
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Build the autocomplete popup lines (port of tau
/// `render_completion_suggestions`): grouped by category, a `› ` marker on the
/// selected row and `  ` on the rest, command + description columns.
#[must_use]
pub fn build_completion_lines(state: &CompletionState, theme: &TuiTheme) -> Vec<Line<'static>> {
    let description_style = parse_style(&theme.completion_description);
    let selected_style = parse_style(&theme.completion_selected);
    let selected_description_style = parse_style(&theme.completion_selected_description);
    let text_style = parse_style(&theme.prompt_text);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut previous_category: Option<String> = None;
    for (index, item) in state.items.iter().enumerate() {
        if item.category != previous_category {
            if index > 0 {
                lines.push(Line::default());
            }
            if let Some(category) = &item.category {
                lines.push(Line::from(Span::styled(
                    category.clone(),
                    description_style,
                )));
            }
            previous_category = item.category.clone();
        }

        let selected = index == state.selected_index;
        let prefix = if selected { "› " } else { "  " };
        let command_style = if selected { selected_style } else { text_style };
        let desc_style = if selected {
            selected_description_style
        } else {
            description_style
        };
        let mut spans = vec![
            Span::styled(prefix.to_string(), command_style),
            Span::styled(item.display.clone(), command_style),
            Span::styled("  ".to_string(), command_style),
        ];
        if let Some(description) = &item.description {
            spans.push(Span::styled(description.clone(), desc_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Render the autocomplete popup into `area` with the theme's popup background.
pub fn render_completion_popup(
    frame: &mut Frame,
    area: Rect,
    state: &CompletionState,
    theme: &TuiTheme,
) {
    if state.items.is_empty() {
        return;
    }
    let bg: Style = parse_style(&theme.autocomplete_background);
    let lines = build_completion_lines(state, theme);
    frame.render_widget(Paragraph::new(lines).style(bg), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::{CompletionItem, CompletionState};
    use crate::theme::{TuiThemeName, get_tui_theme};

    fn item(display: &str, category: Option<&str>, description: Option<&str>) -> CompletionItem {
        CompletionItem {
            display: display.to_string(),
            replacement: display.to_string(),
            start: 0,
            end: 0,
            description: description.map(str::to_string),
            category: category.map(str::to_string),
        }
    }

    #[test]
    fn prompt_prefix_switches_on_running() {
        assert_eq!(prompt_prefix(false, 0), "ρ");
        assert_eq!(prompt_prefix(true, 0), TOOL_SPINNER_FRAMES[0]);
        assert_eq!(prompt_prefix(true, 3), TOOL_SPINNER_FRAMES[3]);
    }

    #[test]
    fn queued_preview_is_first_line() {
        assert_eq!(queued_message_preview("hello\nworld"), "hello");
        assert_eq!(queued_message_preview(""), "");
    }

    #[test]
    fn queued_lines_label_steer_and_follow_up() {
        let theme = get_tui_theme(TuiThemeName::TauDark);
        let lines = build_queued_message_lines(
            &["fix the bug".to_string()],
            &["then add tests".to_string()],
            &theme,
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text[0], "↪ steering · queued: fix the bug");
        assert_eq!(text[1], "↳ follow-up · queued: then add tests");
    }

    #[test]
    fn completion_lines_group_by_category_with_marker() {
        let theme = get_tui_theme(TuiThemeName::TauDark);
        let mut state = CompletionState::new(vec![
            item("/clear", Some("Commands"), Some("clear the screen")),
            item("/compact", Some("Commands"), Some("compact context")),
        ]);
        state = state.select_next(); // select index 1
        let lines = build_completion_lines(&state, &theme);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // First line is the category header.
        assert_eq!(text[0], "Commands");
        assert!(text[1].starts_with("  /clear"));
        assert!(text[2].starts_with("› /compact"));
    }
}
