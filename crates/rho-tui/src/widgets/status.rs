//! The compact session-info status line (port of tau
//! `widgets.render_compact_session_info` + its helpers).
//!
//! tau renders a two-column grid docked below the prompt: the left column shows
//! `cwd (git-branch)`, the right column shows `context-usage  provider:model
//! (thinking)`. There is no cost figure and no elapsed timer — token usage is the
//! only quantitative field (confirmed against `app.py`'s chrome).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::TuiTheme;
use crate::widgets::style::parse_style;

/// The session facts the status line needs, snapshotted from a `CodingSession`
/// so the render layer never borrows the live session (see `app.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    /// Working directory (for the left column + git branch lookup).
    pub cwd: PathBuf,
    /// Active provider name.
    pub provider_name: String,
    /// Active model id.
    pub model: String,
    /// Resolved thinking-level display (`_thinking_level`).
    pub thinking_display: String,
    /// Estimated tokens in the active context.
    pub context_token_estimate: i64,
    /// The provider's context window size.
    pub context_window_tokens: i64,
    /// The auto-compaction threshold, if enabled (denominator override).
    pub auto_compact_token_threshold: Option<i64>,
    /// The current git branch (cached; `--` if none). Snapshotted so the render
    /// layer never shells out mid-frame.
    pub git_branch: String,
}

/// Compact a raw token count the way tau's `_compact_token_count` does:
/// `<= 0 -> "0k"`, `< 1000 -> "<1k"`, else round-half-up to thousands + `k`.
#[must_use]
pub fn compact_token_count(value: i64) -> String {
    if value <= 0 {
        return "0k".to_string();
    }
    if value < 1000 {
        return "<1k".to_string();
    }
    format!("{}k", (value + 500) / 1000)
}

/// The context-usage fragment: `"{used}/{window} context"`, using the
/// auto-compact threshold as the denominator when one is set (tau
/// `_context_usage`).
#[must_use]
pub fn context_usage(info: &StatusInfo) -> String {
    let denominator = match info.auto_compact_token_threshold {
        Some(threshold) if threshold > 0 => threshold,
        _ => info.context_window_tokens,
    };
    format!(
        "{}/{} context",
        compact_token_count(info.context_token_estimate),
        compact_token_count(denominator)
    )
}

/// Build the (left, right) columns of the compact session-info line.
///
/// left  = `"{short_cwd} ({git_branch})"`
/// right = `"{context_usage}  {provider}:{model} ({thinking})"`
#[must_use]
pub fn build_compact_session_info(info: &StatusInfo) -> (String, String) {
    let left = format!("{} ({})", short_path(&info.cwd), info.git_branch);
    let right = format!(
        "{}  {}:{} ({})",
        context_usage(info),
        info.provider_name,
        info.model,
        info.thinking_display
    );
    (left, right)
}

/// Abbreviate a path under `$HOME` as `~/...` (tau `_short_path`).
#[must_use]
pub fn short_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Expand a leading `~` to `$HOME` (tau `Path.expanduser`).
fn expand_home(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

/// The sidebar display label for a project context file (tau
/// `_context_file_label`): the path relative to `cwd` when the file lives under
/// the project, otherwise `~/`-abbreviated for paths under `$HOME` and the full
/// absolute path only for anything outside home. tau e3fc26d shortened these
/// external home paths in the sidebar instead of showing the raw absolute path.
#[must_use]
pub fn context_file_label(path: &Path, cwd: &Path) -> String {
    let expanded = expand_home(path);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let resolved = absolute.canonicalize().unwrap_or(absolute);
    let cwd_resolved = {
        let base = expand_home(cwd);
        base.canonicalize().unwrap_or(base)
    };
    if let Ok(rel) = resolved.strip_prefix(&cwd_resolved) {
        return rel.display().to_string();
    }
    short_path(&resolved)
}

/// The deadline tau applies to the git-branch lookup (`_git_branch`, `timeout=0.5`).
const GIT_BRANCH_TIMEOUT: Duration = Duration::from_millis(500);

/// Resolve the current git branch for `cwd`, returning `"--"` on any failure or if
/// git does not finish within 500 ms (tau `_git_branch`: `git -C cwd branch
/// --show-current`, `timeout=0.5`). The deadline keeps a stalled git/filesystem
/// from blocking the synchronous chrome refresh indefinitely; the child is killed
/// and reaped before returning `"--"`.
#[must_use]
pub fn git_branch(cwd: &Path) -> String {
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["branch", "--show-current"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return "--".to_string();
    };

    let deadline = Instant::now() + GIT_BRANCH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return "--".to_string();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return "--".to_string();
            }
        }
    }

    // The `branch --show-current` output is a single short line, so the pipe never
    // fills before the child exits (no writer-blocks-on-full-pipe deadlock).
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    let branch = output.trim();
    if branch.is_empty() {
        "--".to_string()
    } else {
        branch.to_string()
    }
}

/// Render the compact session-info line into `area`: left column left-justified,
/// right column right-justified, padded to fill the width (tau's two-column
/// `Table.grid`).
pub fn render_compact_session_info(
    frame: &mut Frame,
    area: Rect,
    info: &StatusInfo,
    theme: &TuiTheme,
) {
    let (left, right) = build_compact_session_info(info);
    let style = parse_style(&theme.muted_text);
    let bg = parse_style(&theme.chrome_background);
    let width = area.width as usize;
    let line = fit_two_columns(&left, &right, width, style);
    frame.render_widget(Paragraph::new(line).style(bg), area);
}

