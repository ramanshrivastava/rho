//! Exact-match edit application and diff rendering (tau `tau_coding/tools.py`
//! edit helpers). Matching semantics are byte-for-byte with tau: LF
//! normalization for matching, BOM preservation, unique non-overlapping match
//! validation, reverse-order application, and the exact error strings.

use super::difflib::{ndiff, unified_diff};

/// UTF-8 BOM (tau `UTF8_BOM`).
pub const UTF8_BOM: char = '\u{feff}';

/// A single `{oldText, newText}` replacement.
#[derive(Debug, Clone)]
pub struct Edit {
    /// Text to find (must be unique and non-empty).
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
}

/// An edit could not be applied — carries tau's exact user-facing message.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct EditError(pub String);

/// Python `str.splitlines(keepends)` (shared implementation in [`crate::pystr`]).
#[must_use]
pub fn splitlines(s: &str, keepends: bool) -> Vec<String> {
    crate::pystr::splitlines(s, keepends)
}

/// tau `detect_line_ending`.
#[must_use]
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf = content.find("\r\n");
    let lf = content.find('\n');
    match (lf, crlf) {
        (None, _) | (_, None) => "\n",
        (Some(lf), Some(crlf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// tau `normalize_to_lf`.
#[must_use]
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// tau `restore_line_endings`.
#[must_use]
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// tau `_strip_bom`: `(bom, rest)` when a UTF-8 BOM leads, else `("", content)`.
#[must_use]
pub fn strip_bom(content: &str) -> (String, String) {
    if content.starts_with(UTF8_BOM) {
        (
            UTF8_BOM.to_string(),
            content[UTF8_BOM.len_utf8()..].to_string(),
        )
    } else {
        (String::new(), content.to_string())
    }
}

fn count_occurrences(content: &str, text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(idx) = content[start..].find(text) {
        count += 1;
        start += idx + text.len();
    }
    count
}

/// Apply edits to already-LF-normalized content (tau
/// `apply_edits_to_normalized_content`). Returns `(base, new)` where `base` is
/// the input unchanged and `new` is the result. Validates uniqueness and
/// non-overlap before applying, so a failure leaves the file untouched.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<(String, String), EditError> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();

    let total = normalized_edits.len();
    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(EditError(empty_old_text_error(path, index, total)));
        }
    }

    // (start, end, new_text)
    let mut matches: Vec<(usize, usize, String)> = Vec::new();
    for (index, edit) in normalized_edits.iter().enumerate() {
        let occurrences = count_occurrences(normalized_content, &edit.old_text);
        if occurrences == 0 {
            return Err(EditError(not_found_error(path, index, total)));
        }
        if occurrences > 1 {
            return Err(EditError(duplicate_error(path, index, total, occurrences)));
        }
        let start = normalized_content
            .find(&edit.old_text)
            .expect("occurrence counted");
        matches.push((start, start + edit.old_text.len(), edit.new_text.clone()));
    }

    validate_non_overlapping(&matches)?;

    // Apply in reverse start order (sorted descending), matching tau's
    // `sorted(matches, reverse=True)`.
    let mut ordered = matches.clone();
    ordered.sort_by(|a, b| b.cmp(a));
    let mut new_content = normalized_content.to_string();
    for (start, end, new_text) in ordered {
        new_content = format!(
            "{}{}{}",
            &new_content[..start],
            new_text,
            &new_content[end..]
        );
    }

    if new_content == normalized_content {
        return Err(EditError(no_change_error(path, total)));
    }
    Ok((normalized_content.to_string(), new_content))
}

fn validate_non_overlapping(spans: &[(usize, usize, String)]) -> Result<(), EditError> {
    let mut sorted: Vec<&(usize, usize, String)> = spans.iter().collect();
    sorted.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let mut previous_end: Option<usize> = None;
    for (start, end, _) in sorted {
        if previous_end.is_some_and(|pe| *start < pe) {
            return Err(EditError("Edits must not overlap".to_string()));
        }
        previous_end = Some(*end);
    }
    Ok(())
}

