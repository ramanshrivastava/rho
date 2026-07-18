//! Streaming transcript render layer (port of tau `widgets.TranscriptView` +
//! `render_chat_item` and its body renderers).
//!
//! Each [`crate::state::ChatItem`] becomes a Toad-inspired transcript block: a
//! one-column colored left gutter (`▌`) followed by the role's body text,
//! wrapped to the viewport width. The block formatters in
//! [`crate::state`] are reused verbatim, so tool/status/error rows read
//! byte-identically to tau; only the visual block (gutter + body styling) and
//! the markdown body rendering are re-derived for ratatui.
//!
//! Markdown rendering is a self-contained, dependency-free Rust renderer
//! covering the structures tau's assistant output actually uses — headings,
//! fenced code blocks (themed background), bullet lists, inline code, bold, and
//! links. Per-token syntax highlighting (tau's pygments `Syntax`) is deferred:
//! code blocks render with tau's `markdown_code_block_background` only. See
//! `dev-notes/phase-5.md` for the deferral ledger.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::{ChatItem, ChatItemRole, TuiState};
use crate::theme::TuiTheme;
use crate::widgets::style::{RoleStyles, chat_role_styles, parse_color, parse_style};

/// The left gutter marker tau renders beside each transcript block.
const GUTTER_BAR: &str = "▌";

/// A memo over [`build_transcript_lines`], keyed by a cheap fingerprint of every
/// input that affects the rendered output (item contents, the toggles, the
/// active spinner, the theme, and the width). The transcript is otherwise
/// rebuilt from scratch on *every* frame — each 150 ms spinner tick during a run
/// and each keystroke while composing — which is O(transcript) markdown parsing
/// and word wrapping (tens of milliseconds on a long history, per the
/// `transcript_render` bench). With the cache a frame that changed nothing (idle
/// typing, an idle tick between deltas) reuses the prior render for the cost of
/// one hash. Correctness comes entirely from the fingerprint: whenever the
/// output could differ, one hashed input differs, so there is no manual
/// invalidation to drift out of sync.
#[derive(Default)]
pub struct TranscriptCache {
    key: Option<u64>,
    lines: Vec<Line<'static>>,
}

impl TranscriptCache {
    /// The rendered transcript lines for the current state, rebuilding only when
    /// the fingerprint changed since the last call.
    pub fn lines(&mut self, state: &TuiState, theme: &TuiTheme, width: u16) -> &[Line<'static>] {
        let key = transcript_fingerprint(state, theme, width);
        if self.key != Some(key) {
            self.lines = build_transcript_lines(state, theme, width);
            self.key = Some(key);
        }
        &self.lines
    }
}

/// Hash every input `build_transcript_lines` reads. The per-item fields mirror
/// the branches in [`build_chat_item_lines`] / [`visible_chat_text`]; the global
/// `tool_spinner` covers the live spinner + elapsed timer applied to any
/// still-executing tool row (so an executing turn correctly re-renders each
/// tick), and `theme.name` stands in for the whole palette (name ↔ colors 1:1).
fn transcript_fingerprint(state: &TuiState, theme: &TuiTheme, width: u16) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut h);
    theme.name.as_str().hash(&mut h);
    state.show_tool_results.hash(&mut h);
    state.show_thinking.hash(&mut h);
    state.assistant_buffer.hash(&mut h);
    state.tool_spinner.hash(&mut h);
    for item in &state.items {
        std::mem::discriminant(&item.role).hash(&mut h);
        item.text.hash(&mut h);
        item.tool_result_text.hash(&mut h);
        item.update_text.hash(&mut h);
        item.always_show_tool_result.hash(&mut h);
    }
    h.finish()
}

/// Placeholder shown in place of a hidden thinking block (tau
/// `_HIDDEN_THINKING_PLACEHOLDER`). Consecutive hidden thinking blocks collapse
/// to a single placeholder.
pub const HIDDEN_THINKING_PLACEHOLDER: &str = "Thinking… Press Ctrl+T to show thinking tokens.";

/// Build every transcript line for the current state at `width` columns.
///
/// The returned lines include each item's colored gutter and wrapped body,
/// separated by a blank line, matching tau's `Padding((1, 1, 1, 0), …)` block
/// spacing at a 1-line resolution. The app layer renders these (e.g. via
/// [`render_transcript`]) and applies its own scroll offset.
#[must_use]
pub fn build_transcript_lines(
    state: &TuiState,
    theme: &TuiTheme,
    width: u16,
) -> Vec<Line<'static>> {
    let inner_width = inner_text_width(width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_any = false;
    // tau `TranscriptView.render_state`: when thinking is hidden, a run of
    // consecutive thinking items collapses to a SINGLE placeholder block
    // (`_HIDDEN_THINKING_PLACEHOLDER`); any non-thinking item resets the run.
    let mut hidden_thinking_placeholder = false;
    for item in &state.items {
        if item.role == ChatItemRole::Thinking && !state.show_thinking {
            if !hidden_thinking_placeholder {
                if rendered_any {
                    lines.push(Line::default());
                }
                let placeholder = ChatItem::new(
                    ChatItemRole::Thinking,
                    HIDDEN_THINKING_PLACEHOLDER.to_string(),
                );
                lines.extend(build_chat_item_lines(
                    &placeholder,
                    state,
                    theme,
                    inner_width,
                ));
                rendered_any = true;
                hidden_thinking_placeholder = true;
            }
            continue;
        }
        hidden_thinking_placeholder = false;
        if rendered_any {
            lines.push(Line::default());
        }
        let mut block = build_chat_item_lines(item, state, theme, inner_width);
        lines.append(&mut block);
        rendered_any = true;
    }
    if !state.assistant_buffer.is_empty() {
        if rendered_any {
            lines.push(Line::default());
        }
        let mut item = ChatItem::new(ChatItemRole::Assistant, state.assistant_buffer.clone());
        // A streaming assistant block has no tool result; render it like a normal
        // assistant turn so the gutter + body styling matches.
        item.always_show_tool_result = false;
        lines.extend(build_chat_item_lines(&item, state, theme, inner_width));
    }
    lines
}

