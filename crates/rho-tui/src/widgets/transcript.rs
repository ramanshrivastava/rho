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

use crate::motion::{self, MotionCaps};
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
/// the branches in [`build_chat_item_lines`] / [`visible_chat_text`]; the per-item
/// resolved invocation (`resolve_tool_invocation`, hashed in the loop) covers the
/// whole-second elapsed timer on a still-executing tool row, so an executing turn
/// re-renders on each timer tick without the timer ever going stale; `theme.name`
/// stands in for the whole palette (name ↔ colors 1:1).
fn transcript_fingerprint(state: &TuiState, theme: &TuiTheme, width: u16) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut h);
    theme.name.as_str().hash(&mut h);
    state.show_tool_results.hash(&mut h);
    state.show_thinking.hash(&mut h);
    state.assistant_buffer.hash(&mut h);
    for item in &state.items {
        std::mem::discriminant(&item.role).hash(&mut h);
        item.text.hash(&mut h);
        item.tool_result_text.hash(&mut h);
        item.update_text.hash(&mut h);
        item.always_show_tool_result.hash(&mut h);
        // Hash the *resolved* invocation for an executing tool row: it folds in
        // the whole-second elapsed timer (`started_at.elapsed()`), which advances
        // the "(Ns)" suffix once per second so a mid-turn redraw reflects the new
        // value. `None` for settled/non-tool rows, so this is cheap for everything
        // but the one running tool.
        state.resolve_tool_invocation(item).hash(&mut h);
    }
    // Not hashed: `tool_name` / `tool_arguments` / `custom_type` / `details`.
    // They only reach the render through the extension resolvers
    // (`tool_call_renderer` / `custom_renderer`), which are never installed
    // before M7 — today those items fall back to `item.text` (already hashed).
    // They are also set once when an item is created and never mutated, so any
    // change to them arrives as a new item, i.e. a new sequence with a different
    // hash. When M7 wires the resolvers this fingerprint must fold them in.
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

/// Whether to show the rho welcome splash: an empty transcript on a **fresh,
/// idle** session. Gated on `!running` so it never lingers after the user
/// submits the first prompt while a slow provider (or pre-prompt work like
/// auto-compaction) has not yet produced the first event.
#[must_use]
pub fn should_show_splash(state: &TuiState) -> bool {
    !state.running && transcript_is_empty(state)
}

/// The committed benchmark record set, baked in at build time so the splash's
/// benchmark brag cites the same numbers the repo committed (there is no
/// `dev-notes/` beside an installed binary to read at runtime). Parsed lazily by
/// [`bench_brag_line`]; a malformed/edited file degrades to no brag line.
const BENCHMARKS_JSON: &str = include_str!("../../../../dev-notes/benchmarks.json");

/// The heritage lineage stages: glyph + language label, in ancestry order.
const LINEAGE: [(&str, &str); 3] = [
    ("π", "Pi·TypeScript"),
    ("τ", "tau·Python"),
    ("ρ", "rho·Rust"),
];

/// The name "rho", written across scripts for the splash: Greek, Japanese
/// (katakana), Hindi (Devanagari). A quiet, cool multi-script flourish under the
/// mark.
const NAME_IN_SCRIPTS: [&str; 3] = ["ρο", "ロー", "रो"];

/// Rotating "did you know" heritage facts (task #45 welcome tips).
const DID_YOU_KNOW: [&str; 4] = [
    "ρ reads and writes τ's exact session files",
    "π is TypeScript, τ is Python, ρ is Rust",
    "ρ cold-starts in a few ms — no interpreter to boot",
    "the rho theme is rust-oxide over warm parchment",
];

/// The one-line hints row (Claude-Code-style getting-started), built from the
/// active keybindings so a user with customized `TuiSettings::keybindings` sees
/// the keys they actually bound (the `/` and `!cmd` prefixes are literal syntax,
/// not bindings).
fn splash_hints_row(kb: &crate::theme::TuiKeybindings) -> String {
    use crate::widgets::footer::key_hint;
    format!(
        "/ commands  ·  {} model  ·  {} sessions  ·  !cmd shell  ·  {} quit",
        key_hint(&kb.model_cycle),
        key_hint(&kb.session_picker),
        key_hint(&kb.quit),
    )
}

/// Frames each lineage glyph stays "active" before the oxidation marches on.
const LINEAGE_STEP_FRAMES: usize = 4;
/// Frames each "did you know" fact holds before rotating.
const FACT_STEP_FRAMES: usize = 30;

