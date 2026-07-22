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
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarInfo {
    /// Human-friendly session title (tau `session_title`), if named.
    pub session_title: Option<String>,
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
    /// Cumulative user/custom turns on the active branch.
    pub turn_count: usize,
    /// Cumulative tool calls on the active branch.
    pub tool_call_count: usize,
    /// Prompt tokens (input + cache-read + cache-write).
    pub input_tokens: i64,
    /// Output tokens.
    pub output_tokens: i64,
    /// Estimated USD cost, or `None` when pricing is incomplete.
    pub estimated_cost: Option<f64>,
    /// Resolved context-file labels.
    pub context_labels: Vec<String>,
    /// Tool names.
    pub tool_names: Vec<String>,
    /// Skill names.
    pub skill_names: Vec<String>,
    /// Prompt-template names.
    pub prompt_names: Vec<String>,
    /// Loaded extension names, in load order.
    pub extension_names: Vec<String>,
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

    // session title (tau's sidebar `session_title` header)
    let title = info
        .session_title
        .clone()
        .unwrap_or_else(|| "Untitled session".to_string());
    lines.push(Line::from(Span::styled(format!(" {title}"), header_style)));
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

    push_insights(&mut lines, info, header_style, label_style, rule_style);

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
    push_rule(&mut lines, rule_style);

    push_section_header(&mut lines, "extensions", header_style);
    push_bullets(
        &mut lines,
        &info.extension_names,
        "No extensions",
        label_style,
        value_style,
    );

    lines
}

/// tau `_plural`: `singular` unless `count != 1`.
fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

/// tau `_compact_usage_count`: raw under 1k, else `N.Nk`/`N.Nm` with trailing
/// zeros trimmed.
#[allow(clippy::cast_precision_loss)] // token counts are far below f64's 2^52 exact-integer bound
fn compact_usage_count(value: i64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value < 1_000_000 {
        return format!("{}k", trim_decimal(value as f64 / 1_000.0));
    }
    format!("{}m", trim_decimal(value as f64 / 1_000_000.0))
}

/// Format a one-decimal value dropping a trailing `.0` (tau's
/// `.rstrip("0").rstrip(".")`).
fn trim_decimal(value: f64) -> String {
    let formatted = format!("{value:.1}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// tau `_format_cost`: three decimals for sub-cent, else two.
fn format_cost(value: f64) -> String {
    if value > 0.0 && value < 0.01 {
        format!("${value:.3}")
    } else {
        format!("${value:.2}")
    }
}

fn push_section_header(lines: &mut Vec<Line<'static>>, title: &str, style: Style) {
    lines.push(Line::from(Span::styled(format!(" {title}"), style)));
}

/// Push the `activity` + `usage` session-insight sections (tau `session_stats`).
fn push_insights(
    lines: &mut Vec<Line<'static>>,
    info: &SidebarInfo,
    header_style: Style,
    label_style: Style,
    rule_style: Style,
) {
    push_section_header(lines, "activity", header_style);
    let activity = format!(
        "{} {}, {} tool {}",
        info.turn_count,
        plural(info.turn_count, "turn"),
        info.tool_call_count,
        plural(info.tool_call_count, "call"),
    );
    lines.push(Line::from(Span::styled(
        format!(" {activity}"),
        label_style,
    )));
    push_rule(lines, rule_style);

    // tau 7f4be2c: label the sidebar totals as *cumulative* usage — they sum
    // every provider turn this session, which can exceed the active-context
    // estimate shown in the footer's context ratio.
    push_section_header(lines, "cumulative usage", header_style);
    let cost = match info.estimated_cost {
        None => "$N/A".to_string(),
        Some(cost) => format!("~{}", format_cost(cost)),
    };
    let usage = format!(
        "{} in, {} out · {}",
        compact_usage_count(info.input_tokens),
        compact_usage_count(info.output_tokens),
        cost,
    );
    lines.push(Line::from(Span::styled(format!(" {usage}"), label_style)));
    push_rule(lines, rule_style);
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
            session_title: Some("Port session insights".to_string()),
            provider_name: "anthropic".to_string(),
            model: "claude".to_string(),
            thinking_display: "medium".to_string(),
            tools_count: 3,
            skills_count: 0,
            turn_count: 2,
            tool_call_count: 5,
            input_tokens: 12_500,
            output_tokens: 800,
            estimated_cost: Some(0.008),
            context_labels: vec!["AGENTS.md".to_string()],
            tool_names: vec!["bash".to_string(), "read".to_string()],
            skill_names: vec![],
            prompt_names: vec![],
            extension_names: vec![],
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
        // Session-insight sections (tau `session_stats`). The token totals are
        // labeled "cumulative usage" (tau 7f4be2c), distinct from the footer's
        // active-context ratio.
        assert!(text.iter().any(|t| t.contains("Port session insights")));
        assert!(
            text.iter().any(|t| t.trim() == "cumulative usage"),
            "usage section is labeled cumulative: {text:?}"
        );
        assert!(text.iter().any(|t| t.contains("2 turns, 5 tool calls")));
        assert!(
            text.iter()
                .any(|t| t.contains("12.5k in, 800 out") && t.contains("~$0.008"))
        );
        assert!(text.iter().any(|t| t.contains("No extensions")));
    }

    #[test]
    fn untitled_session_falls_back_to_placeholder() {
        let theme = get_tui_theme(TuiThemeName::TauDark);
        let mut i = info();
        i.session_title = None;
        i.estimated_cost = None;
        let lines = build_sidebar_lines(&i, &theme);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|t| t.contains("Untitled session")));
        assert!(text.iter().any(|t| t.contains("$N/A")));
    }

    #[test]
    fn usage_and_cost_formatting_matches_tau() {
        assert_eq!(compact_usage_count(999), "999");
        assert_eq!(compact_usage_count(1_000), "1k");
        assert_eq!(compact_usage_count(12_500), "12.5k");
        assert_eq!(compact_usage_count(2_000_000), "2m");
        assert_eq!(compact_usage_count(2_500_000), "2.5m");
        assert_eq!(format_cost(0.005), "$0.005");
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(plural(1, "turn"), "turn");
        assert_eq!(plural(0, "turn"), "turns");
    }
}
