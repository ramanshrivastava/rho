//! The session sidebar (port of tau `widgets.render_session_sidebar`).
//!
//! A dark, minimalist summary docked on the left: a centered logo, a `session`
//! metadata grid (provider / model / thinking / tools / skills counts), then
//! `context` / `tools` / `skills` / `prompts` bullet-list sections separated by
//! subtle rules. tau's `TAU_SIDEBAR_LOGO` (`"τ = 2π"`) is rebranded to `ρ` for
//! rho's identity (an intentional divergence, journaled in `phase-5.md`).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::TuiTheme;
use crate::widgets::style::parse_style;

/// rho's sidebar logo (tau uses `"τ = 2π"`; rebranded here).
pub const SIDEBAR_LOGO: &str = "ρ";

/// A snapshot of the session facts the sidebar renders (built in `app.rs` so the
/// widget never borrows the live session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarInfo {
    /// Active provider name.
    pub provider_name: String,
    /// Active model id.
    pub model: String,
    /// Resolved thinking-level display.
    pub thinking_display: String,
    /// Number of tools.
    pub tools_count: usize,
    /// Number of loaded skills.
    pub skills_count: usize,
    /// Resolved context-file labels.
    pub context_labels: Vec<String>,
    /// Tool names.
    pub tool_names: Vec<String>,
    /// Skill names.
    pub skill_names: Vec<String>,
    /// Prompt-template names.
    pub prompt_names: Vec<String>,
}

/// Build the sidebar's rendered lines.
#[must_use]
pub fn build_sidebar_lines(info: &SidebarInfo, theme: &TuiTheme) -> Vec<Line<'static>> {
    let label_style = parse_style(&theme.completion_description);
    let value_style = parse_style(&theme.prompt_text);
    let header_style = parse_style(&theme.accent).add_modifier(Modifier::BOLD);
    let logo_style = value_style.add_modifier(Modifier::BOLD);
    let rule_style = parse_style(&theme.border);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(SIDEBAR_LOGO, logo_style)));
    lines.push(Line::default());

    // session metadata grid
    push_section_header(&mut lines, "session", header_style);
    push_metadata(
        &mut lines,
        "provider",
        &info.provider_name,
        label_style,
        value_style,
    );
    push_metadata(&mut lines, "model", &info.model, label_style, value_style);
    push_metadata(
        &mut lines,
        "thinking",
        &info.thinking_display,
        label_style,
        value_style,
    );
    push_metadata(
        &mut lines,
        "tools",
        &info.tools_count.to_string(),
        label_style,
        value_style,
    );
    push_metadata(
        &mut lines,
        "skills",
        &info.skills_count.to_string(),
        label_style,
        value_style,
    );
    push_rule(&mut lines, rule_style);

    push_section_header(&mut lines, "context", header_style);
    push_bullets(
        &mut lines,
        &info.context_labels,
        "No context files",
        label_style,
        value_style,
    );
    push_rule(&mut lines, rule_style);

    push_section_header(&mut lines, "tools", header_style);
    push_bullets(
        &mut lines,
        &info.tool_names,
        "No tools",
        label_style,
        value_style,
    );
    push_rule(&mut lines, rule_style);

    push_section_header(&mut lines, "skills", header_style);
    push_bullets(
        &mut lines,
        &info.skill_names,
        "No skills loaded yet",
        label_style,
        value_style,
    );
    push_rule(&mut lines, rule_style);

    push_section_header(&mut lines, "prompts", header_style);
    push_bullets(
        &mut lines,
        &info.prompt_names,
        "No prompt templates",
        label_style,
        value_style,
    );

    lines
}

fn push_section_header(lines: &mut Vec<Line<'static>>, title: &str, style: Style) {
    lines.push(Line::from(Span::styled(format!(" {title}"), style)));
}

fn push_metadata(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    label_style: Style,
    value_style: Style,
) {
    lines.push(Line::from(vec![
        Span::styled(format!(" {label} "), label_style),
        Span::styled(value.to_string(), value_style),
    ]));
}

fn push_bullets(
    lines: &mut Vec<Line<'static>>,
    items: &[String],
    empty: &str,
    empty_style: Style,
    item_style: Style,
) {
    if items.is_empty() {
        lines.push(Line::from(Span::styled(format!(" {empty}"), empty_style)));
        return;
    }
    for item in items {
        lines.push(Line::from(vec![
            Span::styled(" • ", empty_style),
            Span::styled(item.clone(), item_style),
        ]));
    }
}

fn push_rule(lines: &mut Vec<Line<'static>>, style: Style) {
    lines.push(Line::from(Span::styled(String::new(), style)));
}

/// Render the sidebar into `area`.
pub fn render_sidebar(frame: &mut Frame, area: Rect, info: &SidebarInfo, theme: &TuiTheme) {
    let bg = parse_style(&theme.sidebar_background);
    let lines = build_sidebar_lines(info, theme);
    frame.render_widget(Paragraph::new(lines).style(bg), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::TuiThemeName;
    use crate::theme::get_tui_theme;

    fn info() -> SidebarInfo {
        SidebarInfo {
            provider_name: "anthropic".to_string(),
            model: "claude".to_string(),
            thinking_display: "medium".to_string(),
            tools_count: 3,
            skills_count: 0,
            context_labels: vec!["AGENTS.md".to_string()],
            tool_names: vec!["bash".to_string(), "read".to_string()],
            skill_names: vec![],
            prompt_names: vec![],
        }
    }

    #[test]
    fn builds_sections_with_logo_and_metadata() {
        let theme = get_tui_theme(TuiThemeName::TauDark);
        let lines = build_sidebar_lines(&info(), &theme);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text[0], SIDEBAR_LOGO);
        assert!(
            text.iter()
                .any(|t| t.contains("provider") && t.contains("anthropic"))
        );
        assert!(
            text.iter()
                .any(|t| t.contains("model") && t.contains("claude"))
        );
        assert!(text.iter().any(|t| t == " • bash"));
        // Empty skills section shows the empty placeholder.
        assert!(text.iter().any(|t| t.contains("No skills loaded yet")));
    }
}