/// Render the rho welcome splash into `area`, filling the ENTIRE pane with the
/// theme background (no color seam) and centering the heritage block: the ρ mark,
/// the animated π → τ → ρ lineage, a one-line pitch, a real benchmark brag pulled
/// from the committed `benchmarks.json`, the hints row, and a rotating heritage
/// fact. An owner-sanctioned identity divergence (tau shows a blank transcript on
/// a fresh session). The lineage's active glyph oxidizes/brightens marching
/// π→τ→ρ while `caps` allow motion; otherwise it settles, bright, on ρ.
pub fn render_splash(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    theme: &TuiTheme,
    keybindings: &crate::theme::TuiKeybindings,
    frame_idx: usize,
    caps: MotionCaps,
) {
    let bg = parse_color(&theme.transcript_background)
        .map_or_else(Style::default, |color| Style::default().bg(color));

    // 1. Fill the WHOLE pane with the theme background first — this is the fix for
    //    the "half-screen theme" seam (the centered block used to leave the top
    //    padding at the default terminal background).
    frame.render_widget(ratatui::widgets::Block::default().style(bg), area);

    let accent = parse_style(&theme.accent).add_modifier(ratatui::style::Modifier::BOLD);
    let muted = parse_style(&theme.muted_text);
    let heading = parse_style(&theme.markdown_heading);

    // 2. Build the centered heritage block.
    let mark_style = if caps.animated() {
        Style::default()
            .fg(motion::oxide_ramp(
                0.4 + 0.5 * motion::throb01(frame_idx, motion::THROB_PERIOD_FRAMES),
            ))
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        accent
    };

    let bench = bench_brag_line();
    // Rotate the fact only when motion is allowed; a reduced-motion / non-truecolor
    // splash holds on the first fact so nothing shifts under the user.
    let fact_index = if caps.animated() {
        (frame_idx / FACT_STEP_FRAMES) % DID_YOU_KNOW.len()
    } else {
        0
    };
    let fact = DID_YOU_KNOW[fact_index];

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("ρ", mark_style)),
        Line::from(name_scripts_spans(frame_idx, caps, theme)),
        Line::default(),
        Line::from(lineage_spans(frame_idx, caps, theme)),
        Line::default(),
        Line::from(Span::styled("a Rust coding agent, oxidized", heading)),
    ];
    if let Some(bench) = bench {
        lines.push(Line::from(bench_brag_spans(&bench, theme)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        splash_hints_row(keybindings),
        muted,
    )));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("· did you know — {fact}"),
        muted.add_modifier(ratatui::style::Modifier::ITALIC),
    )));

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

/// The name "rho" across scripts (Greek · Japanese · Hindi) as a quiet, sleek
/// flourish beneath the mark: each script glyph on a gentle oxide gradient (dim →
/// bright, cool → hot), thin-spaced with muted dot separators. A subtle
/// synchronized throb rides the whole line when motion is available.
fn name_scripts_spans(frame_idx: usize, caps: MotionCaps, theme: &TuiTheme) -> Vec<Span<'static>> {
    let muted = parse_style(&theme.muted_text);
    // A small brightness lift shared by all three scripts so they pulse together.
    let lift = if caps.animated() {
        0.15 * motion::throb01(frame_idx, motion::THROB_PERIOD_FRAMES)
    } else {
        0.0
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, script) in NAME_IN_SCRIPTS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", muted));
        }
        // Cool → hot across the three scripts (Greek dim, Japanese mid, Hindi hot).
        #[allow(clippy::cast_precision_loss)]
        let base = 0.35 + 0.25 * (i as f32);
        let color = if caps.truecolor {
            motion::oxide_ramp(base + lift)
        } else {
            ratatui::style::Color::Red
        };
        spans.push(Span::styled(
            (*script).to_string(),
            Style::default().fg(color),
        ));
    }
    spans
}

/// The animated π → τ → ρ lineage spans: the active stage oxidizes/brightens
/// (throbbing while animated), the rest stay muted; language labels trail each
/// glyph. Marches π→τ→ρ over time; settles bright on ρ under no-motion.
fn lineage_spans(frame_idx: usize, caps: MotionCaps, theme: &TuiTheme) -> Vec<Span<'static>> {
    let muted = parse_style(&theme.muted_text);
    let active = if caps.animated() {
        (frame_idx / LINEAGE_STEP_FRAMES) % LINEAGE.len()
    } else {
        LINEAGE.len() - 1 // settle on ρ
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (glyph, label)) in LINEAGE.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   →   ", muted));
        }
        let (glyph_style, label_style) = if i == active {
            let bright = if caps.animated() {
                motion::oxide_ramp(
                    0.55 + 0.45 * motion::throb01(frame_idx, motion::THROB_PERIOD_FRAMES),
                )
            } else {
                motion::oxide_ramp(0.75)
            };
            (
                Style::default()
                    .fg(bright)
                    .add_modifier(ratatui::style::Modifier::BOLD),
                Style::default().fg(bright),
            )
        } else {
            (
                muted.add_modifier(ratatui::style::Modifier::DIM),
                muted.add_modifier(ratatui::style::Modifier::DIM),
            )
        };
        spans.push(Span::styled((*glyph).to_string(), glyph_style));
        spans.push(Span::styled(format!(" {label}"), label_style));
    }
    spans
}

