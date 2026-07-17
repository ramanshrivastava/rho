//! Minimal Python-string-semantics helpers used by the TUI ports.
//!
//! `rho-coding` has an internal `pystr` module, but it is private to that crate;
//! the TUI needs the same `str.splitlines()` / codepoint-slice behaviour for
//! byte-parity with tau's transcript formatting, so the small subset is ported
//! here rather than widening `rho-coding`'s API surface.

/// Python `str.splitlines()` — split on every Unicode line boundary, dropping the
/// terminators, with no trailing empty element for a final boundary.
///
/// Matches CPython's boundary set: `\n \r \r\n \v \f \x1c \x1d \x1e \x85
///  `.
#[must_use]
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes_len = text.len();
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        let is_break = matches!(
            ch,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&text[start..idx]);
        // Consume a `\n` following a `\r` (CRLF is a single boundary).
        let mut next_start = idx + ch.len_utf8();
        if ch == '\r' {
            if let Some(&(_, '\n')) = chars.peek() {
                chars.next();
                next_start += '\n'.len_utf8();
            }
        }
        start = next_start;
    }
    if start < bytes_len {
        lines.push(&text[start..]);
    }
    lines
}

/// Count Unicode codepoints (Python `len(str)`).
#[must_use]
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Python `text[:n]` — take the first `n` codepoints.
#[must_use]
pub fn char_prefix(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_drops_terminators_without_trailing_empty() {
        assert_eq!(splitlines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(splitlines("a\r\nb"), vec!["a", "b"]);
        assert_eq!(splitlines("a\rb\nc"), vec!["a", "b", "c"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("solo"), vec!["solo"]);
    }

    #[test]
    fn char_prefix_counts_codepoints() {
        assert_eq!(char_prefix("héllo", 3), "hél");
        assert_eq!(char_prefix("hi", 10), "hi");
    }
}
