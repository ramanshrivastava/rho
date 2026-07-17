//! Approximate context-size estimation for rho coding sessions (port of tau's
//! `tau_coding/context_window.py`).
//!
//! The estimates are deterministic and provider-neutral: they exist to drive
//! Pi-style automatic compaction, not to match any tokenizer. The character
//! counts follow Python's `len()` (Unicode codepoints), and the compaction
//! serializers reproduce tau's `str(dict)` / f-string output byte-for-byte via
//! [`crate::pystr::python_repr`].
#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;

use rho_agent::messages::AgentMessage;
use rho_agent::tools::AgentTool;
use rho_agent::types::JsonValue;

/// Characters per token used by the rough estimator (tau `CHARS_PER_TOKEN`).
pub const CHARS_PER_TOKEN: i64 = 4;
/// Fixed per-message token overhead (tau `MESSAGE_OVERHEAD_TOKENS`).
pub const MESSAGE_OVERHEAD_TOKENS: i64 = 4;
/// Fixed per-tool token overhead (tau `TOOL_OVERHEAD_TOKENS`).
pub const TOOL_OVERHEAD_TOKENS: i64 = 16;
/// Per-line character cap in a deterministic compaction summary
/// (tau `SUMMARY_MESSAGE_CHAR_LIMIT`).
pub const SUMMARY_MESSAGE_CHAR_LIMIT: i64 = 500;
/// Default model context window (tau `DEFAULT_CONTEXT_WINDOW_TOKENS`).
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: i64 = 128_000;
/// Tokens reserved below the window before auto-compaction
/// (tau `DEFAULT_COMPACTION_RESERVE_TOKENS`).
pub const DEFAULT_COMPACTION_RESERVE_TOKENS: i64 = 16_384;
/// Recent-token budget kept verbatim during compaction
/// (tau `DEFAULT_COMPACTION_KEEP_RECENT_TOKENS`).
pub const DEFAULT_COMPACTION_KEEP_RECENT_TOKENS: i64 = 20_000;
/// Prefix marking a stored previous compaction summary
/// (tau `COMPACTION_SUMMARY_PREFIX`).
pub const COMPACTION_SUMMARY_PREFIX: &str = "Previous conversation summary:\n";

/// System prompt for the compaction summarizer (tau `SUMMARIZATION_SYSTEM_PROMPT`).
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Prompt asking the model to build a fresh compaction summary
/// (tau `SUMMARIZATION_PROMPT`).
pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// Prompt asking the model to update an existing summary
/// (tau `UPDATE_SUMMARIZATION_PROMPT`).
pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n- UPDATE \"Next Steps\" based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nUse this EXACT format:\n\n## Goal\n[Preserve existing goals, add new ones if the task expanded]\n\n## Constraints & Preferences\n- [Preserve existing, add new ones discovered]\n\n## Progress\n### Done\n- [x] [Include previously done items AND newly completed items]\n\n### In Progress\n- [ ] [Current work - update based on progress]\n\n### Blocked\n- [Current blockers - remove if resolved]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\n## Next Steps\n1. [Update based on current state]\n\n## Critical Context\n- [Preserve important context, add new if needed]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// Prompt summarizing a too-large turn prefix (tau `TURN_PREFIX_SUMMARIZATION_PROMPT`).
pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.\n\nSummarize the prefix to provide context for the retained suffix:\n\n## Original Request\n[What did the user ask for in this turn?]\n\n## Early Progress\n- [Key decisions and work done in the prefix]\n\n## Context for Suffix\n- [Information needed to understand the retained recent work]\n\nBe concise. Focus on what's needed to understand the kept suffix.";

/// Deterministic context-size accounting for one provider request
/// (tau `ContextUsageEstimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Total estimated tokens.
    pub total_tokens: i64,
    /// Tokens attributed to the system prompt.
    pub system_tokens: i64,
    /// Tokens attributed to the messages.
    pub message_tokens: i64,
    /// Tokens attributed to the tool definitions.
    pub tool_tokens: i64,
    /// Number of messages counted.
    pub message_count: i64,
    /// Number of tools counted.
    pub tool_count: i64,
}