/// Lay `left` and `right` on one line of `width` cells: left starts at column 0,
/// right is flush against the right edge, with at least one space between. If the
/// two would collide, the left column is truncated with an ellipsis.
fn fit_two_columns(left: &str, right: &str, width: usize, style: Style) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return Line::default();
    }
    let right_w = right.width();
    let left_w = left.width();
    if right_w > width {
        // Right column alone overflows; show as much of it as fits.
        return Line::from(Span::styled(truncate_to_width(right, width), style));
    }
    if right_w == width {
        // Exactly fills the line — show it whole, no left column, no truncation.
        return Line::from(Span::styled(right.to_string(), style));
    }
    let available_left = width - right_w;
    let (left_str, left_used) = if left_w + 1 > available_left {
        let cap = available_left.saturating_sub(1);
        let truncated = truncate_to_width(left, cap);
        let used = truncated.width();
        (truncated, used)
    } else {
        (left.to_string(), left_w)
    };
    let gap = width - left_used - right_w;
    let mut spans = vec![Span::styled(left_str, style)];
    if gap > 0 {
        spans.push(Span::styled(" ".repeat(gap), style));
    }
    spans.push(Span::styled(right.to_string(), style));
    Line::from(spans)
}

fn truncate_to_width(text: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    use unicode_width::UnicodeWidthStr;
    if max == 0 {
        return String::new();
    }
    // If the text already fits, return it verbatim — never spend a cell on an
    // ellipsis for text that fits exactly (the off-by-one CodeRabbit flagged).
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    let ellipsis_w = 1;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > max.saturating_sub(ellipsis_w) {
            out.push('…');
            return out;
        }
        out.push(ch);
        used += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> StatusInfo {
        StatusInfo {
            cwd: PathBuf::from("/tmp/project"),
            provider_name: "anthropic".to_string(),
            model: "claude".to_string(),
            thinking_display: "medium".to_string(),
            context_token_estimate: 12_400,
            context_window_tokens: 128_000,
            auto_compact_token_threshold: None,
            git_branch: "main".to_string(),
        }
    }

    #[test]
    fn compact_token_count_matches_tau_buckets() {
        assert_eq!(compact_token_count(0), "0k");
        assert_eq!(compact_token_count(-5), "0k");
        assert_eq!(compact_token_count(1), "<1k");
        assert_eq!(compact_token_count(999), "<1k");
        // (1000 + 500) / 1000 = 1 (integer division) -> "1k"
        assert_eq!(compact_token_count(1000), "1k");
        // (1499 + 500) / 1000 = 1 -> "1k"; (1500 + 500) / 1000 = 2 -> "2k"
        assert_eq!(compact_token_count(1499), "1k");
        assert_eq!(compact_token_count(1500), "2k");
    }

    #[test]
    fn context_usage_uses_window_by_default() {
        assert_eq!(context_usage(&info()), "12k/128k context");
    }

    #[test]
    fn context_usage_uses_threshold_when_set() {
        let mut i = info();
        i.auto_compact_token_threshold = Some(64_000);
        assert_eq!(context_usage(&i), "12k/64k context");
    }

    #[test]
    fn builds_two_columns() {
        let (left, right) = build_compact_session_info(&info());
        assert_eq!(left, "/tmp/project (main)");
        assert_eq!(right, "12k/128k context  anthropic:claude (medium)");
    }

    #[test]
    fn short_path_abbreviates_home() {
        // Directly exercise the ~ rewrite via a synthetic HOME.
        // SAFETY: single-threaded test; restores nothing (process-local).
        let home = std::env::var_os("HOME");
        if let Some(h) = &home {
            let p = PathBuf::from(h).join("code/x");
            assert_eq!(short_path(&p), "~/code/x");
            assert_eq!(short_path(&PathBuf::from(h)), "~");
        }
    }

    #[test]
    fn context_label_is_cwd_relative_for_project_files() {
        // A context file under the project renders cwd-relative (tau
        // `_context_file_label`), not as an absolute path.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let file = cwd.join("AGENTS.md");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(context_file_label(&file, cwd), "AGENTS.md");

        let nested = cwd.join("docs/AGENTS.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "x").unwrap();
        // Path separators are platform-native; on unix this is `docs/AGENTS.md`.
        assert_eq!(
            context_file_label(&nested, cwd),
            PathBuf::from("docs")
                .join("AGENTS.md")
                .display()
                .to_string()
        );
    }

    #[test]
    fn context_label_shortens_home_paths_outside_cwd() {
        // tau e3fc26d: a context file under $HOME but outside the project cwd is
        // shown `~/`-abbreviated rather than as a full absolute path.
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        // A home-relative path (non-existent is fine: the resolve falls back to the
        // absolute path, which is still under $HOME and gets the ~ prefix).
        let external = home.join("shared/global/AGENTS.md");
        assert_eq!(
            context_file_label(&external, tmp.path()),
            "~/shared/global/AGENTS.md"
        );
        // A `~`-prefixed input is expanded first, then re-abbreviated.
        assert_eq!(
            context_file_label(&PathBuf::from("~/shared/global/AGENTS.md"), tmp.path()),
            "~/shared/global/AGENTS.md"
        );
    }
}