/// Style the benchmark brag line: the `ρ` and the `×N` ratios in accent, the
/// prose muted.
fn bench_brag_spans(text: &str, theme: &TuiTheme) -> Vec<Span<'static>> {
    // The whole line is short and cited from real data; keep it a single muted
    // span with the leading ρ in accent so the brag stays quiet, not shouty.
    let accent = parse_style(&theme.accent);
    let muted = parse_style(&theme.muted_text);
    if let Some(rest) = text.strip_prefix("ρ ") {
        vec![
            Span::styled("ρ ", accent),
            Span::styled(rest.to_string(), muted),
        ]
    } else {
        vec![Span::styled(text.to_string(), muted)]
    }
}

/// Build the benchmark brag line from the committed benchmark records, e.g.
/// `ρ · ~302× faster cold start than τ · ~21× lighter`. Returns `None` if the
/// baked-in JSON can't be parsed or lacks the records to compute a ratio (so the
/// splash simply omits the line — graceful degradation).
#[must_use]
pub fn bench_brag_line() -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(BENCHMARKS_JSON).ok()?;
    let records = value.get("records")?.as_array()?;

    let cold = cold_start_ratio(records);
    let mem = memory_ratio(records);
    match (cold, mem) {
        (Some(cold), Some(mem)) => Some(format!(
            "ρ · ~{cold}× faster cold start than τ · ~{mem}× lighter"
        )),
        (Some(cold), None) => Some(format!("ρ · ~{cold}× faster cold start than τ")),
        (None, Some(mem)) => Some(format!("ρ · ~{mem}× lighter than τ")),
        (None, None) => None,
    }
}

/// The rho-vs-tau cold-start speedup, preferring the purest launch variant.
fn cold_start_ratio(records: &[serde_json::Value]) -> Option<u64> {
    for variant in ["version-direct", "version", "0ms"] {
        let rho = bench_mean(records, "cold_start", "rho", variant);
        let tau = bench_mean(records, "cold_start", "tau", variant);
        if let (Some(rho), Some(tau)) = (rho, tau) {
            if rho > 0.0 {
                return Some(ratio_round(tau / rho));
            }
        }
    }
    None
}

/// The rho-vs-tau memory-footprint ratio at the smallest turn count.
fn memory_ratio(records: &[serde_json::Value]) -> Option<u64> {
    let rho = memory_rss_bytes(records, "rho", 1)?;
    let tau = memory_rss_bytes(records, "tau", 1)?;
    if rho > 0.0 {
        Some(ratio_round(tau / rho))
    } else {
        None
    }
}

fn bench_mean(
    records: &[serde_json::Value],
    family: &str,
    impl_name: &str,
    variant: &str,
) -> Option<f64> {
    records.iter().find_map(|r| {
        (r.get("family")?.as_str()? == family
            && r.get("impl")?.as_str()? == impl_name
            && r.get("variant")?.as_str()? == variant)
            .then(|| r.get("mean_ms")?.as_f64())
            .flatten()
    })
}