/// Convert a `usize` count to `i64`, saturating (matches Python's unbounded int
/// arithmetic for any realistic input).
fn to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Return a deterministic rough token estimate for text (tau `estimate_text_tokens`).
///
/// Uses Python `len()` semantics: the length is the Unicode **codepoint** count,
/// not the UTF-8 byte count.
#[must_use]
pub fn estimate_text_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let chars = to_i64(text.chars().count());
    std::cmp::max(1, (chars + CHARS_PER_TOKEN - 1) / CHARS_PER_TOKEN)
}

/// Return a rough token estimate for one provider-neutral message
/// (tau `estimate_message_tokens`).
#[must_use]
pub fn estimate_message_tokens(message: &AgentMessage) -> i64 {
    let mut tokens = MESSAGE_OVERHEAD_TOKENS + estimate_text_tokens(&message.text());
    match message {
        AgentMessage::Assistant(assistant) => {
            for call in assistant.tool_calls() {
                let arguments = crate::pystr::python_repr(&JsonValue::Object(call.arguments));
                tokens += estimate_text_tokens(&call.name) + estimate_text_tokens(&arguments);
            }
        }
        AgentMessage::ToolResult(result) => {
            tokens += estimate_text_tokens(&result.tool_name);
        }
        _ => {}
    }
    tokens
}

/// Return a rough token estimate for one tool definition (tau `estimate_tool_tokens`).
#[must_use]
pub fn estimate_tool_tokens(tool: &AgentTool) -> i64 {
    let schema = crate::pystr::python_repr(&JsonValue::Object(tool.input_schema().clone()));
    TOOL_OVERHEAD_TOKENS
        + estimate_text_tokens(&tool.name)
        + estimate_text_tokens(&tool.description)
        + estimate_text_tokens(&schema)
}

