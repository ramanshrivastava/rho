//! Detect and normalize files dragged into the terminal (port of tau
//! `tau_coding/tui/file_drop.py`).
//!
//! Terminals do not deliver OS drag-and-drop as a dedicated event: dropping a
//! file types its path into the running program. With bracketed paste enabled
//! that typed path arrives as a single paste. The exact text depends on the
//! terminal — shell-escaped (`/tmp/my\ file.png`), quoted (`"/tmp/my file.png"`),
//! a `file://` URI, or a bare path with unescaped spaces.
//!
//! [`normalize_dropped_paths`] recognizes pasted text that consists solely of
//! one or more *existing absolute paths* and normalizes it to clean,
//! space-separated filesystem paths, quoting any path that contains whitespace.
//! Anything else returns `None` so the paste falls through to default handling.

use std::path::Path;

/// Return normalized prompt text when `text` looks like a file drop (tau
/// `normalize_dropped_paths`).
///
/// Treated as a drop only when it consists exclusively of one or more absolute
/// paths that exist on disk (shell-escaped, quoted, or `file://` URI forms are
/// accepted). Otherwise returns `None`.
#[must_use]
pub fn normalize_dropped_paths(text: &str) -> Option<String> {
    let stripped = text.trim();
    if stripped.is_empty() {
        return None;
    }

    // A single dropped file may arrive as a bare path with unescaped spaces.
    if let Some(whole) = token_to_path(stripped) {
        return Some(quote_path(&whole));
    }

    let tokens = shlex_split(stripped)?;
    if tokens.is_empty() {
        return None;
    }

    let mut paths: Vec<String> = Vec::with_capacity(tokens.len());
    for token in tokens {
        let path = token_to_path(&token)?;
        paths.push(path);
    }
    Some(
        paths
            .iter()
            .map(|p| quote_path(p))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Resolve one dropped token to an existing absolute path, if possible (tau
/// `_token_to_path`).
fn token_to_path(token: &str) -> Option<String> {
    let mut candidate = token.to_string();
    if let Some(rest) = candidate.strip_prefix("file://") {
        // Split into netloc (up to the first '/') and path (from that '/').
        let (netloc, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        if !netloc.is_empty() && netloc != "localhost" {
            return None;
        }
        candidate = percent_decode(path);
    }
    let path = Path::new(&candidate);
    if !path.is_absolute() || !path.exists() {
        return None;
    }
    Some(candidate)
}

/// Quote `path` with double quotes when it contains whitespace (tau
/// `_quote_path`).
fn quote_path(path: &str) -> String {
    if !path.chars().any(char::is_whitespace) {
        return path.to_string();
    }
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Percent-decode a URI path component (minimal `urllib.parse.unquote`).
#[allow(clippy::cast_possible_truncation)] // hi*16+lo is a byte value in 0..256
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split `text` into tokens using POSIX shell rules (a focused port of Python's
/// `shlex.split(text, posix=True)` covering the escaping terminals apply to
/// dropped paths). Returns `None` on an unterminated quote.
fn shlex_split(text: &str) -> Option<Vec<String>> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            '\\' => {
                has_token = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' => {
                has_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return None, // unterminated single quote
                    }
                }
            }
            '"' => {
                has_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            // In POSIX double quotes, a backslash only escapes
                            // $ ` " \ and newline; otherwise it is literal.
                            match chars.peek() {
                                Some('$' | '`' | '"' | '\\') => {
                                    current.push(chars.next().unwrap());
                                }
                                Some('\n') => {
                                    chars.next();
                                }
                                _ => current.push('\\'),
                            }
                        }
                        Some(ch) => current.push(ch),
                        None => return None, // unterminated double quote
                    }
                }
            }
            other => {
                has_token = true;
                current.push(other);
            }
        }
    }
    if has_token {
        tokens.push(current);
    }
    Some(tokens)
}

/// Add separating whitespace around a drop `insertion` given the text
/// immediately `before` and `after` the cursor (tau `_insert_dropped_paths`).
#[must_use]
pub fn pad_dropped_insertion(insertion: &str, before: &str, after: &str) -> String {
    let mut result = insertion.to_string();
    if before
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace())
    {
        result = format!(" {result}");
    }
    if after.chars().next().is_none_or(|c| !c.is_whitespace()) {
        result = format!("{result} ");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, b"x").unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn bare_existing_path_is_normalized() {
        let dir = TempDir::new().unwrap();
        let path = touch(dir.path(), "note.md");
        assert_eq!(
            normalize_dropped_paths(&path).as_deref(),
            Some(path.as_str())
        );
    }

    #[test]
    fn bare_path_with_spaces_is_quoted() {
        let dir = TempDir::new().unwrap();
        let path = touch(dir.path(), "my file.png");
        let got = normalize_dropped_paths(&path).unwrap();
        assert_eq!(got, format!("\"{path}\""));
    }

    #[test]
    fn shell_escaped_spaces_are_recognized() {
        let dir = TempDir::new().unwrap();
        let path = touch(dir.path(), "my file.png");
        let escaped = path.replace(' ', "\\ ");
        let got = normalize_dropped_paths(&escaped).unwrap();
        assert_eq!(got, format!("\"{path}\""));
    }

    #[test]
    fn multiple_paths_are_space_separated() {
        let dir = TempDir::new().unwrap();
        let a = touch(dir.path(), "a.txt");
        let b = touch(dir.path(), "b.txt");
        let dropped = format!("{a} {b}");
        let got = normalize_dropped_paths(&dropped).unwrap();
        assert_eq!(got, format!("{a} {b}"));
    }

    #[test]
    fn file_uri_is_decoded() {
        let dir = TempDir::new().unwrap();
        let path = touch(dir.path(), "my file.png");
        let uri = format!("file://{}", path.replace(' ', "%20"));
        let got = normalize_dropped_paths(&uri).unwrap();
        assert_eq!(got, format!("\"{path}\""));
    }

    #[test]
    fn file_uri_with_remote_host_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = touch(dir.path(), "note.md");
        let uri = format!("file://remote{path}");
        assert_eq!(normalize_dropped_paths(&uri), None);
    }

    #[test]
    fn non_path_text_falls_through() {
        assert_eq!(normalize_dropped_paths("just some pasted text"), None);
        assert_eq!(normalize_dropped_paths(""), None);
        assert_eq!(normalize_dropped_paths("   "), None);
    }

    #[test]
    fn relative_or_missing_paths_are_rejected() {
        assert_eq!(normalize_dropped_paths("relative/path.txt"), None);
        assert_eq!(normalize_dropped_paths("/nonexistent/abc/xyz.zzz"), None);
    }

    #[test]
    fn mixed_valid_and_invalid_falls_through() {
        let dir = TempDir::new().unwrap();
        let a = touch(dir.path(), "a.txt");
        let dropped = format!("{a} /nonexistent/zzz.txt");
        assert_eq!(normalize_dropped_paths(&dropped), None);
    }

    #[test]
    fn padding_adds_separators_only_when_needed() {
        assert_eq!(pad_dropped_insertion("P", "", ""), "P ");
        assert_eq!(pad_dropped_insertion("P", "hi", ""), " P ");
        assert_eq!(pad_dropped_insertion("P", "hi ", "there"), "P ");
        assert_eq!(pad_dropped_insertion("P", "hi", " there"), " P");
    }
}