/// Whether the transcript is empty (no items, nothing streaming) — the cue to
/// show the rho welcome splash instead of a blank pane.
#[must_use]
pub fn transcript_is_empty(state: &TuiState) -> bool {
    state.items.is_empty() && state.assistant_buffer.is_empty()
}

/// Render the rho welcome splash into `area`, vertically + horizontally centered:
/// the ρ mark, the π → τ → ρ lineage, and a one-line hint. A sanctioned identity
/// divergence (tau shows a blank transcript on a fresh session).
pub fn render_splash(frame: &mut Frame, area: ratatui::layout::Rect, theme: &TuiTheme) {
    let accent = parse_style(&theme.accent).add_modifier(ratatui::style::Modifier::BOLD);
    let muted = parse_style(&theme.muted_text);
    let bg = parse_color(&theme.transcript_background)
        .map_or_else(Style::default, |color| Style::default().bg(color));

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("ρ", accent)),
        Line::default(),
        Line::from(Span::styled("π → τ → ρ", muted)),
        Line::default(),
        Line::from(Span::styled("rho", accent)),
        Line::from(Span::styled("a Rust port of tau", muted)),
        Line::default(),
        Line::from(Span::styled(
            "Type a message, /command, or !shell to begin.",
            muted,
        )),
    ];

    // Vertically center: pad the top by half the slack (clamped so a short pane
    // still shows the mark from the top).
    let slack = (area.height as usize).saturating_sub(lines.len());
    let top_pad = u16::try_from(slack / 2).unwrap_or(0);
    let inner = ratatui::layout::Rect {
        y: area.y.saturating_add(top_pad),
        height: area.height.saturating_sub(top_pad),
        ..area
    };
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .style(bg),
        inner,
    );
}

/// Render the whole transcript into `area` from the top (no scroll), clipped by
/// the area. The app layer owns scroll/follow behavior.
pub fn render_transcript(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &TuiState,
    theme: &TuiTheme,
) {
    let lines = build_transcript_lines(state, theme, area.width);
    let bg = parse_color(&theme.transcript_background).unwrap_or(ratatui::style::Color::Reset);
    let paragraph = Paragraph::new(lines).style(Style::default().bg(bg));
    frame.render_widget(paragraph, area);
}

/// Body text width = total width minus the gutter column and the right padding
/// (tau's block padding is `(0, 1, 0, 1)`).
fn inner_text_width(width: u16) -> usize {
    width.saturating_sub(2) as usize
}

/// Build the gutter + wrapped-body lines for one chat item.
fn build_chat_item_lines(
    item: &ChatItem,
    state: &TuiState,
    theme: &TuiTheme,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let styles = chat_item_styles(item, theme);
    let body = build_chat_body_lines(item, state, theme, styles.body, inner_width);

    let bar_style = styles.border;
    let blank_gutter = Span::styled(" ", styles.body);
    let mut lines = Vec::with_capacity(body.len() + 1);
    for (i, mut body_line) in body.into_iter().enumerate() {
        let gutter = if i == 0 {
            Span::styled(GUTTER_BAR, bar_style)
        } else {
            blank_gutter.clone()
        };
        // Preserve body styling as the line's base style so trailing cells (the
        // right padding) fill with the body background, matching tau's block fill.
        let base = body_line.style;
        let mut spans = Vec::with_capacity(body_line.spans.len() + 1);
        spans.push(gutter);
        spans.append(&mut body_line.spans);
        body_line.spans = spans;
        body_line.style = base;
        lines.push(body_line);
    }
    if lines.is_empty() {
        // Even an empty body (rare) keeps the gutter so the block is visible.
        lines.push(Line::styled(format!("{GUTTER_BAR} "), bar_style));
    }
    lines
}

/// Resolve the border + body styles for a chat item, applying tau's tool
/// success/error border override (`_chat_item_role_style`).
fn chat_item_styles(item: &ChatItem, theme: &TuiTheme) -> RoleStyles {
    let base = chat_role_styles(theme, item.role);
    if item.role == ChatItemRole::Tool {
        if let Some(result) = &item.tool_result_text {
            let body = base.body;
            if result.starts_with('✓') {
                return RoleStyles {
                    border: parse_style(&tool_success_color(theme))
                        .fg(parse_color(&tool_success_color(theme)).unwrap()),
                    body,
                };
            }
            if result.starts_with('✗') {
                return RoleStyles {
                    border: parse_style("#ff4f4f"),
                    body,
                };
            }
        }
    }
    base
}