fn memory_rss_bytes(records: &[serde_json::Value], impl_name: &str, turns: u64) -> Option<f64> {
    records.iter().find_map(|r| {
        (r.get("family")?.as_str()? == "memory_rss"
            && r.get("impl")?.as_str()? == impl_name
            && r.get("turns")?.as_u64()? == turns)
            .then(|| r.get("peak_rss_bytes")?.as_f64())
            .flatten()
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ratio_round(ratio: f64) -> u64 {
    ratio.round().max(0.0) as u64
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
        // A still-executing tool keeps a static, status-colored marker (the tool
        // role's border color) rather than relying on the removed animated spinner
        // (tau fd327d0).
        return theme
            .role_style("tool")
            .map_or(body, |r| parse_style(&r.border));
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

/// `_render_transcript_tool_invocation`: split `→ name args` / `$ cmd` and apply
/// the status accent to the tool **name and argument tail**, keeping only the
/// `→ `/`$` marker in the body style.
///
/// rho's ratatui transcript is tau's *selectable plain-text* path, so it mirrors
/// `_render_transcript_tool_invocation` (name → accent), not the mounted-widget
/// `_render_tool_invocation` (name → body). This matters for a still-executing
/// tool: `tool_accent_style` returns the tool border color, and — because rho's
/// tool `border` differs from its `body` (unlike tau-dark, where they coincide)
/// — the status color must reach the name too, or a pending `→ read README.md`
/// would leave `→ read` in the plain body and colorize only ` README.md`, which
/// reads as a half-applied spinner replacement (tau fd327d0).
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
        spans.push(Span::styled(name.to_string(), accent_style));
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
    fn splash_shows_only_on_a_fresh_idle_session() {
        let mut state = TuiState::new();
        // Fresh + idle → splash.
        assert!(should_show_splash(&state));
        // A pending turn (running set before the first event) suppresses it, so
        // the splash never lingers after the user submits the first prompt.
        state.running = true;
        assert!(!should_show_splash(&state));
        state.running = false;
        // Any content suppresses it (via items or a streaming buffer).
        state.add_item(ChatItemRole::User, "hi".to_string());
        assert!(!should_show_splash(&state));
        state.items.clear();
        state.assistant_buffer = "streaming…".to_string();
        assert!(!should_show_splash(&state));
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
    fn code_fence_keeps_theme_background_across_builtin_themes() {
        // tau f025e1d added cross-theme coverage that fenced code keeps the
        // configured background — in tau a Textual `MarkdownFence:light` CSS rule
        // used to strip it in the light theme. rho's ratatui path applies
        // `markdown_code_block_background` directly, so every built-in theme
        // (light included) must render the fence with that exact background.
        for name in crate::theme::BUILTIN_TUI_THEME_NAMES {
            let theme = crate::theme::get_tui_theme(
                crate::theme::TuiThemeName::parse(name).expect("built-in theme name"),
            );
            let body = s("#d8dee9");
            let lines = code_block_lines("x = 1", &theme, body, 40);
            let expected = parse_color(&theme.markdown_code_block_background);
            assert!(
                expected.is_some(),
                "{name}: theme background must be a real color"
            );
            assert_eq!(
                lines[0].style.bg, expected,
                "{name}: code fence dropped the themed background"
            );
        }
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
    fn pending_tool_accent_uses_tool_border_marker() {
        // tau fd327d0: a still-executing tool row (no result yet) keeps a static,
        // status-colored marker — the tool role's border color — instead of the
        // removed animated spinner. Previously this fell back to the plain body.
        let theme = crate::theme::tau_dark_theme();
        let body = s("#d8dee9");
        let pending = ChatItem::new(ChatItemRole::Tool, "→ read README.md".into());
        let accent = tool_accent_style(&pending, &theme, body);
        let expected = parse_style(&theme.role_style("tool").expect("tool role").border);
        assert_eq!(accent, expected);
        assert_ne!(
            accent, body,
            "pending tool must not fall back to body style"
        );
        // Once a result lands the accent switches to success/error coloring.
        let mut done = ChatItem::new(ChatItemRole::Tool, "→ read README.md".into());
        done.tool_result_text = Some("✓ read\nok".into());
        assert_ne!(tool_accent_style(&done, &theme, body), expected);
    }

    #[test]
    fn pending_tool_invocation_colors_name_and_args_not_marker() {
        // tau `test_pending_tool_invocation_uses_tool_accent_color`: in the
        // selectable transcript path (`_render_transcript_tool_invocation`) the
        // status accent covers the tool NAME and the argument tail, while the
        // `→ ` marker stays in the body style. rho's tool border differs from its
        // body, so this is what makes the whole pending invocation read as
        // status-colored rather than only the trailing filename.
        let body = s("#cbd5e1");
        let accent = s("#8a7a52"); // stand-in for the tool border (pending accent)
        let lines = render_tool_invocation("→ read README.md", body, accent, 80);
        let spans = &lines[0].spans;
        // Reconstruct (content, style) triples for the three segments.
        let marker = spans
            .iter()
            .find(|sp| sp.content.as_ref() == "→ ")
            .expect("marker span");
        let name = spans
            .iter()
            .find(|sp| sp.content.as_ref() == "read")
            .expect("name span");
        let args = spans
            .iter()
            .find(|sp| sp.content.as_ref() == " README.md")
            .expect("args span");
        assert_eq!(marker.style, body, "marker keeps the body style");
        assert_eq!(name.style, accent, "tool name carries the status accent");
        assert_eq!(
            args.style, accent,
            "argument tail carries the status accent"
        );
        assert_ne!(
            accent, body,
            "accent must differ from body for this to matter"
        );
    }

    #[test]
    fn resolve_invocation_appends_timer_without_spinner() {
        // tau fd327d0: a long-running tool row shows the elapsed timer appended to
        // its plain invocation — no braille spinner glyph stands in for the marker.
        let mut item = ChatItem::new(ChatItemRole::Tool, "→ agent · Summarize codebase".into());
        item.started_at = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(83))
                .unwrap(),
        );
        let state = TuiState::new();
        let resolved = state
            .resolve_tool_invocation(&item)
            .expect("pending tool resolves to a timed invocation");
        assert_eq!(resolved, "→ agent · Summarize codebase (1m 23s)");
        // No braille spinner frame leaked into the marker.
        for frame in ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] {
            assert!(!resolved.contains(frame), "spinner frame {frame} leaked");
        }
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
