//! Small parsing helpers shared by the SSE adapters (tau's per-module
//! `_parse_sse_line` / `_loads_object` / `_int_or_*`).

use serde_json::{Map, Value};

/// Strip a `data:` SSE line after trimming surrounding whitespace, returning the
/// payload (tau's `_parse_sse_line` in openai/mistral/google: `line.strip()`,
/// then `removeprefix("data:").strip()`). Non-`data:` / blank lines yield `None`.
#[must_use]
pub fn parse_sse_line(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("data:")?;
    Some(rest.trim().to_string())
}

/// Strip a `data:` SSE line **without** a leading trim (tau's anthropic
/// `_parse_sse_line`: `if not line.startswith("data:")`, then
/// `removeprefix("data:").strip()`).
#[must_use]
pub fn parse_sse_line_no_lstrip(line: &str) -> Option<String> {
    let rest = line.strip_prefix("data:")?;
    Some(rest.trim().to_string())
}

/// Parse a JSON object, returning `None` for invalid JSON or a non-object
/// (tau's `_loads_object`).
#[must_use]
pub fn loads_object(value: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// A JSON integer, else `0` (tau `_int_or_zero`). Booleans are not integers.
#[must_use]
pub fn int_or_zero(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// A JSON integer, else `None` (tau `_int_or_none`). Booleans are not integers.
#[must_use]
pub fn int_or_none(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

/// A non-empty JSON string, else `default` (tau `_string_or_default`).
#[must_use]
pub fn string_or_default(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

/// A JSON string (possibly empty), else `""` (tau `_string_or_empty`).
#[must_use]
pub fn string_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}
