//! Output truncation with byte/line limits (tau `tau_coding/tools.py`
//! `TruncationResult`, `truncate_head`, `truncate_tail`).
//!
//! Byte counts are UTF-8 byte lengths (Python's `str.encode()` length ==
//! Rust's `str::len()`). Line counting splits on `\n` and drops a single
//! trailing empty segment when the content ends with a newline, matching tau's
//! `_split_lines_for_counting`.
//!
//! `TruncationResult` serializes with **`snake_case`** keys and keeps `None`
//! (`truncated_by: null`) — it lands inside a tool result's free-form `details`
//! payload, which tau's `exclude_none` does not recurse into.

use serde::Serialize;

/// Default byte cap for tool output (tau `DEFAULT_MAX_OUTPUT_BYTES` = 50 KiB).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 50 * 1024;
/// Default line cap for tool output (tau `DEFAULT_MAX_OUTPUT_LINES`).
pub const DEFAULT_MAX_OUTPUT_LINES: usize = 2_000;

/// Metadata describing how a tool output was shortened (tau `TruncationResult`).
///
/// Field order and names match tau's `asdict()` output exactly; `truncated_by`
/// stays `null` when absent (free-form `details` preserves inner nulls).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TruncationResult {
    /// The returned slice.
    pub content: String,
    /// Whether truncation happened.
    pub truncated: bool,
    /// Which limit triggered truncation (`"lines"` / `"bytes"`), or `null`.
    pub truncated_by: Option<String>,
    /// Total lines in the original output.
    pub total_lines: usize,
    /// Total bytes in the original output.
    pub total_bytes: usize,
    /// Lines in the returned slice.
    pub output_lines: usize,
    /// Bytes in the returned slice.
    pub output_bytes: usize,
    /// Whether the last returned line was itself clipped mid-line.
    pub last_line_partial: bool,
    /// Whether the first line alone exceeds the byte limit.
    pub first_line_exceeds_limit: bool,
    /// The line cap in force.
    pub max_lines: usize,
    /// The byte cap in force.
    pub max_bytes: usize,
}

impl TruncationResult {
    #[allow(clippy::too_many_arguments)]
    fn build(
        content: String,
        truncated: bool,
        truncated_by: Option<&str>,
        total_lines: usize,
        total_bytes: usize,
        output_lines: usize,
        output_bytes: usize,
        last_line_partial: bool,
        first_line: bool,
    ) -> Self {
        Self {
            content,
            truncated,
            truncated_by: truncated_by.map(str::to_string),
            total_lines,
            total_bytes,
            output_lines,
            output_bytes,
            last_line_partial,
            first_line_exceeds_limit: first_line,
            max_lines: DEFAULT_MAX_OUTPUT_LINES,
            max_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// The truncation dict as a free-form JSON value (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("TruncationResult serializes")
    }
}

/// Format a byte count like tau's `format_size` (`123B`, `1.5KB`, `2.0MB`).
#[must_use]
pub fn format_size(bytes_count: usize) -> String {
    if bytes_count < 1024 {
        format!("{bytes_count}B")
    } else if bytes_count < 1024 * 1024 {
        #[allow(clippy::cast_precision_loss)]
        let kb = bytes_count as f64 / 1024.0;
        format!("{kb:.1}KB")
    } else {
        #[allow(clippy::cast_precision_loss)]
        let mb = bytes_count as f64 / (1024.0 * 1024.0);
        format!("{mb:.1}MB")
    }
}

/// Split content into lines for counting (tau `_split_lines_for_counting`):
/// split on `\n`, dropping a single trailing empty segment from a final newline.
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Keep the leading lines within limits (tau `truncate_head`).
#[must_use]
pub fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::build(
            content.to_string(),
            false,
            None,
            total_lines,
            total_bytes,
            total_lines,
            total_bytes,
            false,
            false,
        );
    }

    let first_line_bytes = lines.first().map_or(0, |l| l.len());
    if first_line_bytes > max_bytes {
        return TruncationResult::build(
            String::new(),
            true,
            Some("bytes"),
            total_lines,
            total_bytes,
            0,
            0,
            false,
            true,
        );
    }

    let mut output_lines: Vec<&str> = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    for (index, line) in lines.iter().take(max_lines).enumerate() {
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes";
            break;
        }
        output_lines.push(line);
        output_bytes += line_bytes;
    }

    let output = output_lines.join("\n");
    let out_len = output.len();
    let out_line_count = output_lines.len();
    TruncationResult::build(
        output,
        true,
        Some(truncated_by),
        total_lines,
        total_bytes,
        out_line_count,
        out_len,
        false,
        false,
    )
}

