//! Helpers shared by the adapters' request builders.
//!
//! * [`message_text`] ports tau's `message_text` — the user-visible text of any
//!   transcript message — used wherever an adapter falls back to
//!   `message_to_user(message)` for a non-provider-native message kind.
//! * [`python_dumps`] reproduces Python's `json.dumps(value)` **default** output
//!   (`", "`/`": "` separators, `ensure_ascii=True`). tau serializes tool-call
//!   `arguments` into a string with this call (`dumps(tool_call.arguments)`); the
//!   spaces and `\uXXXX` escapes become string *content* inside the request body,
//!   so they survive the fixture's compact re-serialization and must match.

use std::fmt::Write;

use rho_agent::messages::AgentMessage;
use serde_json::Value;

/// Return the user-visible text represented by an agent message
/// (tau `message_text`).
#[must_use]
pub fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(m) => m.text(),
        AgentMessage::Assistant(m) => m.text(),
        AgentMessage::ToolResult(m) => m.text(),
        AgentMessage::Custom(m) => m.text(),
        AgentMessage::BranchSummary(m) => m.summary.clone(),
        AgentMessage::CompactionSummary(m) => m.summary.clone(),
        AgentMessage::BashExecution(m) => m.output.clone(),
    }
}

/// Serialize a JSON value the way Python's `json.dumps(value)` does by default
/// (tau's `dumps(...)`): `", "` between items, `": "` after keys, ASCII-only
/// (non-ASCII escaped as `\uXXXX`, with surrogate pairs above the BMP), and keys
/// in insertion order.
#[must_use]
pub fn python_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_string(out, key);
                out.push_str(": ");
                write_value(out, val);
            }
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if c.is_ascii() => out.push(c),
            c => {
                // ensure_ascii: escape as \uXXXX, splitting astral chars into a
                // UTF-16 surrogate pair (Python's behavior).
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_dumps_uses_spaces_and_preserves_order() {
        let value = serde_json::json!({"path": "a.txt", "n": 1});
        assert_eq!(python_dumps(&value), r#"{"path": "a.txt", "n": 1}"#);
    }

    #[test]
    fn python_dumps_escapes_non_ascii() {
        // ensure_ascii=True: `é` (U+00E9) → é, `🎉` (U+1F389) → surrogate pair.
        let value = serde_json::json!({"emoji": "\u{e9}\u{1f389}"});
        let expected = "{\"emoji\": \"\\u00e9\\ud83c\\udf89\"}";
        assert_eq!(python_dumps(&value), expected);
    }
}
