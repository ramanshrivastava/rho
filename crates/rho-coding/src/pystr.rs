//! Python string-semantics helpers shared across the coding layer.
//!
//! These reproduce CPython behaviors rho must byte-match: `str.splitlines`, the
//! `str(dict)` / `repr` shape used by the transcript's malformed-call fallback
//! (and, later, M4b's token estimator), `str.isspace` for a single char, and
//! whitespace `rstrip`.

// This module is dense with Python identifiers in prose; backticking every one
// hurts readability more than it helps.
#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;

use rho_agent::types::JsonValue;

/// Python `str.splitlines(keepends)` — splits on the universal newline set
/// (`\n`, `\r`, `\r\n`, `\v`, `\f`, the C0 separators, `\x85`, ` `,
/// ` `); a trailing newline does **not** yield a trailing empty segment.
#[must_use]
pub(crate) fn splitlines(s: &str, keepends: bool) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut result = Vec::new();
    let mut i = 0;
    let mut start = 0;
    while i < n {
        let c = chars[i];
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if is_break {
            let mut eol = i + 1;
            if c == '\r' && eol < n && chars[eol] == '\n' {
                eol += 1;
            }
            let line: String = if keepends {
                chars[start..eol].iter().collect()
            } else {
                chars[start..i].iter().collect()
            };
            result.push(line);
            i = eol;
            start = eol;
        } else {
            i += 1;
        }
    }
    if start < n {
        result.push(chars[start..n].iter().collect());
    }
    result
}

/// Whether `c` is whitespace by Python's `str.isspace` for a single character.
///
/// This is Rust's Unicode `White_Space` plus the C0 information separators
/// `\x1c`–`\x1f`, which Python treats as whitespace (bidirectional class B/S)
/// but Rust's `char::is_whitespace` does not.
#[must_use]
pub(crate) fn is_python_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{1f}')
}

/// Python `str.rstrip()` with no argument: trim trailing [`is_python_space`].
#[must_use]
pub(crate) fn py_rstrip(s: &str) -> String {
    let end = s
        .char_indices()
        .rev()
        .find(|&(_, c)| !is_python_space(c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    s[..end].to_string()
}

/// Python `str(dict)` / `repr(obj)` for a JSON value (tau's fallback tool-call
/// rendering uses `str(tool_call.arguments)`). Strings use Python's quote
/// selection and escapes; `True`/`False`/`None`; floats carry a `.0` and Python
/// exponent form.
#[must_use]
pub(crate) fn python_repr(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "None".to_string(),
        JsonValue::Bool(true) => "True".to_string(),
        JsonValue::Bool(false) => "False".to_string(),
        JsonValue::Number(n) => {
            if n.is_i64() || n.is_u64() {
                n.to_string()
            } else {
                python_float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => python_str_repr(s),
        JsonValue::Array(items) => {
            let inner: Vec<String> = items.iter().map(python_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        JsonValue::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", python_str_repr(k), python_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python `repr(str)`: pick `'`, or `"` when the string has a `'` but no `"`;
/// escape backslash, the chosen quote, and `\n`/`\r`/`\t`; escape other
/// non-printable characters as `\xXX`/`\uXXXX`/`\UXXXXXXXX`.
fn python_str_repr(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };

    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !c.is_control() => out.push(c),
            c => {
                let cp = c as u32;
                if cp <= 0xff {
                    let _ = write!(out, "\\x{cp:02x}");
                } else if cp <= 0xffff {
                    let _ = write!(out, "\\u{cp:04x}");
                } else {
                    let _ = write!(out, "\\U{cp:08x}");
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python `repr(float)`: keep a trailing `.0` for whole values and use Python's
/// signed, ≥2-digit exponent form (`1e+20`). Rust's `{:?}` already gives the
/// shortest round-tripping mantissa; only its exponent shape differs.
///
/// Also reused by `branch_summary`'s `json.dumps(..., sort_keys=True)` port so
/// float arguments render Python-identically (`1e-07`, not serde's `1e-7`).
pub(crate) fn python_float_repr(value: f64) -> String {
    let s = format!("{value:?}");
    match s.split_once('e') {
        Some((mantissa, exp)) => {
            let exp: i32 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            format!("{mantissa}e{sign}{:02}", exp.abs())
        }
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repr(value: &serde_json::Value) -> String {
        python_repr(value)
    }

    #[test]
    fn dict_repr_matches_python_str_dict() {
        // Goldens captured from `str(dict)` in tau's interpreter.
        assert_eq!(
            repr(&serde_json::json!({"path": "a.py", "n": 1})),
            "{'path': 'a.py', 'n': 1}"
        );
        assert_eq!(
            repr(&serde_json::json!({"flag": true, "off": false, "nothing": null})),
            "{'flag': True, 'off': False, 'nothing': None}"
        );
        assert_eq!(
            repr(&serde_json::json!({"list": [1, "two", null, true], "nested": {"k": "v"}})),
            "{'list': [1, 'two', None, True], 'nested': {'k': 'v'}}"
        );
        assert_eq!(repr(&serde_json::json!({})), "{}");
    }

    #[test]
    fn str_repr_quote_selection_and_escapes() {
        assert_eq!(
            repr(&serde_json::json!({"s": "it's", "q": "say \"hi\"", "both": "a'b\"c"})),
            r#"{'s': "it's", 'q': 'say "hi"', 'both': 'a\'b"c'}"#
        );
        assert_eq!(
            repr(&serde_json::json!({"nl": "line1\nline2\ttab", "u": "café"})),
            r"{'nl': 'line1\nline2\ttab', 'u': 'café'}"
        );
    }

    #[test]
    fn float_repr_keeps_dot_zero_and_python_exponent() {
        assert_eq!(python_float_repr(1.0), "1.0");
        assert_eq!(python_float_repr(1.5), "1.5");
        assert_eq!(python_float_repr(1e20), "1e+20");
    }

    #[test]
    fn splitlines_drops_trailing_newline_segment() {
        assert_eq!(splitlines("a\nb\n", false), vec!["a", "b"]);
        assert_eq!(splitlines("a\r\nb", true), vec!["a\r\n", "b"]);
    }

    #[test]
    fn py_rstrip_handles_c0_separators() {
        assert_eq!(py_rstrip("x \t\u{1c}"), "x");
        assert_eq!(py_rstrip("keep"), "keep");
    }
}