/// Return a rough estimate of the active provider context size
/// (tau `estimate_context_tokens`).
#[must_use]
pub fn estimate_context_tokens(
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> i64 {
    estimate_context_usage(system, messages, tools).total_tokens
}

/// Return Pi-style automatic compaction threshold for a model context window
/// (tau `auto_compaction_threshold_for_context_window`).
#[must_use]
pub fn auto_compaction_threshold_for_context_window(context_window_tokens: i64) -> Option<i64> {
    if context_window_tokens <= 0 {
        return None;
    }
    Some(std::cmp::max(
        1,
        context_window_tokens - DEFAULT_COMPACTION_RESERVE_TOKENS,
    ))
}

/// Return deterministic context accounting for the active provider request
/// (tau `estimate_context_usage`).
#[must_use]
pub fn estimate_context_usage(
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> ContextUsageEstimate {
    let system_tokens = estimate_text_tokens(system);
    let message_tokens: i64 = messages.iter().map(estimate_message_tokens).sum();
    let tool_tokens: i64 = tools.iter().map(estimate_tool_tokens).sum();
    ContextUsageEstimate {
        total_tokens: system_tokens + message_tokens + tool_tokens,
        system_tokens,
        message_tokens,
        tool_tokens,
        message_count: to_i64(messages.len()),
        tool_count: to_i64(tools.len()),
    }
}

/// Build a deterministic compact summary from provider-neutral messages
/// (tau `summarize_messages_for_compaction`).
#[must_use]
pub fn summarize_messages_for_compaction(messages: &[AgentMessage]) -> String {
    if messages.is_empty() {
        return "No prior messages.".to_string();
    }
    let mut lines = vec![format!(
        "Automatically compacted {} prior message(s).",
        messages.len()
    )];
    for (offset, message) in messages.iter().enumerate() {
        let index = offset + 1;
        let role = if matches!(message, AgentMessage::ToolResult(_)) {
            "tool"
        } else {
            message.role()
        };
        lines.push(format!(
            "{index}. {role}: {}",
            message_summary_text(message)
        ));
    }
    lines.join("\n")
}

/// Build the model prompt Tau uses to summarize compacted history
/// (tau `build_compaction_summary_prompt`).
#[must_use]
pub fn build_compaction_summary_prompt(
    messages: &[AgentMessage],
    custom_instructions: Option<&str>,
) -> String {
    let (previous_summary, new_messages) = split_previous_compaction_summary(messages);
    let conversation = serialize_messages_for_compaction(new_messages);
    let mut prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    }
    .to_string();

    if let Some(previous) = &previous_summary {
        let _ = write!(
            prompt,
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        );
    }

    let instructions = custom_instructions.map(str::trim).unwrap_or_default();
    if !instructions.is_empty() {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {instructions}");
    }

    format!("{prompt}{base_prompt}")
}

/// Serialize provider-neutral messages for the compaction summarizer
/// (tau `serialize_messages_for_compaction`).
#[must_use]
pub fn serialize_messages_for_compaction(messages: &[AgentMessage]) -> String {
    if messages.is_empty() {
        return "(no new messages)".to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    for (offset, message) in messages.iter().enumerate() {
        let index = offset + 1;
        let mut attributes = format!("index={index} role={}", message.role());
        if let AgentMessage::ToolResult(result) = message {
            let error = if result.is_error { "true" } else { "false" };
            let _ = write!(attributes, " name={} error={error}", result.tool_name);
        }
        lines.push(format!("<message {attributes}>"));
        let text = message.text();
        if !text.is_empty() {
            lines.push(text);
        }
        if let AgentMessage::Assistant(assistant) = message {
            let calls = assistant.tool_calls();
            if !calls.is_empty() {
                lines.push("<tool-calls>".to_string());
                for call in &calls {
                    let arguments =
                        crate::pystr::python_repr(&JsonValue::Object(call.arguments.clone()));
                    lines.push(format!("- {}: {arguments}", call.name));
                }
                lines.push("</tool-calls>".to_string());
            }
        }
        lines.push("</message>".to_string());
    }
    lines.join("\n")
}

/// One-line role-tagged text for a message inside a deterministic summary
/// (tau `_message_text`).
fn message_summary_text(message: &AgentMessage) -> String {
    let mut text = message.text();
    match message {
        AgentMessage::Assistant(assistant) if !assistant.tool_calls().is_empty() => {
            let names = assistant
                .tool_calls()
                .iter()
                .map(|call| call.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            text = format!("{text} [tool calls: {names}]");
        }
        AgentMessage::ToolResult(result) => {
            let status = if result.is_error { "failed" } else { "ok" };
            text = format!("{} {status}: {text}", result.tool_name);
        }
        _ => {}
    }
    truncate_summary_text(&text)
}

/// Collapse whitespace and cap a summary line at [`SUMMARY_MESSAGE_CHAR_LIMIT`]
/// characters (tau `_truncate_summary_text`).
fn truncate_summary_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if to_i64(collapsed.chars().count()) <= SUMMARY_MESSAGE_CHAR_LIMIT {
        return collapsed;
    }
    let keep = usize::try_from(SUMMARY_MESSAGE_CHAR_LIMIT - 3).unwrap_or(0);
    let head: String = collapsed.chars().take(keep).collect();
    format!("{}...", crate::pystr::py_rstrip(&head))
}

/// Split a leading stored compaction summary from the remaining messages
/// (tau `_split_previous_compaction_summary`).
fn split_previous_compaction_summary(
    messages: &[AgentMessage],
) -> (Option<String>, &[AgentMessage]) {
    let Some(AgentMessage::User(first)) = messages.first() else {
        return (None, messages);
    };
    let text = first.text();
    let Some(rest) = text.strip_prefix(COMPACTION_SUMMARY_PREFIX) else {
        return (None, messages);
    };
    (Some(rest.to_string()), &messages[1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_agent::messages::{
        AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
        ToolResultMessage, UserMessage,
    };
    use rho_agent::types::JsonMap;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User(UserMessage::new(text))
    }

    fn assistant_text(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage::new(vec![AssistantContent::Text(
            TextContent::new(text),
        )]))
    }

    fn args(pairs: &[(&str, &str)]) -> JsonMap {
        let mut map = JsonMap::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), JsonValue::String((*value).to_string()));
        }
        map
    }

    fn assistant_with_call(text: &str, call: ToolCall) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage::new(vec![
            AssistantContent::Text(TextContent::new(text)),
            AssistantContent::ToolCall(call),
        ]))
    }

    fn tool_result(call_id: &str, name: &str, text: &str) -> AgentMessage {
        AgentMessage::ToolResult(ToolResultMessage::new(
            call_id,
            name,
            vec![ToolResultContent::Text(TextContent::new(text))],
        ))
    }

    fn coding_tools(cwd: &std::path::Path) -> Vec<AgentTool> {
        crate::tools::create_coding_tools(cwd, None)
    }

    #[test]
    fn text_token_estimate_is_deterministic() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
    }

    #[test]
    fn message_token_estimate_counts_roles_and_tool_calls() {
        let tool_call = ToolCall::new("call-1", "read", args(&[("path", "README.md")]));

        let user_tokens = estimate_message_tokens(&user("hello"));
        let assistant_tokens =
            estimate_message_tokens(&assistant_with_call("using tool", tool_call));
        let tool_tokens = estimate_message_tokens(&tool_result("call-1", "read", "contents"));

        assert!(user_tokens > estimate_text_tokens("hello"));
        assert!(assistant_tokens > user_tokens);
        assert!(tool_tokens > estimate_text_tokens("contents"));
    }

    #[test]
    fn context_token_estimate_includes_system_messages_and_tools() {
        let dir = tempfile::tempdir().unwrap();
        let tools = coding_tools(dir.path());
        let messages = [user("hello"), assistant_text("hi")];

        let estimate = estimate_context_tokens("You are Tau.", &messages, &tools);

        assert!(estimate > estimate_text_tokens("You are Tau.hellohi"));
    }

    #[test]
    fn auto_compaction_threshold_keeps_pi_style_reserve() {
        assert_eq!(
            auto_compaction_threshold_for_context_window(128_000),
            Some(111_616)
        );
        assert_eq!(
            auto_compaction_threshold_for_context_window(16_384),
            Some(1)
        );
        assert_eq!(auto_compaction_threshold_for_context_window(0), None);
    }

    #[test]
    fn context_usage_estimate_reports_breakdown() {
        let dir = tempfile::tempdir().unwrap();
        let tools = coding_tools(dir.path());
        let messages = [user("hello"), assistant_text("hi")];

        let usage = estimate_context_usage("You are Tau.", &messages, &tools);

        assert_eq!(usage.message_count, 2);
        assert_eq!(usage.tool_count, to_i64(tools.len()));
        assert_eq!(usage.system_tokens, estimate_text_tokens("You are Tau."));
        assert_eq!(
            usage.message_tokens,
            messages.iter().map(estimate_message_tokens).sum::<i64>()
        );
        assert_eq!(
            usage.total_tokens,
            usage.system_tokens + usage.message_tokens + usage.tool_tokens
        );
        assert_eq!(
            estimate_context_tokens("You are Tau.", &messages, &tools),
            usage.total_tokens
        );
    }

    #[test]
    fn summarize_messages_for_compaction_is_deterministic() {
        let tool_call = ToolCall::new("call-1", "read", args(&[("path", "README.md")]));
        let messages = [
            user("Read README.md"),
            assistant_with_call("I'll inspect it.", tool_call),
            tool_result("call-1", "read", "README contents"),
        ];

        let summary = summarize_messages_for_compaction(&messages);

        assert_eq!(
            summary,
            [
                "Automatically compacted 3 prior message(s).",
                "1. user: Read README.md",
                "2. assistant: I'll inspect it. [tool calls: read]",
                "3. tool: read ok: README contents",
            ]
            .join("\n")
        );
    }

    #[test]
    fn compaction_summary_prompt_uses_pi_format_and_custom_instructions() {
        let messages = [
            user("Refactor src/app.py"),
            assistant_text("Updated src/app.py"),
        ];
        let prompt = build_compaction_summary_prompt(&messages, Some("Focus on files changed."));

        assert!(prompt.contains("<conversation>"));
        assert!(prompt.contains("Use this EXACT format:"));
        assert!(prompt.contains("## Goal"));
        assert!(prompt.contains("Preserve exact file paths"));
        assert!(prompt.contains("Additional focus: Focus on files changed."));
        assert!(prompt.contains("Refactor src/app.py"));
    }

    #[test]
    fn compaction_summary_prompt_updates_previous_summary() {
        let messages = [
            user("Previous conversation summary:\n## Goal\nShip compaction."),
            user("Now add tests."),
        ];
        let prompt = build_compaction_summary_prompt(&messages, None);

        assert!(
            prompt.contains("<previous-summary>\n## Goal\nShip compaction.\n</previous-summary>")
        );
        assert!(prompt.contains("NEW conversation messages"));
        assert!(prompt.contains("Now add tests."));
        assert!(
            !serialize_messages_for_compaction(&[user("Now add tests.")])
                .contains("Previous conversation summary")
        );
    }
}
