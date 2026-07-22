//! The prompt composer chrome: activity-indicator prefix, queued-message
//! banner, and the autocomplete popup (ports of tau's `#prompt-prefix`,
//! `_render_queued_messages`, and `render_completion_suggestions`).
//!
//! The editable prompt itself is a [`tui_textarea::TextArea`] owned by `app.rs`;
//! these functions render the surrounding chrome around it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::autocomplete::CompletionState;
use crate::motion::{self, MotionCaps};
use crate::theme::TuiTheme;
use crate::widgets::style::parse_style;

/// The rho prompt-prefix glyph — always the rho mark `ρ`.
///
/// tau animates a 3-row bouncing square while running and shows `τ` idle. rho's
/// prefix is a single cell that is always the `ρ` glyph; its *motion* lives in
/// the color: a settled dim ρ when idle, and a throbbing ember ρ while a turn
/// runs (heated-iron breathing along the oxide ramp — see [`crate::motion`]).
/// The π→τ→ρ lineage cycle now lives, animated, in the heritage splash.
#[must_use]
pub const fn prompt_prefix() -> &'static str {
    "ρ"
}

/// Render the prompt-prefix cell into `area` (single column): the `ρ` glyph,
/// throbbing along the oxide ramp while `running` (static dim when idle, or when
/// motion is unavailable). This is the working-state "spinner" equivalent —
/// rho's answer to tau's animated activity indicator, and the fix for the
/// static-while-running parity gap.
pub fn render_prompt_prefix(
    frame: &mut Frame,
    area: Rect,
    running: bool,
    frame_idx: usize,
    caps: MotionCaps,
) {
    let style = motion::ember_throb_style(caps, frame_idx, running);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(prompt_prefix(), style))),
        area,
    );
}

/// Build the working-state signature line shown above the composer while a turn
/// runs: `⟨shimmering forge-verb⟩…  ·  ⟨elapsed⟩  ·  ⟨interrupt⟩ to interrupt`,
/// e.g. `Tempering…  ·  2m 14s  ·  esc to interrupt`. The verb shimmers along the
/// oxide ramp (Codex's light-sweep, oxidized); the whole line degrades to plain
/// oxide + muted text under reduced-motion / non-truecolor.
#[must_use]
pub fn build_working_status_line(
    verb: &str,
    elapsed_secs: u64,
    frame_idx: usize,
    interrupt_key: &str,
    caps: MotionCaps,
    theme: &TuiTheme,
) -> Line<'static> {
    let muted = parse_style(&theme.muted_text);
    let mut spans = motion::shimmer_spans(verb, caps, frame_idx);
    spans.push(Span::styled("…", muted));
    spans.push(Span::styled("  ·  ", muted));
    spans.push(Span::styled(
        motion::format_working_elapsed(elapsed_secs),
        parse_style(&theme.accent),
    ));
    spans.push(Span::styled("  ·  ", muted));
    spans.push(Span::styled(format!("{interrupt_key} to interrupt"), muted));
    Line::from(spans)
}

/// Render the working-state signature line into `area` (see
/// [`build_working_status_line`]).
#[allow(clippy::too_many_arguments)]
pub fn render_working_status(
    frame: &mut Frame,
    area: Rect,
    verb: &str,
    elapsed_secs: u64,
    frame_idx: usize,
    interrupt_key: &str,
    caps: MotionCaps,
    theme: &TuiTheme,
) {
    let bg = parse_style(&theme.chrome_background);
    let line = build_working_status_line(verb, elapsed_secs, frame_idx, interrupt_key, caps, theme);
    frame.render_widget(Paragraph::new(line).style(bg), area);
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
            previous_category.clone_from(&item.category);
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
    fn prompt_prefix_is_the_rho_glyph() {
        // The prefix glyph is always the rho mark; its motion is in the color
        // (throbbing ember while running, static dim when idle), not the glyph.
        assert_eq!(prompt_prefix(), "ρ");
        // The glyph must NOT be a braille spinner frame (tau's old transcript tool
        // spinner glyphs, removed in fd327d0).
        assert_ne!(prompt_prefix(), "⠋");
    }

    #[test]
    fn working_status_line_reads_verb_timer_and_interrupt() {
        let theme = get_tui_theme(TuiThemeName::Rho);
        // Plain caps → the verb is a single span, so the whole line reads cleanly.
        let line =
            build_working_status_line("Tempering", 134, 0, "esc", MotionCaps::plain(), &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Tempering…  ·  2m 14s  ·  esc to interrupt");
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