/// Keep the trailing lines within limits (tau `truncate_tail`).
#[must_use]
pub fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::build(
            content.to_string(),
            false,
            None,
            total_lines,
            total_bytes,
            total_lines,
            total_bytes,
            false,
            false,
        );
    }

    let mut output_lines: Vec<String> = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    let mut last_line_partial = false;
    for line in lines.iter().rev() {
        let line_bytes = line.len() + usize::from(!output_lines.is_empty());
        if output_lines.len() >= max_lines {
            truncated_by = "lines";
            break;
        }
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes";
            if output_lines.is_empty() {
                let clipped = truncate_string_to_bytes_from_end(line, max_bytes);
                output_lines.insert(0, clipped);
                last_line_partial = true;
            }
            break;
        }
        output_lines.insert(0, (*line).to_string());
        output_bytes += line_bytes;
    }

    let output = output_lines.join("\n");
    let out_len = output.len();
    let out_line_count = output_lines.len();
    TruncationResult::build(
        output,
        true,
        Some(truncated_by),
        total_lines,
        total_bytes,
        out_line_count,
        out_len,
        last_line_partial,
        false,
    )
}

/// Clip a string to the last `max_bytes` UTF-8 bytes, dropping a broken leading
/// char (tau `_truncate_string_to_bytes_from_end`, `decode(errors="ignore")`).
fn truncate_string_to_bytes_from_end(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && std::str::from_utf8(&bytes[start..]).is_err() {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_matches_tau() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0MB");
    }

    #[test]
    fn no_truncation_when_within_limits() {
        let r = truncate_head(
            "a\nb\nc\n",
            DEFAULT_MAX_OUTPUT_LINES,
            DEFAULT_MAX_OUTPUT_BYTES,
        );
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.content, "a\nb\nc\n");
        assert_eq!(r.truncated_by, None);
    }

    #[test]
    fn head_truncates_by_lines() {
        let content = "l1\nl2\nl3\nl4\n";
        let r = truncate_head(content, 2, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(r.truncated);
        assert_eq!(r.truncated_by.as_deref(), Some("lines"));
        assert_eq!(r.output_lines, 2);
        assert_eq!(r.content, "l1\nl2");
    }

    #[test]
    fn head_truncates_by_bytes_at_boundary() {
        // 10 lines of "aaaa" (4 bytes) — cap bytes so only some fit.
        let content = (0..10).map(|_| "aaaa").collect::<Vec<_>>().join("\n");
        // First line 4 bytes; each subsequent +1 (newline) +4 = 5.
        let r = truncate_head(&content, DEFAULT_MAX_OUTPUT_LINES, 9);
        assert!(r.truncated);
        assert_eq!(r.truncated_by.as_deref(), Some("bytes"));
        // 4 + 5 = 9 <= 9 → two lines fit; third would be 14 > 9.
        assert_eq!(r.content, "aaaa\naaaa");
    }

    #[test]
    fn head_first_line_exceeds_limit() {
        let content = "aaaaaaaaaa\nb";
        let r = truncate_head(content, DEFAULT_MAX_OUTPUT_LINES, 5);
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.truncated_by.as_deref(), Some("bytes"));
        assert_eq!(r.content, "");
        assert_eq!(r.output_lines, 0);
    }

    #[test]
    fn tail_keeps_trailing_lines() {
        let content = "l1\nl2\nl3\nl4\n";
        let r = truncate_tail(content, 2, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(r.truncated);
        assert_eq!(r.content, "l3\nl4");
    }

    #[test]
    fn tail_clips_single_long_line_from_end() {
        // One line longer than the byte cap → clipped from the end, partial.
        let content = "abcdefghij";
        let r = truncate_tail(content, DEFAULT_MAX_OUTPUT_LINES, 4);
        assert!(r.truncated);
        assert!(r.last_line_partial);
        assert_eq!(r.content, "ghij");
        assert_eq!(r.truncated_by.as_deref(), Some("bytes"));
    }

    #[test]
    fn tail_clip_respects_utf8_boundary() {
        // "é" is two bytes (0xC3 0xA9). Clipping to 3 bytes from the end of
        // "aéé" must drop the broken leading byte (errors="ignore").
        let content = "aéé"; // bytes: 61 C3 A9 C3 A9 (5 bytes)
        let r = truncate_tail(content, DEFAULT_MAX_OUTPUT_LINES, 3);
        assert!(r.last_line_partial);
        // last 3 bytes = A9 C3 A9; drop leading A9 → "é".
        assert_eq!(r.content, "é");
    }

    #[test]
    fn truncation_json_keeps_null_truncated_by() {
        let r = truncate_head("short", DEFAULT_MAX_OUTPUT_LINES, DEFAULT_MAX_OUTPUT_BYTES);
        let json = serde_json::to_string(&r.to_json()).unwrap();
        assert!(json.contains("\"truncated_by\":null"), "{json}");
        assert!(json.contains("\"max_lines\":2000"));
        assert!(json.contains("\"max_bytes\":51200"));
    }
}