fn tool_success_color(theme: &TuiTheme) -> String {
    if theme.name == crate::theme::TuiThemeName::TauLight {
        "#166534".into()
    } else {
        "#9cffb1".into()
    }
}

/// The accent style applied to a tool invocation's argument tail on success/error
/// (`_tool_accent_style` + `_tool_success_style`/`_tool_error_style`).
fn tool_accent_style(item: &ChatItem, theme: &TuiTheme, body: Style) -> Style {
    if item.role != ChatItemRole::Tool {
        return body;
    }
    let Some(result) = &item.tool_result_text else {
        return body;
    };
    if result.starts_with('✓') {
        let color = tool_success_color(theme);
        if theme.name == crate::theme::TuiThemeName::TauLight {
            parse_style(&color)
        } else {
            parse_style(&format!("{color} on #000000"))
        }
    } else if result.starts_with('✗') {
        if theme.name == crate::theme::TuiThemeName::TauLight {
            parse_style(
                &theme
                    .role_style("error")
                    .map_or_else(|| "#b91c1c".into(), |r| r.border.clone()),
            )
        } else {
            parse_style("#ff4f4f on #000000")
        }
    } else {
        body
    }
}

/// Build the (un-guttered) wrapped body lines for one chat item.
fn build_chat_body_lines(
    item: &ChatItem,
    state: &TuiState,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    if item.role == ChatItemRole::Tool {
        return build_tool_body_lines(item, state, theme, body_style, inner_width);
    }
    let visible = visible_chat_text(item, state);
    render_role_body(&visible, item.role, theme, body_style, inner_width)
}

/// `_render_tool_chat_body`: the invocation line (with accent) plus, when
/// expanded, a blank line and the result body.
fn build_tool_body_lines(
    item: &ChatItem,
    state: &TuiState,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let accent = tool_accent_style(item, theme, body_style);
    let invocation = resolve_invocation_text(item, state);
    let mut lines = render_tool_invocation(&invocation, body_style, accent, inner_width);
    if state.show_tool_results || item.always_show_tool_result {
        if let Some(result) = &item.tool_result_text {
            lines.push(Line::default());
            lines.extend(render_role_body(
                result,
                item.role,
                theme,
                body_style,
                inner_width,
            ));
        }
    }
    // `_visible_chat_text`: while a tool is still executing (no final result)
    // any recorded live progress (`record_tool_update`) is surfaced as a
    // trailing `… {update}` block, regardless of the expand toggle.
    if item.tool_result_text.is_none() {
        if let Some(update) = &item.update_text {
            lines.push(Line::default());
            lines.extend(render_role_body(
                &format!("… {update}"),
                item.role,
                theme,
                body_style,
                inner_width,
            ));
        }
    }
    lines
}

/// The invocation line for a tool item, applying the live spinner/timer.
fn resolve_invocation_text(item: &ChatItem, state: &TuiState) -> String {
    if let Some(resolved) = state.resolve_tool_invocation(item) {
        return resolved;
    }
    item.text.clone()
}

/// `_visible_chat_text`: the text shown for an item given the expand toggles.
fn visible_chat_text(item: &ChatItem, state: &TuiState) -> String {
    match item.role {
        ChatItemRole::BranchSummary => {
            if state.show_tool_results {
                if let Some(summary) = &item.tool_result_text {
                    return format!("**Branch Summary**\n\n{summary}");
                }
            }
            item.text.clone()
        }
        ChatItemRole::CompactionSummary => {
            if state.show_tool_results {
                if let Some(summary) = &item.tool_result_text {
                    return format!("**Compaction Summary**\n\n{summary}");
                }
            }
            item.text.clone()
        }
        ChatItemRole::Tool | ChatItemRole::Skill => {
            let base = item.text.clone();
            if state.show_tool_results {
                if let Some(result) = &item.tool_result_text {
                    return format!("{base}\n\n{result}");
                }
            }
            if let Some(update) = &item.update_text {
                if item.tool_result_text.is_none() {
                    return format!("{base}\n\n… {update}");
                }
            }
            base
        }
        _ => item.text.clone(),
    }
}

/// `_render_tool_invocation`: split `→ name args` / `$ cmd` and color the tail.
fn render_tool_invocation(
    text: &str,
    body_style: Style,
    accent_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let (prefix, name, remainder) = split_tool_invocation(text);
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), body_style));
    }
    if !name.is_empty() {
        spans.push(Span::styled(name.to_string(), body_style));
    }
    if !remainder.is_empty() {
        spans.push(Span::styled(remainder.to_string(), accent_style));
    }
    wrap_spans(spans, body_style, inner_width)
}

/// `_split_tool_invocation`.
fn split_tool_invocation(text: &str) -> (&str, &str, &str) {
    if let Some(rest) = text.strip_prefix("→ ") {
        let (name, sep_remainder) = rest.split_once(' ').unwrap_or((rest, ""));
        let remainder = if sep_remainder.is_empty() { "" } else { rest }
            .get(name.len()..)
            .unwrap_or("");
        return ("→ ", name, remainder);
    }
    if text.starts_with("$ ") {
        // tau keeps the space after the `$` marker so the tail reads `$ cmd`,
        // not `$cmd`: `_split_tool_invocation` returns `("$", "", text[1:])`.
        return ("$", "", &text[1..]);
    }
    let (name, sep_remainder) = text.split_once(' ').unwrap_or((text, ""));
    let remainder = if sep_remainder.is_empty() {
        ""
    } else {
        text.get(name.len()..).unwrap_or("")
    };
    ("", name, remainder)
}

