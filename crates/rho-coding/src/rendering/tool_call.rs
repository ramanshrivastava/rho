//! Terse tool-call formatting for the transcript renderer (port of the slice of
//! tau's `tau_coding/tui/state.py` the renderer imports:
//! `format_tool_call_block` / `format_tool_call_invocation`).

use rho_agent::messages::ToolCall;
use rho_agent::types::{JsonMap, JsonValue};

const FALLBACK_INVOCATION_ARGS_CHARS: usize = 160;

/// Format a collapsed tool call (tau `format_tool_call_block`): `bash` renders
/// bare, every other tool is prefixed with `→ `.
#[must_use]
pub fn format_tool_call_block(tool_call: &ToolCall) -> String {
    let invocation = format_tool_call_invocation(tool_call);
    if tool_call.name == "bash" {
        invocation
    } else {
        format!("→ {invocation}")
    }
}

/// Format a tool call as a terse invocation (tau `format_tool_call_invocation`).
#[must_use]
pub fn format_tool_call_invocation(tool_call: &ToolCall) -> String {
    let args = &tool_call.arguments;
    match tool_call.name.as_str() {
        "read" => match string_argument(args, "path") {
            Some(path) => format!("read {path}{}", read_line_suffix(args)),
            None => fallback_invocation(tool_call),
        },
        "edit" => match string_argument(args, "path") {
            Some(path) => format!("edit {path}"),
            None => fallback_invocation(tool_call),
        },
        "write" => match string_argument(args, "path") {
            Some(path) => format!("write {path}"),
            None => fallback_invocation(tool_call),
        },
        "bash" => match string_argument(args, "command") {
            Some(command) => {
                let suffix = match number_argument(args, "timeout") {
                    Some(timeout) => {
                        format!(" (timeout {}s)", crate::fmt_util::format_g(timeout))
                    }
                    None => String::new(),
                };
                format!("$ {command}{suffix}")
            }
            None => fallback_invocation(tool_call),
        },
        _ => fallback_invocation(tool_call),
    }
}

fn read_line_suffix(args: &JsonMap) -> String {
    let offset = int_argument(args, "offset");
    let limit = int_argument(args, "limit");
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start = offset.map_or(1, |o| o.max(1));
    match limit {
        None => format!(":{start}-"),
        // Saturating: `start`/`limit` come from arbitrary tool-call JSON, and a
        // debug build would otherwise panic on `i64` overflow for huge values.
        Some(limit) => {
            let end = start.saturating_add(limit.max(1).saturating_sub(1));
            format!(":{start}-{end}")
        }
    }
}

fn fallback_invocation(tool_call: &ToolCall) -> String {
    if tool_call.arguments.is_empty() {
        return tool_call.name.clone();
    }
    let mut rendered =
        serde_json::to_string(&JsonValue::Object(tool_call.arguments.clone())).unwrap_or_default();
    if rendered.chars().count() > FALLBACK_INVOCATION_ARGS_CHARS {
        rendered = rendered
            .chars()
            .take(FALLBACK_INVOCATION_ARGS_CHARS)
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…";
    }
    format!("{} {rendered}", tool_call.name)
}

fn string_argument(args: &JsonMap, key: &str) -> Option<String> {
    match args.get(key) {
        Some(JsonValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn int_argument(args: &JsonMap, key: &str) -> Option<i64> {
    // A JSON bool is a distinct `Value` variant here, so — unlike Python, where
    // `bool` is an `int` subclass and must be excluded explicitly — it never
    // matches `Number` and needs no special arm.
    match args.get(key) {
        Some(JsonValue::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

fn number_argument(args: &JsonMap, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(JsonValue::Number(n)) => n.as_f64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        let map = match args {
            JsonValue::Object(m) => m,
            _ => JsonMap::new(),
        };
        ToolCall::new("c", name, map)
    }

    #[test]
    fn read_block_has_arrow_and_path() {
        assert_eq!(
            format_tool_call_block(&call("read", serde_json::json!({"path": "a.py"}))),
            "→ read a.py"
        );
    }

    #[test]
    fn read_with_offset_and_limit_suffix() {
        assert_eq!(
            format_tool_call_invocation(&call(
                "read",
                serde_json::json!({"path": "a.py", "offset": 5, "limit": 3})
            )),
            "read a.py:5-7"
        );
    }

    #[test]
    fn bash_block_is_bare_dollar() {
        assert_eq!(
            format_tool_call_block(&call("bash", serde_json::json!({"command": "ls -la"}))),
            "$ ls -la"
        );
    }
}
