//! Model-assisted summaries for abandoned session-tree branches (port of tau's
//! `tau_coding/branch_summary.py`).
//!
//! The pure helpers here serialize a branch conversation into a summarizer
//! prompt and post-process the model's answer with the branch's file
//! operations. [`summarize_branch_messages_with_model`] drives one streamed
//! model call and returns `None` on any failure (matching tau's
//! error-is-`None` contract).
//!
//! Parity note: [`format_tool_call_arguments`] reproduces Python's
//! `json.dumps(value, sort_keys=True)` — sorted keys, `ensure_ascii` escaping of
//! non-ASCII codepoints, and the default `", "` / `": "` separators (which carry
//! a space, unlike a compact JSON dump).
#![allow(clippy::doc_markdown)]

use std::collections::HashSet;
use std::fmt::Write as _;

use rho_agent::messages::{AgentMessage, AssistantMessage};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::types::{JsonMap, JsonValue};

use futures::StreamExt as _;

/// System prompt for the branch summarizer (tau `BRANCH_SUMMARY_SYSTEM_PROMPT`).
pub const BRANCH_SUMMARY_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Preamble prepended to a rendered branch summary (tau `BRANCH_SUMMARY_PREAMBLE`).
pub const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// Structured-format instructions for the branch summarizer
/// (tau `BRANCH_SUMMARY_PROMPT`).
pub const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context
when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Per-message source cap for the summarizer input (tau `MAX_SUMMARY_SOURCE_MESSAGE_CHARS`).
pub const MAX_SUMMARY_SOURCE_MESSAGE_CHARS: i64 = 4_000;
/// Total source cap for the summarizer input (tau `MAX_SUMMARY_SOURCE_TOTAL_CHARS`).
pub const MAX_SUMMARY_SOURCE_TOTAL_CHARS: i64 = 60_000;
/// Per-tool-result source cap (tau `TOOL_RESULT_MAX_CHARS`).
pub const TOOL_RESULT_MAX_CHARS: i64 = 2_000;

/// Convert a `usize` count to `i64`, saturating.
fn to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Return a model-generated branch summary, or `None` when generation fails
/// (tau `summarize_branch_messages_with_model`).
pub async fn summarize_branch_messages_with_model(
    provider: &dyn ModelProvider,
    model: &str,
    messages: &[AgentMessage],
    custom_instructions: Option<&str>,
    replace_instructions: bool,
) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let prompt = branch_summary_prompt(messages, custom_instructions, replace_instructions);
    let request = [AgentMessage::User(rho_agent::messages::UserMessage::new(
        prompt,
    ))];

    let mut stream =
        provider.stream_response(model, BRANCH_SUMMARY_SYSTEM_PROMPT, &request, &[], None);

    let mut response: Option<AssistantMessage> = None;
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Error(_) => return None,
            AssistantMessageEvent::Done(done) => response = Some(done.message.clone()),
            _ => {}
        }
    }

    let response = response?;
    let summary = response.text();
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }
    Some(add_branch_summary_context(summary, messages))
}

/// Build the branch summarizer prompt (tau `_branch_summary_prompt`).
fn branch_summary_prompt(
    messages: &[AgentMessage],
    custom_instructions: Option<&str>,
    replace_instructions: bool,
) -> String {
    let conversation = serialize_branch_conversation(messages);
    let custom = custom_instructions.filter(|value| !value.is_empty());
    let instructions = if replace_instructions {
        match custom {
            Some(value) => value.to_string(),
            None => BRANCH_SUMMARY_PROMPT.to_string(),
        }
    } else if let Some(value) = custom {
        format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {value}")
    } else {
        BRANCH_SUMMARY_PROMPT.to_string()
    };
    format!("<conversation>\n{conversation}\n</conversation>\n\n{instructions}")
}

/// Serialize a branch conversation under the total-character budget
/// (tau `_serialize_branch_conversation`).
fn serialize_branch_conversation(messages: &[AgentMessage]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut remaining_chars = MAX_SUMMARY_SOURCE_TOTAL_CHARS;
    let mut omitted_count: usize = 0;

    for (offset, message) in messages.iter().enumerate() {
        let index = offset + 1;
        let rendered = format_summary_source_message(message);
        if to_i64(rendered.chars().count()) > remaining_chars {
            omitted_count = messages.len() - index + 1;
            break;
        }
        remaining_chars -= to_i64(rendered.chars().count());
        parts.push(rendered);
    }

    if omitted_count > 0 {
        parts.push(format!(
            "[... {omitted_count} message(s) omitted because the branch was too long]"
        ));
    }

    parts.join("\n\n")
}

