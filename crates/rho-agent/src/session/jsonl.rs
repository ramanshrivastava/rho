//! JSONL line codec and Tau-v1 persisted-session migration (tau
//! `tau_agent/session/jsonl.py`).
//!
//! ## Encode
//!
//! [`entry_to_json_line`] serializes one [`SessionEntry`] with `exclude_none`
//! semantics (each optional field already carries `skip_serializing_if`) and
//! appends a `\n`. This is tau's *storage* path; the HTML-export path writes
//! nulls and is a separate concern in a later crate.
//!
//! ## Decode + migrate
//!
//! Old Tau-v1 sessions used a looser message shape (string `content`, a `tool`
//! role, `data` payloads, sibling `tool_calls`). Because our typed models use
//! `deny_unknown_fields`, they would reject that shape outright — so migration
//! runs **before** typed decoding, as a transform on the raw `serde_json::Value`
//! (mirroring tau, which migrates on decoded dicts). Migration is confined to
//! this persistence boundary so the runtime models keep one strict protocol, and
//! it is a no-op on already-current (v2) entries.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::session::entries::SessionEntry;

/// Raised when a session JSONL line cannot be decoded (tau `SessionJsonlError`).
#[derive(Debug, thiserror::Error)]
#[error("Invalid session entry{location}: {source}")]
pub struct SessionJsonlError {
    location: String,
    source: serde_json::Error,
}

/// Serialize one entry to a canonical JSONL line (trailing newline included).
pub fn entry_to_json_line(entry: &SessionEntry) -> String {
    let mut line = serde_json::to_string(entry).expect("SessionEntry serialization is infallible");
    line.push('\n');
    line
}

/// Deserialize one entry, migrating persisted Tau-v1 messages first.
pub fn entry_from_json_line(
    line: &str,
    line_number: Option<usize>,
) -> Result<SessionEntry, SessionJsonlError> {
    let location = line_number.map_or_else(String::new, |n| format!(" on line {n}"));
    decode(line).map_err(|source| SessionJsonlError { location, source })
}

fn decode(line: &str) -> Result<SessionEntry, serde_json::Error> {
    let mut payload: Value = serde_json::from_str(line)?;
    migrate_session_entry(&mut payload);
    SessionEntry::deserialize(payload)
}

/// Deserialize every non-blank JSONL line in order.
pub fn entries_from_json_lines(lines: &[&str]) -> Result<Vec<SessionEntry>, SessionJsonlError> {
    let mut entries = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        entries.push(entry_from_json_line(line, Some(index + 1))?);
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Tau-v1 migration (a transform on the raw decoded Value)
// ---------------------------------------------------------------------------

fn migrate_session_entry(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if obj.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    if let Some(message) = obj.get_mut("message") {
        migrate_message(message);
    }
}

fn migrate_message(value: &mut Value) {
    let Some(msg) = value.as_object_mut() else {
        return;
    };
    match msg.get("role").and_then(Value::as_str) {
        Some("user") if msg.contains_key("custom_type") || msg.contains_key("customType") => {
            migrate_user_to_custom(msg);
        }
        Some("assistant") => migrate_assistant(msg),
        Some("tool") => migrate_tool(msg),
        _ => {}
    }
}

fn migrate_user_to_custom(msg: &mut Map<String, Value>) {
    msg.insert("role".into(), Value::String("custom".into()));
    // `customType` takes the snake `custom_type` if present, else the existing
    // camel value; the snake key is then removed.
    let custom_type = msg
        .remove("custom_type")
        .or_else(|| msg.get("customType").cloned());
    if let Some(ct) = custom_type {
        msg.insert("customType".into(), ct);
    }
    msg.entry("display").or_insert(Value::Bool(true));
}

fn migrate_assistant(msg: &mut Map<String, Value>) {
    // usage.cost == null → {} (so it decodes to the all-zero default cost).
    if let Some(Value::Object(usage)) = msg.get_mut("usage") {
        if matches!(usage.get("cost"), Some(Value::Null) | None) {
            usage.insert("cost".into(), Value::Object(Map::new()));
        }
    }

    let content = msg.get("content").cloned();
    match content {
        Some(Value::String(text)) => {
            let mut blocks = Vec::new();
            if !text.is_empty() {
                blocks.push(text_block(&text));
            }
            blocks.extend(take_tool_calls(msg));
            msg.insert("content".into(), Value::Array(blocks));
        }
        _ if msg.contains_key("tool_calls") || msg.contains_key("toolCalls") => {
            let mut blocks = match content {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            };
            blocks.extend(take_tool_calls(msg));
            msg.insert("content".into(), Value::Array(blocks));
        }
        _ => {}
    }
}

fn migrate_tool(msg: &mut Map<String, Value>) {
    msg.insert("role".into(), Value::String("toolResult".into()));

    let tool_name = msg
        .remove("name")
        .or_else(|| msg.get("toolName").cloned())
        .unwrap_or_else(|| Value::String("unknown".into()));
    msg.insert("toolName".into(), tool_name);

    let tool_call_id = msg
        .remove("tool_call_id")
        .or_else(|| msg.get("toolCallId").cloned())
        .unwrap_or_else(|| Value::String(String::new()));
    msg.insert("toolCallId".into(), tool_call_id);

    let ok = msg.remove("ok").and_then(|v| v.as_bool()).unwrap_or(true);
    msg.insert("isError".into(), Value::Bool(!ok));

    // Normalize content exactly as tau does: only a *string* (or a missing)
    // content is rewritten to a text-block list; an existing list is left as-is.
    match msg.get("content") {
        Some(Value::String(text)) => {
            let blocks = if text.is_empty() {
                Vec::new()
            } else {
                vec![text_block(text)]
            };
            msg.insert("content".into(), Value::Array(blocks));
        }
        None => {
            msg.insert("content".into(), Value::Array(Vec::new()));
        }
        _ => {}
    }

    // Fold legacy `data` into `details` (data first, then details wins).
    let data = msg.remove("data");
    let details = msg.get("details").cloned();
    match (data, details) {
        (Some(Value::Object(data_map)), Some(Value::Object(details_map))) => {
            let mut merged = Map::new();
            for (k, v) in data_map {
                merged.insert(k, v);
            }
            for (k, v) in details_map {
                merged.insert(k, v);
            }
            msg.insert("details".into(), Value::Object(merged));
        }
        (Some(data_val), None) => {
            msg.insert("details".into(), data_val);
        }
        _ => {}
    }

    // A legacy top-level `error` becomes the content when no content survived.
    if let Some(err) = msg.remove("error") {
        let content_empty = msg
            .get("content")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
        if content_empty && !err.is_null() {
            // Fixtures only carry string errors; non-string is a best-effort edge.
            let text = match err {
                Value::String(s) => s,
                other => other.to_string(),
            };
            msg.insert("content".into(), Value::Array(vec![text_block(&text)]));
        }
    }
}

fn text_block(text: &str) -> Value {
    let mut block = Map::new();
    block.insert("type".into(), Value::String("text".into()));
    block.insert("text".into(), Value::String(text.to_string()));
    Value::Object(block)
}

/// Remove both `tool_calls` and `toolCalls`, returning the preferred list.
///
/// Mirrors tau's `message.pop("tool_calls", message.pop("toolCalls", []))`:
/// both keys are removed and the snake variant wins when both are present.
fn take_tool_calls(msg: &mut Map<String, Value>) -> Vec<Value> {
    let snake = msg.remove("tool_calls");
    let camel = msg.remove("toolCalls");
    match snake.or(camel) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}