/// `_render_chat_body`: dispatch by role to patch / fenced / markdown / plain.
fn render_role_body(
    text: &str,
    role: ChatItemRole,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    if let Some(patch_lines) = render_patch_body(text, theme, body_style, inner_width) {
        return patch_lines;
    }
    if matches!(
        role,
        ChatItemRole::Assistant | ChatItemRole::Thinking | ChatItemRole::Status
    ) {
        if has_unclosed_fence(text) {
            return plain_lines(text, body_style, inner_width);
        }
        return markdown_lines(text, theme, body_style, inner_width);
    }
    if let Some(fenced) = render_fenced_body(text, theme, body_style, inner_width) {
        return fenced;
    }
    plain_lines(text, body_style, inner_width)
}

/// `_render_patch_body`: a `Patch:\n` section renders the trailing diff as a
/// code block (themed background); the preamble is plain text.
fn render_patch_body(
    text: &str,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Option<Vec<Line<'static>>> {
    const MARKER: &str = "\nPatch:\n";
    let marker_index = text.find(MARKER)?;
    let patch = &text[marker_index + MARKER.len()..];
    if patch.trim().is_empty() {
        return None;
    }
    // tau builds `f"{before_patch}{marker.rstrip()}"` where `marker` is
    // `"\nPatch:\n"`, so the leading newline of the marker is preserved and the
    // preamble stays separated from the `Patch:` label.
    let before = format!("{}\nPatch:", &text[..marker_index]);
    let mut lines = plain_lines(&before, body_style, inner_width);
    lines.extend(code_block_lines(
        patch.trim_end_matches('\n'),
        theme,
        body_style,
        inner_width,
    ));
    Some(lines)
}

/// `_render_fenced_body`: render well-formed triple-backtick fences as code
/// blocks with plain text between them. Returns `None` if the fences are
/// malformed, so the caller falls back to plain text (matching tau).
fn render_fenced_body(
    text: &str,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Option<Vec<Line<'static>>> {
    if !text.contains("```") {
        return None;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor: usize = 0;
    let bytes = text.as_bytes();
    while cursor < bytes.len() {
        let Some(offset) = text[cursor..].find("```") else {
            append_plain(&mut lines, &text[cursor..], body_style, inner_width);
            break;
        };
        let fence_start = cursor + offset;
        // tau requires the fence to begin a line.
        let line_start = text[..fence_start].rfind('\n').map_or(0, |i| i + 1);
        if line_start != fence_start {
            return None;
        }
        let fence_line_end = text[fence_start..]
            .find('\n')
            .map_or(text.len(), |i| fence_start + i);
        if fence_line_end == text.len()
            && !text[fence_start..].ends_with('\n')
            && cursor >= text.len()
        {
            return None;
        }
        // Search for the closing fence AFTER the opening fence line's terminator.
        // Starting at `fence_line_end` lets an EMPTY block ("```\n```") match the
        // opening line's own newline as the closing fence, so `code` became a
        // reversed-range slice (`text[fence_line_end+1..closing]` with
        // `closing < fence_line_end+1`) and panicked. `get` also guards a fence
        // with no trailing newline.
        let code_start = fence_line_end + 1;
        let rest = text.get(code_start..)?;
        let rel = rest.find("\n```")?;
        let closing = code_start + rel; // '\n' before the closing fence; >= code_start
        append_plain(
            &mut lines,
            &text[cursor..fence_start],
            body_style,
            inner_width,
        );
        let _language = text[fence_start + 3..fence_line_end].trim_start();
        let code = &text[code_start..closing];
        lines.extend(code_block_lines(
            code.trim_end_matches('\n'),
            theme,
            body_style,
            inner_width,
        ));
        let after_closing = closing + "\n```".len();
        cursor = text[after_closing..]
            .find('\n')
            .map_or(text.len(), |i| after_closing + i + 1);
    }
    Some(lines)
}

fn append_plain(lines: &mut Vec<Line<'static>>, text: &str, body_style: Style, inner_width: usize) {
    if text.is_empty() {
        return;
    }
    lines.extend(plain_lines(
        text.trim_end_matches('\n'),
        body_style,
        inner_width,
    ));
}

/// `_plain_text`: a plain body block, wrapped.
fn plain_lines(text: &str, body_style: Style, inner_width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(Line::default().style(body_style));
            continue;
        }
        let spans = vec![Span::styled(raw.to_string(), body_style)];
        lines.extend(wrap_spans(spans, body_style, inner_width));
    }
    if lines.is_empty() {
        lines.push(Line::default().style(body_style));
    }
    lines
}

/// Render a fenced code block with tau's `markdown_code_block_background`.
fn code_block_lines(
    code: &str,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let bg = parse_color(&theme.markdown_code_block_background);
    let code_style = match bg {
        Some(bg) => body_style.bg(bg),
        None => body_style,
    };
    let max = inner_width.max(1);
    let mut lines = Vec::new();
    for raw in code.split('\n') {
        lines.push(Line::styled(truncate_to_width(raw, max), code_style));
    }
    if lines.is_empty() {
        lines.push(Line::styled(String::new(), code_style));
    }
    lines
}