/// Render one source message for the summarizer (tau `_format_summary_source_message`).
fn format_summary_source_message(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(user) => format!(
            "[User]: {}",
            trim_summary_source_text(&user.text(), MAX_SUMMARY_SOURCE_MESSAGE_CHARS)
        ),
        AgentMessage::Assistant(assistant) => format_assistant_summary_source(assistant),
        AgentMessage::ToolResult(result) => {
            let status = if result.is_error { "failed" } else { "ok" };
            let content = trim_summary_source_text(&result.text(), TOOL_RESULT_MAX_CHARS);
            format!("[Tool result: {} ({status})]: {content}", result.tool_name)
        }
        other => format!(
            "[{}]: {}",
            other.role(),
            trim_summary_source_text(&other.text(), MAX_SUMMARY_SOURCE_MESSAGE_CHARS)
        ),
    }
}

/// Render an assistant message with its tool calls (tau `_format_assistant_summary_source`).
fn format_assistant_summary_source(message: &AssistantMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let content = trim_summary_source_text(&message.text(), MAX_SUMMARY_SOURCE_MESSAGE_CHARS);
    if content != "(empty)" {
        parts.push(format!("[Assistant]: {content}"));
    }
    let calls = message.tool_calls();
    if !calls.is_empty() {
        let rendered = calls
            .iter()
            .map(|call| {
                format!(
                    "{}({})",
                    call.name,
                    format_tool_call_arguments(&call.arguments)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!("[Assistant tool calls]: {rendered}"));
    }
    if parts.is_empty() {
        "[Assistant]: (empty)".to_string()
    } else {
        parts.join("\n")
    }
}

/// Format tool-call arguments as `key=json.dumps(value, sort_keys=True)` pairs,
/// keys sorted (tau `_format_tool_call_arguments`).
fn format_tool_call_arguments(arguments: &JsonMap) -> String {
    let mut keys: Vec<&String> = arguments.keys().collect();
    keys.sort();
    keys.iter()
        .map(|key| {
            let mut value = String::new();
            write_json_sorted(&arguments[*key], &mut value);
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Serialize a JSON value like Python `json.dumps(value, sort_keys=True)`:
/// sorted object keys, `ensure_ascii` escaping, and `", "` / `": "` separators.
fn write_json_sorted(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(number) => out.push_str(&number.to_string()),
        JsonValue::String(text) => write_json_string(text, out),
        JsonValue::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_json_sorted(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_json_string(key, out);
                out.push_str(": ");
                write_json_sorted(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// Write a JSON string literal with Python `json` escaping (`ensure_ascii=True`).
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (u32::from(c)) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let cp = u32::from(c);
                if cp <= 0xFFFF {
                    let _ = write!(out, "\\u{cp:04x}");
                } else {
                    let value = cp - 0x10000;
                    let high = 0xD800 + (value >> 10);
                    let low = 0xDC00 + (value & 0x3FF);
                    let _ = write!(out, "\\u{high:04x}\\u{low:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Trim and cap source text, appending a truncation note (tau `_trim_summary_source_text`).
fn trim_summary_source_text(text: &str, max_chars: i64) -> String {
    let trimmed = text.trim();
    let normalized = if trimmed.is_empty() {
        "(empty)".to_string()
    } else {
        trimmed.to_string()
    };
    let length = to_i64(normalized.chars().count());
    if length <= max_chars {
        return normalized;
    }
    let truncated_chars = length - max_chars;
    let keep = usize::try_from(max_chars).unwrap_or(0);
    let head: String = normalized.chars().take(keep).collect();
    format!(
        "{}\n\n[... {truncated_chars} more characters truncated]",
        crate::pystr::py_rstrip(&head)
    )
}

/// Append read/modified file context to a rendered summary (tau `_add_branch_summary_context`).
fn add_branch_summary_context(summary: &str, messages: &[AgentMessage]) -> String {
    let (read_files, modified_files) = branch_file_operations(messages);
    let mut sections = vec![format!("{BRANCH_SUMMARY_PREAMBLE}{summary}")];
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    sections.join("\n\n")
}

/// Return `(read-only files, modified files)` from the branch's tool calls
/// (tau `_branch_file_operations`).
fn branch_file_operations(messages: &[AgentMessage]) -> (Vec<String>, Vec<String>) {
    let mut read: HashSet<String> = HashSet::new();
    let mut modified: HashSet<String> = HashSet::new();
    for message in messages {
        let AgentMessage::Assistant(assistant) = message else {
            continue;
        };
        for call in assistant.tool_calls() {
            let Some(JsonValue::String(path)) = call.arguments.get("path") else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            if call.name == "read" {
                read.insert(path.clone());
            } else if call.name == "edit" || call.name == "write" {
                modified.insert(path.clone());
            }
        }
    }
    let mut read_only: Vec<String> = read
        .iter()
        .filter(|path| !modified.contains(*path))
        .cloned()
        .collect();
    read_only.sort();
    let mut modified_files: Vec<String> = modified.into_iter().collect();
    modified_files.sort();
    (read_only, modified_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_agent::messages::{
        AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
        ToolResultMessage, UserMessage,
    };
    use rho_agent::provider_events::{AssistantDoneEvent, DoneReason};
    use rho_ai::FakeProvider;
    use serde_json::json;

    fn args(value: JsonValue) -> JsonMap {
        match value {
            JsonValue::Object(map) => map,
            _ => unreachable!("test helper expects a JSON object"),
        }
    }

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User(UserMessage::new(text))
    }

    fn assistant(text: &str, calls: Vec<ToolCall>) -> AgentMessage {
        let mut content = vec![AssistantContent::Text(TextContent::new(text))];
        for call in calls {
            content.push(AssistantContent::ToolCall(call));
        }
        AgentMessage::Assistant(AssistantMessage::new(content))
    }

    #[test]
    fn tool_call_arguments_sorts_keys_and_uses_json_dumps_spacing() {
        let arguments = args(json!({"path": "src/app.py", "count": 2, "flag": true}));
        assert_eq!(
            format_tool_call_arguments(&arguments),
            "count=2, flag=true, path=\"src/app.py\""
        );
    }

    #[test]
    fn tool_call_arguments_serializes_nested_values_with_sorted_keys() {
        let arguments = args(json!({"opts": {"b": 1, "a": 2}, "items": [1, 2]}));
        assert_eq!(
            format_tool_call_arguments(&arguments),
            "items=[1, 2], opts={\"a\": 2, \"b\": 1}"
        );
    }

    #[test]
    fn tool_call_arguments_escapes_non_ascii() {
        let arguments = args(json!({"note": "café"}));
        assert_eq!(
            format_tool_call_arguments(&arguments),
            "note=\"caf\\u00e9\""
        );
    }

    #[test]
    fn branch_file_operations_tracks_read_and_modified() {
        let messages = [
            assistant(
                "",
                vec![ToolCall::new("c1", "read", args(json!({"path": "a.py"})))],
            ),
            assistant(
                "",
                vec![ToolCall::new("c2", "edit", args(json!({"path": "b.py"})))],
            ),
            assistant(
                "",
                vec![ToolCall::new("c3", "write", args(json!({"path": "a.py"})))],
            ),
            assistant(
                "",
                vec![ToolCall::new("c4", "read", args(json!({"path": "c.py"})))],
            ),
        ];

        let (read_only, modified) = branch_file_operations(&messages);
        // a.py is both read and written -> classified as modified only.
        assert_eq!(read_only, vec!["c.py".to_string()]);
        assert_eq!(modified, vec!["a.py".to_string(), "b.py".to_string()]);
    }

    #[test]
    fn branch_file_operations_ignores_non_string_and_empty_paths() {
        let messages = [assistant(
            "",
            vec![
                ToolCall::new("c1", "read", args(json!({"path": ""}))),
                ToolCall::new("c2", "read", args(json!({"path": 5}))),
            ],
        )];
        let (read_only, modified) = branch_file_operations(&messages);
        assert!(read_only.is_empty());
        assert!(modified.is_empty());
    }

    #[test]
    fn trim_summary_source_text_marks_empty_and_truncates() {
        assert_eq!(trim_summary_source_text("   ", 10), "(empty)");
        assert_eq!(trim_summary_source_text("hello", 10), "hello");

        let long = "a".repeat(20);
        let trimmed = trim_summary_source_text(&long, 5);
        assert_eq!(trimmed, "aaaaa\n\n[... 15 more characters truncated]");
    }

    #[test]
    fn branch_summary_prompt_wraps_conversation_and_instructions() {
        let messages = [user("Refactor src/app.py")];
        let prompt = branch_summary_prompt(&messages, None, false);
        assert!(prompt.starts_with("<conversation>\n"));
        assert!(prompt.contains("[User]: Refactor src/app.py"));
        assert!(prompt.contains("## Goal"));
        assert!(prompt.trim_end().ends_with("error messages."));
    }

    #[test]
    fn branch_summary_prompt_appends_custom_instructions() {
        let messages = [user("hi")];
        let prompt = branch_summary_prompt(&messages, Some("Focus on tests."), false);
        assert!(prompt.contains("Additional focus: Focus on tests."));
    }

    #[test]
    fn branch_summary_prompt_replaces_instructions() {
        let messages = [user("hi")];
        let prompt = branch_summary_prompt(&messages, Some("Only this."), true);
        assert!(prompt.ends_with("</conversation>\n\nOnly this."));
        assert!(!prompt.contains("## Goal"));
    }

    #[test]
    fn assistant_summary_source_renders_text_and_calls() {
        let AgentMessage::Assistant(message) = assistant(
            "Working on it",
            vec![ToolCall::new("c1", "read", args(json!({"path": "a.py"})))],
        ) else {
            unreachable!()
        };
        let rendered = format_assistant_summary_source(&message);
        assert_eq!(
            rendered,
            "[Assistant]: Working on it\n[Assistant tool calls]: read(path=\"a.py\")"
        );
    }

    #[test]
    fn add_branch_summary_context_appends_file_sections() {
        let messages = [
            assistant(
                "",
                vec![ToolCall::new("c1", "read", args(json!({"path": "a.py"})))],
            ),
            assistant(
                "",
                vec![ToolCall::new("c2", "write", args(json!({"path": "b.py"})))],
            ),
        ];
        let rendered = add_branch_summary_context("SUMMARY", &messages);
        assert!(rendered.starts_with(BRANCH_SUMMARY_PREAMBLE));
        assert!(rendered.contains("<read-files>\na.py\n</read-files>"));
        assert!(rendered.contains("<modified-files>\nb.py\n</modified-files>"));
    }

    #[tokio::test]
    async fn summarize_branch_returns_none_when_empty() {
        let provider = FakeProvider::new(vec![]);
        let result =
            summarize_branch_messages_with_model(&provider, "model", &[], None, false).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn summarize_branch_uses_done_message_and_appends_context() {
        let done = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new(
            "  ## Goal\nDo the thing.  ",
        ))]);
        let provider = FakeProvider::new(vec![vec![AssistantMessageEvent::Done(
            AssistantDoneEvent::new(DoneReason::Stop, done),
        )]]);
        let messages = [
            user("Refactor a.py"),
            assistant(
                "",
                vec![ToolCall::new("c1", "read", args(json!({"path": "a.py"})))],
            ),
        ];

        let result =
            summarize_branch_messages_with_model(&provider, "model", &messages, None, false).await;

        let summary = result.expect("summary");
        assert!(summary.starts_with(BRANCH_SUMMARY_PREAMBLE));
        assert!(summary.contains("## Goal\nDo the thing."));
        assert!(summary.contains("<read-files>\na.py\n</read-files>"));
    }

    #[tokio::test]
    async fn summarize_branch_returns_none_on_empty_summary_text() {
        let done = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new("   "))]);
        let provider = FakeProvider::new(vec![vec![AssistantMessageEvent::Done(
            AssistantDoneEvent::new(DoneReason::Stop, done),
        )]]);
        let messages = [user("hi")];
        let result =
            summarize_branch_messages_with_model(&provider, "model", &messages, None, false).await;
        assert!(result.is_none());
    }

    // Silence unused-import warnings for the tool-result constructors that the
    // helper tests do not exercise directly.
    #[allow(dead_code)]
    fn _tool_result_ctor_ref() -> AgentMessage {
        AgentMessage::ToolResult(ToolResultMessage::new(
            "id",
            "read",
            vec![ToolResultContent::Text(TextContent::new("x"))],
        ))
    }
}