/// tau `generate_diff_string`: `(ndiff_text, first_changed_line)`.
#[must_use]
pub fn generate_diff_string(old: &str, new: &str) -> (String, Option<i64>) {
    let old_lines = splitlines(old, false);
    let new_lines = splitlines(new, false);
    let delta = ndiff(&old_lines, &new_lines);
    let diff = delta.join("\n");

    let mut first_changed_line: Option<i64> = None;
    let mut new_line_number: i64 = 0;
    for line in &delta {
        if line.starts_with("  ") {
            new_line_number += 1;
        } else if line.starts_with('+') {
            new_line_number += 1;
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_number);
            }
        } else if line.starts_with('-') && first_changed_line.is_none() {
            first_changed_line = Some((new_line_number + 1).max(1));
        }
    }
    (diff, first_changed_line)
}

/// tau `generate_unified_patch`.
#[must_use]
pub fn generate_unified_patch(path: &str, old: &str, new: &str) -> String {
    unified_diff(&splitlines(old, true), &splitlines(new, true), path, path)
}

// ---- exact error strings (tau) -------------------------------------------

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly \
including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly \
including all whitespace and newlines."
        )
    }
}

fn duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. \
Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. \
Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. \
This might indicate an issue with special characters or the text not existing \
as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_no_keepends() {
        assert_eq!(splitlines("a\nb\nc\n", false), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\nb", false), vec!["a", "b"]);
        assert_eq!(splitlines("", false), Vec::<String>::new());
    }

    #[test]
    fn splitlines_keepends() {
        assert_eq!(splitlines("a\nb\n", true), vec!["a\n", "b\n"]);
        assert_eq!(splitlines("a\r\nb", true), vec!["a\r\n", "b"]);
    }

    #[test]
    fn detect_line_ending_prefers_first() {
        assert_eq!(detect_line_ending("a\r\nb\n"), "\r\n");
        assert_eq!(detect_line_ending("a\nb\r\n"), "\n");
        assert_eq!(detect_line_ending("no newline"), "\n");
    }

    #[test]
    fn applies_multiple_replacements() {
        let (base, new) = apply_edits_to_normalized_content(
            "alpha\nbeta\ngamma\n",
            &[
                Edit {
                    old_text: "alpha".into(),
                    new_text: "one".into(),
                },
                Edit {
                    old_text: "gamma".into(),
                    new_text: "three".into(),
                },
            ],
            "/f.txt",
        )
        .unwrap();
        assert_eq!(base, "alpha\nbeta\ngamma\n");
        assert_eq!(new, "one\nbeta\nthree\n");
    }

    #[test]
    fn not_found_uses_indexed_message_for_multiple() {
        let err = apply_edits_to_normalized_content(
            "alpha\nbeta\n",
            &[
                Edit {
                    old_text: "alpha".into(),
                    new_text: "one".into(),
                },
                Edit {
                    old_text: "missing".into(),
                    new_text: "nope".into(),
                },
            ],
            "/f.txt",
        )
        .unwrap_err();
        assert!(
            err.0.contains("Could not find edits[1] in /f.txt"),
            "{}",
            err.0
        );
    }

    #[test]
    fn duplicate_match_rejected() {
        let err = apply_edits_to_normalized_content(
            "repeat\nrepeat\n",
            &[Edit {
                old_text: "repeat".into(),
                new_text: "once".into(),
            }],
            "/f.txt",
        )
        .unwrap_err();
        assert!(err.0.contains("Found 2 occurrences"), "{}", err.0);
    }

    #[test]
    fn empty_old_text_rejected() {
        let err = apply_edits_to_normalized_content(
            "x\n",
            &[Edit {
                old_text: String::new(),
                new_text: "y".into(),
            }],
            "/f.txt",
        )
        .unwrap_err();
        assert_eq!(err.0, "oldText must not be empty in /f.txt.");
    }

    #[test]
    fn diff_and_patch_match_tau() {
        let (diff, first) = generate_diff_string("alpha\nbeta\ngamma\n", "one\nbeta\nthree\n");
        assert_eq!(diff, "- alpha\n+ one\n  beta\n- gamma\n+ three");
        assert_eq!(first, Some(1));
        let patch = generate_unified_patch("/f.txt", "alpha\nbeta\ngamma\n", "one\nbeta\nthree\n");
        assert_eq!(
            patch,
            "--- /f.txt\n+++ /f.txt\n@@ -1,3 +1,3 @@\n-alpha\n+one\n beta\n-gamma\n+three\n"
        );
    }

    #[test]
    fn insert_first_changed_line() {
        let (_diff, first) = generate_diff_string("a\nb\n", "a\nX\nb\n");
        assert_eq!(first, Some(2));
    }
}