/// Truncate `text` so it occupies at most `max` terminal cells, accumulating
/// display width (CJK/emoji are two cells wide) rather than character count so
/// a wide-char line never overflows the body width. Mirrors the hard-wrap loop
/// in [`wrap_spans`].
fn truncate_to_width(text: &str, max: usize) -> String {
    let mut buf = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max {
            return buf;
        }
        buf.push(ch);
        width += cw;
    }
    buf
}

/// `_has_unclosed_fence`.
fn has_unclosed_fence(text: &str) -> bool {
    let count = text.lines().filter(|line| line.starts_with("```")).count();
    count % 2 == 1
}

/// Lightweight markdown renderer for assistant/thinking/status bodies.
fn markdown_lines(
    text: &str,
    theme: &TuiTheme,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let heading_style =
        parse_style(&theme.markdown_heading).add_modifier(ratatui::style::Modifier::BOLD);
    let bullet_style = parse_style(&theme.markdown_bullet);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_fenced: Option<Vec<&str>> = None;

    for line in text.split_inclusive('\n') {
        let trimmed_end = line.trim_end_matches('\n');
        if let Some(_lang_and_rest) = trimmed_end.strip_prefix("```") {
            if in_fenced.is_some() {
                // closing fence
                if let Some(block) = in_fenced.take() {
                    lines.extend(code_block_lines(
                        &block.join("\n"),
                        theme,
                        body_style,
                        inner_width,
                    ));
                }
            } else {
                in_fenced = Some(Vec::new());
            }
            continue;
        }
        if let Some(block) = in_fenced.as_mut() {
            block.push(trimmed_end);
            continue;
        }
        push_markdown_line(
            &mut lines,
            trimmed_end,
            theme,
            body_style,
            heading_style,
            bullet_style,
            inner_width,
        );
    }
    if let Some(block) = in_fenced {
        // Unclosed fence inside markdown path: tau falls back to plain for the
        // whole block; we render what we buffered as a code block.
        lines.extend(code_block_lines(
            &block.join("\n"),
            theme,
            body_style,
            inner_width,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::default().style(body_style));
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn push_markdown_line(
    lines: &mut Vec<Line<'static>>,
    raw: &str,
    theme: &TuiTheme,
    body_style: Style,
    heading_style: Style,
    bullet_style: Style,
    inner_width: usize,
) {
    if raw.is_empty() {
        lines.push(Line::default().style(body_style));
        return;
    }
    // Heading: one or more leading `#` followed by a space.
    if let Some(rest) = heading_rest(raw) {
        let spans = inline_spans(rest, theme, body_style);
        for span_line in wrap_spans(spans, body_style, inner_width) {
            // restyle as heading
            let mut hl = span_line;
            hl.spans
                .iter_mut()
                .for_each(|s| s.style = heading_style.patch(s.style));
            lines.push(hl);
        }
        return;
    }
    // Bullet: `- `, `* `, `+ `.
    if let Some(rest) = strip_bullet(raw) {
        let mut spans = vec![Span::styled("• ".to_string(), bullet_style)];
        spans.extend(inline_spans(rest, theme, body_style));
        lines.extend(wrap_spans(spans, body_style, inner_width));
        return;
    }
    // Numbered list: `1. `.
    if let Some(rest) = strip_numbered(raw) {
        let prefix_len = raw.len() - rest.len();
        let prefix: String = raw[..prefix_len].to_string();
        let mut spans = vec![Span::styled(prefix, body_style)];
        spans.extend(inline_spans(rest, theme, body_style));
        lines.extend(wrap_spans(spans, body_style, inner_width));
        return;
    }
    let spans = inline_spans(raw, theme, body_style);
    lines.extend(wrap_spans(spans, body_style, inner_width));
}

fn heading_rest(raw: &str) -> Option<&str> {
    let mut hashes = 0;
    for ch in raw.chars() {
        if ch == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if (1..=6).contains(&hashes) {
        raw.get(hashes..)
            .and_then(|s| s.strip_prefix(' '))
            .map(str::trim_end)
    } else {
        None
    }
}

fn strip_bullet(raw: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = raw.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn strip_numbered(raw: &str) -> Option<&str> {
    let digits_end = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    if digits_end == 0 {
        // No leading digit: not a numbered list item.
        return None;
    }
    let rest = &raw[digits_end..];
    if rest.starts_with(". ") || rest.starts_with(") ") {
        Some(&rest[2..])
    } else {
        None
    }
}

/// Tokenize one source line into styled spans: inline code, bold, links, plain.
fn inline_spans(text: &str, theme: &TuiTheme, body_style: Style) -> Vec<Span<'static>> {
    let inline_code_style = parse_style(&theme.markdown_inline_code);
    let link_style = parse_style(&theme.markdown_link);
    let bold_style = body_style.add_modifier(ratatui::style::Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = text;
    break_simple_inline(
        &mut spans,
        &mut rest,
        theme,
        body_style,
        inline_code_style,
        link_style,
        bold_style,
    );
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), body_style));
    }
    spans
}

#[allow(clippy::too_many_arguments)]
fn break_simple_inline(
    spans: &mut Vec<Span<'static>>,
    rest: &mut &str,
    _theme: &TuiTheme,
    body_style: Style,
    inline_code_style: Style,
    link_style: Style,
    bold_style: Style,
) {
    loop {
        if rest.is_empty() {
            return;
        }
        let next_code = rest.find('`').filter(|&i| {
            rest[i + 1..].find('`').is_some_and(|close| {
                let between = &rest[i + 1..i + 1 + close];
                !between.is_empty()
            })
        });
        let next_bold = rest.find("**").filter(|&i| {
            rest[i + 2..]
                .find("**")
                .is_some_and(|j| i + 2 + j < rest.len())
        });
        let next_link = rest.find('[').filter(|&i| {
            rest[i..]
                .find("](")
                .is_some_and(|open| rest[i + open + 2..].find(')').is_some())
        });

        let earliest = [next_code, next_bold, next_link]
            .into_iter()
            .flatten()
            .min();

        let Some(at) = earliest else {
            spans.push(Span::styled((*rest).to_string(), body_style));
            *rest = "";
            return;
        };

        if at > 0 {
            let (before, after) = rest.split_at(at);
            spans.push(Span::styled(before.to_string(), body_style));
            *rest = after;
        }

        if Some(at) == next_code {
            // `code`
            let inner_start = 1;
            let close_rel = rest[inner_start..].find('`').unwrap();
            let code = &rest[inner_start..inner_start + close_rel];
            spans.push(Span::styled(code.to_string(), inline_code_style));
            *rest = &rest[inner_start + close_rel + 1..];
        } else if Some(at) == next_bold {
            // **bold**
            let inner_start = 2;
            let close_rel = rest[inner_start..].find("**").unwrap();
            let bold = &rest[inner_start..inner_start + close_rel];
            spans.push(Span::styled(bold.to_string(), bold_style));
            *rest = &rest[inner_start + close_rel + 2..];
        } else {
            // [label](url)
            let label_close = rest.find(']').unwrap();
            let label = &rest[1..label_close];
            spans.push(Span::styled(label.to_string(), link_style));
            let url_open = rest[label_close..].find("](").unwrap() + label_close;
            let url_close = rest[url_open + 2..].find(')').unwrap() + url_open + 2;
            *rest = &rest[url_close + 1..];
        }
    }
}

/// Wrap a sequence of styled spans to `inner_width` columns, breaking at spaces
/// inside long spans. Returns one [`Line`] per wrapped row.
fn wrap_spans(
    spans: Vec<Span<'static>>,
    body_style: Style,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let max = inner_width.max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    let flush = |current: &mut Vec<Span<'static>>, rows: &mut Vec<Line<'static>>| {
        if !current.is_empty() {
            let mut spans = std::mem::take(current);
            let style = spans.first().map_or(body_style, |s| s.style);
            rows.push(Line::from(std::mem::take(&mut spans)).style(style));
        }
    };

    for span in spans {
        let style = span.style;
        let text = span.content.to_string();
        if text.is_empty() {
            current.push(span);
            continue;
        }
        let width = unicode_width::UnicodeWidthStr::width(text.as_str());
        if current_width + width <= max {
            current_width += width;
            current.push(Span::styled(text, style));
            continue;
        }
        // Break the span at spaces.
        let words: Vec<&str> = text.split(' ').collect();
        let mut first = true;
        for word in words {
            let word_width = unicode_width::UnicodeWidthStr::width(word);
            let candidate = if first { word_width } else { word_width + 1 };
            if current_width + candidate > max && !current.is_empty() {
                flush(&mut current, &mut rows);
                current_width = 0;
            }
            if !first && !current.is_empty() {
                current.push(Span::styled(" ", style));
                current_width += 1;
            }
            if word_width > max {
                // Hard-wrap an over-long single word by character.
                let mut buf = String::new();
                let mut w = current_width;
                for ch in word.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if w + cw > max && !buf.is_empty() {
                        current.push(Span::styled(std::mem::take(&mut buf), style));
                        flush(&mut current, &mut rows);
                        w = 0;
                    }
                    buf.push(ch);
                    w += cw;
                }
                if !buf.is_empty() {
                    current.push(Span::styled(buf, style));
                    current_width = w;
                }
            } else {
                current.push(Span::styled(word.to_string(), style));
                current_width += word_width;
            }
            first = false;
        }
    }
    flush(&mut current, &mut rows);
    if rows.is_empty() {
        rows.push(Line::default().style(body_style));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(c: &str) -> Style {
        parse_style(c)
    }

    #[test]
    fn split_invocation_arrow() {
        assert_eq!(
            split_tool_invocation("→ read foo.rs"),
            ("→ ", "read", " foo.rs")
        );
    }

    #[test]
    fn split_invocation_dollar() {
        // The space after `$` is preserved so the invocation renders `$ ls -la`.
        assert_eq!(split_tool_invocation("$ ls -la"), ("$", "", " ls -la"));
    }

    #[test]
    fn render_invocation_dollar_keeps_space() {
        let body = s("#d8dee9");
        let accent = s("#9cffb1");
        let lines = render_tool_invocation("$ ls -la", body, accent, 60);
        let rendered: String = lines[0]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        assert_eq!(rendered, "$ ls -la");
    }

    #[test]
    fn split_invocation_bare_name() {
        assert_eq!(split_tool_invocation("custom arg"), ("", "custom", " arg"));
    }

    #[test]
    fn split_invocation_name_only() {
        assert_eq!(split_tool_invocation("custom"), ("", "custom", ""));
    }

    #[test]
    fn inner_text_width_subtracts_gutter_and_pad() {
        assert_eq!(inner_text_width(60), 58);
        assert_eq!(inner_text_width(1), 0);
    }

    #[test]
    fn visible_text_appends_tool_result_when_expanded() {
        let theme = crate::theme::tau_dark_theme();
        let _ = theme;
        let mut item = ChatItem::new(ChatItemRole::Tool, "→ read foo.rs".into());
        item.tool_result_text = Some("✓ read\nok".into());
        let mut state = TuiState::new();
        state.show_tool_results = true;
        let visible = visible_chat_text(&item, &state);
        assert_eq!(visible, "→ read foo.rs\n\n✓ read\nok");
    }

    #[test]
    fn visible_text_shows_update_placeholder() {
        let mut item = ChatItem::new(ChatItemRole::Tool, "→ bash $ ls".into());
        item.update_text = Some("running…".into());
        let state = TuiState::new();
        let visible = visible_chat_text(&item, &state);
        assert_eq!(visible, "→ bash $ ls\n\n… running…");
    }

    #[test]
    fn branch_summary_expanded() {
        let mut item = ChatItem::new(ChatItemRole::BranchSummary, "Branch summary".into());
        item.tool_result_text = Some("the summary".into());
        let mut state = TuiState::new();
        state.show_tool_results = true;
        assert_eq!(
            visible_chat_text(&item, &state),
            "**Branch Summary**\n\nthe summary"
        );
    }

    #[test]
    fn empty_code_fence_does_not_panic() {
        // Regression: an empty fence "```\n```" used to slice a reversed range
        // (`text[fence_line_end+1..closing]` with closing < that) and panic.
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9");
        // Direct: the malformed/empty fence yields None, never a panic.
        assert!(render_fenced_body("```\n```", &theme, body, 40).is_none());
        assert!(render_fenced_body("before\n```\n```", &theme, body, 40).is_none());
        // A well-formed fence still renders.
        assert!(render_fenced_body("```\ncode\n```", &theme, body, 40).is_some());
        // Via a full assistant body render (the real transcript path).
        let item = ChatItem::new(ChatItemRole::Assistant, "text\n```\n```\nmore".into());
        let _ = build_chat_item_lines(&item, &TuiState::new(), &theme, 40);
        // Via a tool RESULT containing an empty fence (the reported repro).
        let mut tool = ChatItem::new(ChatItemRole::Tool, "→ bash $ echo".into());
        tool.tool_result_text = Some("✓ bash\n```\n```".into());
        let mut state = TuiState::new();
        state.show_tool_results = true;
        let _ = build_chat_item_lines(&tool, &state, &theme, 40);
    }

    fn joined(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn transcript_cache_never_returns_stale_content() {
        // The memo must always equal a fresh build across every input that
        // affects the render: content, the thinking toggle (placeholder
        // collapse), and width. (A hit is a perf property; correctness is that
        // the cached bytes are never stale.)
        let theme = crate::theme::tau_dark_theme();
        let mut cache = crate::widgets::TranscriptCache::default();
        let mut state = TuiState::new();
        state.show_thinking = true;
        state.add_item(ChatItemRole::User, "hi".to_string());
        state.add_item(ChatItemRole::Thinking, "a thought".to_string());

        let a = cache.lines(&state, &theme, 60).to_vec();
        assert_eq!(a, build_transcript_lines(&state, &theme, 60));

        // Repeated call with no change: identical output (the hit path).
        assert_eq!(cache.lines(&state, &theme, 60), a.as_slice());

        // Toggling thinking flips to the collapsed placeholder — must invalidate.
        state.show_thinking = false;
        let b = cache.lines(&state, &theme, 60).to_vec();
        assert_eq!(b, build_transcript_lines(&state, &theme, 60));
        assert_ne!(a, b);

        // New content invalidates.
        state.add_item(ChatItemRole::Assistant, "answer".to_string());
        assert_eq!(
            cache.lines(&state, &theme, 60),
            build_transcript_lines(&state, &theme, 60).as_slice()
        );

        // A width change invalidates (wrapping differs).
        assert_eq!(
            cache.lines(&state, &theme, 20),
            build_transcript_lines(&state, &theme, 20).as_slice()
        );
    }

    #[test]
    fn hidden_thinking_collapses_to_single_placeholder() {
        // tau parity: with show_thinking=false a run of thinking items collapses
        // to ONE placeholder; visible thinking shows the real text.
        let theme = crate::theme::tau_dark_theme();
        let mut state = TuiState::new();
        state.add_item(ChatItemRole::User, "hi".to_string());
        state.add_item(ChatItemRole::Thinking, "first thought".to_string());
        state.add_item(ChatItemRole::Thinking, "second thought".to_string());
        state.add_item(ChatItemRole::Assistant, "answer".to_string());

        // Shown: real thinking text appears, no placeholder.
        state.show_thinking = true;
        let shown = joined(&build_transcript_lines(&state, &theme, 60));
        assert!(
            shown.iter().any(|l| l.contains("first thought")),
            "{shown:?}"
        );
        assert!(
            !shown
                .iter()
                .any(|l| l.contains(HIDDEN_THINKING_PLACEHOLDER)),
            "{shown:?}"
        );

        // Hidden: exactly one placeholder replaces the whole thinking run; the
        // real thinking text is gone; user + assistant survive.
        state.show_thinking = false;
        let hidden = joined(&build_transcript_lines(&state, &theme, 60));
        let placeholders = hidden
            .iter()
            .filter(|l| l.contains(HIDDEN_THINKING_PLACEHOLDER))
            .count();
        assert_eq!(placeholders, 1, "{hidden:?}");
        assert!(
            !hidden.iter().any(|l| l.contains("first thought")),
            "{hidden:?}"
        );
        assert!(
            !hidden.iter().any(|l| l.contains("second thought")),
            "{hidden:?}"
        );
        assert!(hidden.iter().any(|l| l.contains("answer")), "{hidden:?}");
    }

    #[test]
    fn plain_lines_wrap_long_text() {
        let body = s("#d8dee9");
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let lines = plain_lines(text, body, 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(unicode_width::UnicodeWidthStr::width(line_to_string(line).as_str()) <= 20);
        }
    }

    fn line_to_string(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn code_block_truncates_overlong_lines() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#cbd5e1 on #000000");
        let lines = code_block_lines("aaaaaaaaaaaaaaaaaaaaaaaaa", &theme, body, 10);
        assert_eq!(lines.len(), 1);
        let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered.chars().count(), 10);
    }

    #[test]
    fn markdown_renders_heading_bold_and_inline_code() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9 on #000000");
        let lines = markdown_lines("# Title\n\nplain with `code` here", &theme, body, 60);
        // heading line + blank + body line
        assert!(lines.len() >= 2);
        let heading: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(heading, "Title");
    }

    #[test]
    fn markdown_bullet_uses_bullet_marker() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9 on #000000");
        let lines = markdown_lines("- one\n- two", &theme, body, 60);
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first.starts_with("• "));
    }

    #[test]
    fn fenced_body_detects_code_block() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9 on #000000");
        let text = "intro\n```rust\nfn main() {}\n```\noutro";
        let fenced = render_fenced_body(text, &theme, body, 60).expect("fenced");
        // intro + code line + outro
        assert!(fenced.len() >= 3);
    }

    #[test]
    fn fenced_body_rejects_malformed() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9 on #000000");
        // fence not at line start
        assert!(render_fenced_body("foo ``` bar", &theme, body, 60).is_none());
    }

    #[test]
    fn patch_body_splits_diff() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#cbd5e1 on #000000");
        let text = "✓ edit\n\nbefore\nPatch:\n--- a\n+++ b";
        let lines = render_patch_body(text, &theme, body, 60).expect("patch");
        assert!(lines.len() >= 3);
    }

    #[test]
    fn empty_patch_returns_none() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#cbd5e1 on #000000");
        assert!(render_patch_body("✓ edit\n\nPatch:\n   ", &theme, body, 60).is_none());
    }

    #[test]
    fn patch_body_preserves_newline_before_marker() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#cbd5e1 on #000000");
        let text = "✓ edit\n\nbefore\nPatch:\n--- a\n+++ b";
        let lines = render_patch_body(text, &theme, body, 60).expect("patch");
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect())
            .collect();
        // `before` and `Patch:` stay on separate lines — not fused as `beforePatch:`.
        assert!(rendered.iter().any(|l| l == "before"), "{rendered:?}");
        assert!(rendered.iter().any(|l| l == "Patch:"), "{rendered:?}");
        assert!(
            !rendered.iter().any(|l| l.contains("beforePatch:")),
            "{rendered:?}"
        );
    }

    #[test]
    fn tool_body_renders_update_text_without_result() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9");
        let mut item = ChatItem::new(ChatItemRole::Tool, "$ ls -la".into());
        item.update_text = Some("running…".into());
        // No tool_result_text yet, and results collapsed.
        let state = TuiState::new();
        let lines = build_tool_body_lines(&item, &state, &theme, body, 60);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect())
            .collect();
        assert!(
            rendered.iter().any(|l| l.contains("… running…")),
            "{rendered:?}"
        );
    }

    #[test]
    fn tool_body_hides_update_once_result_present() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9");
        let mut item = ChatItem::new(ChatItemRole::Tool, "$ ls -la".into());
        item.update_text = Some("running…".into());
        item.tool_result_text = Some("✓ bash\nok".into());
        let state = TuiState::new();
        let lines = build_tool_body_lines(&item, &state, &theme, body, 60);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect())
            .collect();
        assert!(
            !rendered.iter().any(|l| l.contains("… running…")),
            "{rendered:?}"
        );
    }

    #[test]
    fn code_block_truncates_wide_chars_by_cell_width() {
        let theme = crate::theme::tau_dark_theme();
        let body = s("#cbd5e1 on #000000");
        // Ten CJK chars, each two cells wide; inner_width 10 → 5 chars, 10 cells.
        let wide = "你好世界你好世界你好";
        let lines = code_block_lines(wide, &theme, body, 10);
        assert_eq!(lines.len(), 1);
        let rendered: String = lines[0]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(
            unicode_width::UnicodeWidthStr::width(rendered.as_str()) <= 10,
            "width was {}",
            unicode_width::UnicodeWidthStr::width(rendered.as_str())
        );
        // Char-count truncation would have kept 10 chars (20 cells); width-based
        // truncation keeps 5.
        assert_eq!(rendered.chars().count(), 5);
    }
}
