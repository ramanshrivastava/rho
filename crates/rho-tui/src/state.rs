//! Display state for rho's TUI (port of tau `tau_coding/tui/state.py`).
//!
//! [`TuiState`] is the pure, immediate-mode-friendly display model: a flat list
//! of [`ChatItem`]s plus streaming/queue/toggle flags. The ratatui frontend
//! rebuilds each frame from this state, and the [`crate::adapter::TuiEventAdapter`]
//! is the only thing that mutates it in response to session events. Ported 1:1
//! from tau; the formatters (`format_tool_call_block`, `format_tool_result_block`,
//! …) are byte-identical so the transcript reads the same as tau's.
//!
//! Extension markup resolvers (`custom_renderer` / `tool_call_renderer` /
//! `tool_result_renderer`) are carried for structural parity but never installed
//! in M5 — the WASM extension runtime is M7 — so `resolve_*` always falls back to
//! the generic text, exactly as tau does before its extension runtime connects.

use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rho_agent::messages::{AgentMessage, ToolCall};
use rho_agent::tools::AgentToolResult;
use rho_agent::types::{JsonMap, JsonValue};
use rho_coding::skills::{Skill, parse_skill_invocation};

use crate::pystr;

/// One transcript role (tau's `ChatItemRole` literal union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatItemRole {
    /// A user-authored message.
    User,
    /// An assistant text block.
    Assistant,
    /// A tool call + result block.
    Tool,
    /// An error block.
    Error,
    /// A transient status line (retries, notices).
    Status,
    /// A thinking/reasoning block.
    Thinking,
    /// A skill load/use marker.
    Skill,
    /// A branch-summary marker.
    BranchSummary,
    /// A compaction-summary marker.
    CompactionSummary,
    /// An extension-owned custom message.
    Custom,
}

impl ChatItemRole {
    /// The tau string form (used for theme role-style lookup and snapshots).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Error => "error",
            Self::Status => "status",
            Self::Thinking => "thinking",
            Self::Skill => "skill",
            Self::BranchSummary => "branch_summary",
            Self::CompactionSummary => "compaction_summary",
            Self::Custom => "custom",
        }
    }
}

/// Number of tool-result preview lines shown collapsed.
pub const TOOL_RESULT_PREVIEW_LINES: usize = 8;
/// Number of edit-patch preview lines shown collapsed.
pub const TOOL_PATCH_PREVIEW_LINES: usize = 32;
/// Character cap on a collapsed tool-result preview.
pub const TOOL_RESULT_PREVIEW_CHARS: usize = 2_000;
/// Line cap on an input-bar terminal command's visible output.
pub const TERMINAL_COMMAND_OUTPUT_PREVIEW_LINES: usize = 120;
/// Braille spinner frames shown on an executing tool row.
pub const TOOL_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Static invocation markers the spinner stands in for while a tool runs.
const INVOCATION_MARKERS: [&str; 2] = ["→ ", "▸ "];
/// Show the live elapsed timer once a tool stops being instant.
pub const TOOL_TIMER_MIN_SECONDS: f64 = 1.0;
const FALLBACK_INVOCATION_ARGS_CHARS: usize = 160;

/// Resolver for extension-owned custom-message markup (tau `CustomMessageMarkup`).
pub type CustomMessageMarkup =
    Arc<dyn Fn(&str, &str, Option<&JsonValue>, bool) -> Option<String> + Send + Sync>;
/// Resolver for a tool's invocation line (tau `ToolCallMarkup`).
pub type ToolCallMarkup = Arc<dyn Fn(&str, &JsonMap) -> Option<String> + Send + Sync>;
/// Resolver for a tool's result block (tau `ToolResultMarkup`).
pub type ToolResultMarkup =
    Arc<dyn Fn(&str, &AgentToolResult, bool) -> Option<String> + Send + Sync>;

/// One rendered item in the TUI transcript (tau `ChatItem`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatItem {
    /// The item's role.
    pub role: ChatItemRole,
    /// The primary display text.
    pub text: String,
    /// Originating tool-call id (tool/skill rows).
    pub tool_call_id: Option<String>,
    /// The formatted result block, once the tool completes.
    pub tool_result_text: Option<String>,
    /// The raw result object, kept for lazy `render_result`.
    pub tool_result: Option<AgentToolResult>,
    /// Live progress text attached to a pending tool.
    pub update_text: Option<String>,
    /// The tool name (for the lazy invocation renderer).
    pub tool_name: Option<String>,
    /// The tool arguments (for the lazy invocation renderer).
    pub tool_arguments: Option<JsonMap>,
    /// Monotonic start time (drives the elapsed timer).
    pub started_at: Option<Instant>,
    /// Whether the result block is always shown expanded.
    pub always_show_tool_result: bool,
    /// The extension custom-message subtype.
    pub custom_type: Option<String>,
    /// Free-form details for a custom message.
    pub details: Option<JsonValue>,
}

impl ChatItem {
    pub(crate) fn new(role: ChatItemRole, text: String) -> Self {
        Self {
            role,
            text,
            tool_call_id: None,
            tool_result_text: None,
            tool_result: None,
            update_text: None,
            tool_name: None,
            tool_arguments: None,
            started_at: None,
            always_show_tool_result: false,
            custom_type: None,
            details: None,
        }
    }
}

/// Mutable display state for the interactive TUI (tau `TuiState`).
#[derive(Default)]
pub struct TuiState {
    /// The transcript items in render order.
    pub items: Vec<ChatItem>,
    /// The in-progress assistant text buffer (flushed on tool/turn boundaries).
    pub assistant_buffer: String,
    /// Whether the agent is currently running.
    pub running: bool,
    /// The last error message, if any.
    pub error: Option<String>,
    /// Whether tool results are expanded.
    pub show_tool_results: bool,
    /// Whether thinking blocks are shown.
    pub show_thinking: bool,
    /// Queued steering messages.
    pub queued_steering: Vec<String>,
    /// Queued follow-up messages.
    pub queued_follow_up: Vec<String>,
    /// Loaded skills (presentation-only path matching).
    pub skills: Vec<Skill>,
    /// Extension custom-message resolver (never set before M7).
    pub custom_renderer: Option<CustomMessageMarkup>,
    /// Extension tool-invocation resolver (never set before M7).
    pub tool_call_renderer: Option<ToolCallMarkup>,
    /// Extension tool-result resolver (never set before M7).
    pub tool_result_renderer: Option<ToolResultMarkup>,
    /// The current spinner frame while a tool runs.
    pub tool_spinner: Option<String>,
}

impl std::fmt::Debug for TuiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiState")
            .field("items", &self.items)
            .field("assistant_buffer", &self.assistant_buffer)
            .field("running", &self.running)
            .field("error", &self.error)
            .field("show_tool_results", &self.show_tool_results)
            .field("show_thinking", &self.show_thinking)
            .field("queued_steering", &self.queued_steering)
            .field("queued_follow_up", &self.queued_follow_up)
            .field("skills", &self.skills)
            .field("tool_spinner", &self.tool_spinner)
            .finish_non_exhaustive()
    }
}

impl TuiState {
    /// A fresh empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a transcript item (tau `add_item`).
    pub fn add_item(&mut self, role: ChatItemRole, text: impl Into<String>) {
        self.items.push(ChatItem::new(role, text.into()));
    }

    fn add_item_full(
        &mut self,
        role: ChatItemRole,
        text: impl Into<String>,
        tool_call_id: Option<String>,
        tool_result_text: Option<String>,
        custom_type: Option<String>,
        details: Option<JsonValue>,
    ) {
        let mut item = ChatItem::new(role, text.into());
        item.tool_call_id = tool_call_id;
        item.tool_result_text = tool_result_text;
        item.custom_type = custom_type;
        item.details = details;
        self.items.push(item);
    }

    /// Render a custom item's markup via the installed resolver, or `None`.
    #[must_use]
    pub fn resolve_custom_markup(&self, item: &ChatItem, expanded: bool) -> Option<String> {
        if item.role != ChatItemRole::Custom {
            return None;
        }
        let custom_type = item.custom_type.as_deref()?;
        let renderer = self.custom_renderer.as_ref()?;
        renderer(custom_type, &item.text, item.details.as_ref(), expanded)
    }

    /// Render a tool item's invocation, applying the spinner/timer (tau
    /// `resolve_tool_invocation`).
    #[must_use]
    pub fn resolve_tool_invocation(&self, item: &ChatItem) -> Option<String> {
        if item.role != ChatItemRole::Tool {
            return None;
        }
        let mut line: Option<String> = None;
        if let (Some(name), Some(renderer)) = (&item.tool_name, &self.tool_call_renderer) {
            line = renderer(
                name,
                item.tool_arguments.as_ref().unwrap_or(&JsonMap::new()),
            );
        }
        if let Some(frame) = &self.tool_spinner {
            if item.tool_result_text.is_none() {
                let base = line.unwrap_or_else(|| item.text.clone());
                let mut spun = apply_tool_spinner(&base, frame);
                if let Some(started) = item.started_at {
                    let elapsed = started.elapsed().as_secs_f64();
                    if elapsed >= TOOL_TIMER_MIN_SECONDS {
                        spun = format!("{spun} ({})", format_elapsed(elapsed));
                    }
                }
                return Some(spun);
            }
        }
        line
    }

    /// Render a tool item's result via its tool's resolver, or `None`.
    #[must_use]
    pub fn resolve_tool_result(&self, item: &ChatItem, expanded: bool) -> Option<String> {
        if item.role != ChatItemRole::Tool {
            return None;
        }
        let result = item.tool_result.as_ref()?;
        let renderer = self.tool_result_renderer.as_ref()?;
        let name = item.tool_name.as_deref()?;
        renderer(name, result, expanded)
    }

    /// Append a collapsed tool-call item (tau `add_tool_call`).
    pub fn add_tool_call(&mut self, tool_call: &ToolCall) {
        if let Some(skill_name) = self.read_skill_name(tool_call) {
            self.add_item_full(
                ChatItemRole::Skill,
                format!("Loading skill: {skill_name}"),
                Some(tool_call.id.clone()),
                None,
                None,
                None,
            );
            return;
        }
        let mut item = ChatItem::new(ChatItemRole::Tool, format_tool_call_block(tool_call));
        item.tool_call_id = Some(tool_call.id.clone());
        item.tool_name = Some(tool_call.name.clone());
        item.tool_arguments = Some(tool_call.arguments.clone());
        item.started_at = Some(Instant::now());
        self.items.push(item);
    }

    /// Append a user-authored message, compacting skill/summary messages (tau
    /// `add_user_message`).
    pub fn add_user_message(
        &mut self,
        content: &str,
        custom_type: Option<&str>,
        details: Option<JsonValue>,
    ) {
        if let Some(custom_type) = custom_type {
            self.add_item_full(
                ChatItemRole::Custom,
                content,
                None,
                None,
                Some(custom_type.to_string()),
                details,
            );
            return;
        }

        if let Some(branch_summary) = parse_branch_summary_message(content) {
            self.add_item_full(
                ChatItemRole::BranchSummary,
                "Branch summary (Ctrl+O to expand)",
                None,
                Some(branch_summary.to_string()),
                None,
                None,
            );
            return;
        }

        if let Some(compaction_summary) = parse_compaction_summary_message(content) {
            self.add_item_full(
                ChatItemRole::CompactionSummary,
                "Compaction summary (Ctrl+O to expand)",
                None,
                Some(compaction_summary.to_string()),
                None,
                None,
            );
            return;
        }

        match parse_skill_invocation(content) {
            None => self.add_item(ChatItemRole::User, content),
            Some(invocation) => {
                self.add_item(
                    ChatItemRole::Skill,
                    format!("Using skill: {}", invocation.name),
                );
                if let Some(extra) = invocation.additional_instructions {
                    if !extra.is_empty() {
                        self.add_item(ChatItemRole::User, extra);
                    }
                }
            }
        }
    }

    /// Append a thinking fragment to the current thinking block (tau
    /// `add_thinking_delta`).
    pub fn add_thinking_delta(&mut self, delta: &str) {
        if let Some(last) = self.items.last_mut() {
            if last.role == ChatItemRole::Thinking {
                last.text.push_str(delta);
                return;
            }
        }
        self.add_item(ChatItemRole::Thinking, delta);
    }

    /// The transcript item for a tool-call id, or `None` (tau `find_tool_item`).
    #[must_use]
    pub fn find_tool_item(&self, tool_call_id: &str) -> Option<&ChatItem> {
        self.items.iter().rev().find(|item| {
            matches!(item.role, ChatItemRole::Tool | ChatItemRole::Skill)
                && item.tool_call_id.as_deref() == Some(tool_call_id)
        })
    }

    /// Attach live progress to its pending tool call; drop orphan updates (tau
    /// `record_tool_update`). Returns whether an item was updated.
    pub fn record_tool_update(&mut self, tool_call_id: &str, message: &str) -> bool {
        for item in self.items.iter_mut().rev() {
            if matches!(item.role, ChatItemRole::Tool | ChatItemRole::Skill)
                && item.tool_call_id.as_deref() == Some(tool_call_id)
            {
                if item.tool_result_text.is_some() {
                    return false;
                }
                item.update_text = Some(message.to_string());
                return true;
            }
        }
        false
    }

    /// Attach a Pi-compatible tool result to its matching call (tau
    /// `record_tool_result`).
    pub fn record_tool_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        result: AgentToolResult,
        is_error: bool,
    ) {
        let data = match &result.details {
            Some(JsonValue::Object(map)) => Some(map.clone()),
            _ => None,
        };
        let result_text =
            format_tool_result_block(tool_name, !is_error, &result.text(), data.as_ref());
        for item in self.items.iter_mut().rev() {
            if matches!(item.role, ChatItemRole::Tool | ChatItemRole::Skill)
                && item.tool_call_id.as_deref() == Some(tool_call_id)
            {
                item.tool_result_text = Some(result_text);
                item.tool_result = Some(result);
                item.update_text = None;
                return;
            }
        }
        let mut item = ChatItem::new(
            ChatItemRole::Tool,
            format_tool_result_summary(tool_name, !is_error),
        );
        item.tool_call_id = Some(tool_call_id.to_string());
        item.tool_result_text = Some(result_text);
        item.tool_result = Some(result);
        self.items.push(item);
    }

    /// Toggle expanded tool results, returning the new state.
    pub fn toggle_tool_results(&mut self) -> bool {
        self.show_tool_results = !self.show_tool_results;
        self.show_tool_results
    }

    /// Toggle thinking display, returning the new state.
    pub fn toggle_thinking(&mut self) -> bool {
        self.show_thinking = !self.show_thinking;
        self.show_thinking
    }

    /// Replace visible queued-message state (tau `update_queue`).
    pub fn update_queue(&mut self, steering: Vec<String>, follow_up: Vec<String>) {
        self.queued_steering = steering;
        self.queued_follow_up = follow_up;
    }

    /// Total number of pending queued messages (tau `queued_message_count`).
    #[must_use]
    pub fn queued_message_count(&self) -> usize {
        self.queued_steering.len() + self.queued_follow_up.len()
    }

    /// Clear visible transcript state without touching durable history.
    pub fn clear(&mut self) {
        self.items.clear();
        self.assistant_buffer.clear();
        self.error = None;
    }

    /// Replace loaded skill metadata (tau `set_skills`).
    pub fn set_skills(&mut self, skills: impl IntoIterator<Item = Skill>) {
        self.skills = skills.into_iter().collect();
    }

    /// Populate the transcript from restored canonical messages (tau
    /// `load_messages`).
    pub fn load_messages<'a>(&mut self, messages: impl IntoIterator<Item = &'a AgentMessage>) {
        for message in messages {
            match message {
                AgentMessage::User(m) => self.add_user_message(&m.text(), None, None),
                AgentMessage::Custom(m) => {
                    let details = match &m.details {
                        Some(JsonValue::Object(_)) => m.details.clone(),
                        _ => None,
                    };
                    self.add_user_message(&m.text(), Some(&m.custom_type), details);
                }
                AgentMessage::Assistant(m) => {
                    let thinking = m.thinking_text();
                    if !thinking.is_empty() {
                        self.add_item(ChatItemRole::Thinking, thinking);
                    }
                    let text = m.text();
                    if !text.is_empty() {
                        self.add_item(ChatItemRole::Assistant, text);
                    }
                    for tool_call in m.tool_calls() {
                        self.add_tool_call(&tool_call);
                    }
                }
                AgentMessage::ToolResult(m) => {
                    self.record_tool_result(
                        &m.tool_call_id,
                        &m.tool_name,
                        AgentToolResult {
                            content: m.content.clone(),
                            details: m.details.clone(),
                            added_tool_names: None,
                            terminate: None,
                        },
                        m.is_error,
                    );
                }
                AgentMessage::BranchSummary(m) => {
                    self.add_item_full(
                        ChatItemRole::BranchSummary,
                        "Branch summary (Ctrl+O to expand)",
                        None,
                        Some(m.summary.clone()),
                        None,
                        None,
                    );
                }
                AgentMessage::CompactionSummary(m) => {
                    self.add_item_full(
                        ChatItemRole::CompactionSummary,
                        "Compaction summary (Ctrl+O to expand)",
                        None,
                        Some(m.summary.clone()),
                        None,
                        None,
                    );
                }
                AgentMessage::BashExecution(_) => {}
            }
        }
    }

    fn read_skill_name(&self, tool_call: &ToolCall) -> Option<String> {
        if tool_call.name != "read" {
            return None;
        }
        let path = string_argument(&tool_call.arguments, "path")?;
        let read_path = normalized_path(path);
        for skill in &self.skills {
            if normalized_path(&skill.path.to_string_lossy()) == read_path {
                return Some(skill.name.clone());
            }
        }
        None
    }
}

fn parse_branch_summary_message(content: &str) -> Option<&str> {
    let prefix = "The following is a summary of a branch that this conversation came back from:\n<summary>\n";
    let suffix = "\n</summary>";
    if content.starts_with(prefix)
        && content.ends_with(suffix)
        && content.len() >= prefix.len() + suffix.len()
    {
        return Some(&content[prefix.len()..content.len() - suffix.len()]);
    }
    None
}

fn parse_compaction_summary_message(content: &str) -> Option<&str> {
    content.strip_prefix("Previous conversation summary:\n")
}

/// Format an elapsed duration tersely: `23s`, `1m 23s`, `1h 2m` (tau
/// `format_elapsed`).
#[must_use]
pub fn format_elapsed(seconds: f64) -> String {
    // Elapsed wall-clock seconds; intentionally truncate the fractional part.
    #[allow(clippy::cast_possible_truncation)]
    let total = seconds as i64;
    if total < 60 {
        return format!("{total}s");
    }
    let (minutes, secs) = (total / 60, total % 60);
    if minutes < 60 {
        return format!("{minutes}m {secs}s");
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    format!("{hours}h {minutes}m")
}

/// Show the spinner frame in place of a static invocation marker (tau
/// `apply_tool_spinner`).
#[must_use]
pub fn apply_tool_spinner(text: &str, frame: &str) -> String {
    for marker in INVOCATION_MARKERS {
        if let Some(rest) = text.strip_prefix(marker) {
            return format!("{frame} {rest}");
        }
    }
    format!("{frame} {text}")
}

/// Format a collapsed tool call for live and restored blocks (tau
/// `format_tool_call_block`).
#[must_use]
pub fn format_tool_call_block(tool_call: &ToolCall) -> String {
    let invocation = format_tool_call_invocation(tool_call);
    if tool_call.name == "bash" {
        invocation
    } else {
        format!("→ {invocation}")
    }
}

/// Format a tool call as a terse human-readable invocation (tau
/// `format_tool_call_invocation`).
#[must_use]
pub fn format_tool_call_invocation(tool_call: &ToolCall) -> String {
    let arguments = &tool_call.arguments;
    match tool_call.name.as_str() {
        "read" => match string_argument(arguments, "path") {
            None => fallback_tool_call_invocation(tool_call),
            Some(path) => format!("read {path}{}", read_line_suffix(arguments)),
        },
        "edit" => match string_argument(arguments, "path") {
            None => fallback_tool_call_invocation(tool_call),
            Some(path) => format!("edit {path}"),
        },
        "write" => match string_argument(arguments, "path") {
            None => fallback_tool_call_invocation(tool_call),
            Some(path) => format!("write {path}"),
        },
        "bash" => match string_argument(arguments, "command") {
            None => fallback_tool_call_invocation(tool_call),
            Some(command) => {
                let suffix = match number_argument(arguments, "timeout") {
                    Some(timeout) => format!(" (timeout {}s)", format_g(timeout)),
                    None => String::new(),
                };
                format!("$ {command}{suffix}")
            }
        },
        _ => fallback_tool_call_invocation(tool_call),
    }
}

fn read_line_suffix(arguments: &JsonMap) -> String {
    let offset = int_argument(arguments, "offset");
    let limit = int_argument(arguments, "limit");
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start = offset.map_or(1, |o| o.max(1));
    match limit {
        None => format!(":{start}-"),
        Some(limit) => format!(":{start}-{}", start + limit.max(1) - 1),
    }
}

fn fallback_tool_call_invocation(tool_call: &ToolCall) -> String {
    if tool_call.arguments.is_empty() {
        return tool_call.name.clone();
    }
    let rendered = python_repr_map(&tool_call.arguments);
    let rendered = if pystr::char_len(&rendered) > FALLBACK_INVOCATION_ARGS_CHARS {
        format!(
            "{}…",
            pystr::char_prefix(&rendered, FALLBACK_INVOCATION_ARGS_CHARS).trim_end()
        )
    } else {
        rendered
    };
    format!("{} {rendered}", tool_call.name)
}

/// Format a terse tool result line for orphaned results (tau
/// `format_tool_result_summary`).
#[must_use]
pub fn format_tool_result_summary(name: &str, ok: bool) -> String {
    let status = if ok { "✓" } else { "✗" };
    format!("{status} {name}")
}

/// Format a tool result for live and restored blocks (tau
/// `format_tool_result_block`).
#[must_use]
pub fn format_tool_result_block(
    name: &str,
    ok: bool,
    content: &str,
    data: Option<&JsonMap>,
) -> String {
    let status = if ok { "✓" } else { "✗" };
    let mut lines = vec![format!("{status} {name}")];
    if !content.is_empty() {
        lines.push(preview_text(content, TOOL_RESULT_PREVIEW_LINES));
    }
    if let Some(patch) = result_patch(name, ok, data) {
        lines.push(String::new());
        lines.push("Patch:".to_string());
        lines.push(preview_text(patch, TOOL_PATCH_PREVIEW_LINES));
    }
    lines.join("\n")
}

/// Format an input-bar terminal command result for the TUI (tau
/// `format_terminal_command_result_block`).
#[must_use]
pub fn format_terminal_command_result_block(
    ok: bool,
    added_to_context: bool,
    output: &str,
) -> String {
    let status = if ok { "✓" } else { "✗" };
    let suffix = if added_to_context {
        " · added to context"
    } else {
        " · not added to context"
    };
    let mut lines = vec![format!("{status} bash{suffix}")];
    if !output.is_empty() {
        lines.push(preview_text(output, TERMINAL_COMMAND_OUTPUT_PREVIEW_LINES));
    }
    lines.join("\n")
}

fn result_patch<'a>(name: &str, ok: bool, data: Option<&'a JsonMap>) -> Option<&'a str> {
    if name != "edit" || !ok {
        return None;
    }
    let patch = data?.get("patch")?.as_str()?;
    if patch.trim().is_empty() {
        None
    } else {
        Some(patch)
    }
}

fn preview_text(text: &str, max_lines: usize) -> String {
    let lines = pystr::splitlines(text);
    if lines.is_empty() {
        return pystr::char_prefix(text, TOOL_RESULT_PREVIEW_CHARS).to_string();
    }

    let preview_lines = &lines[..lines.len().min(max_lines)];
    let mut preview = preview_lines.join("\n");
    let hidden_lines = lines.len().saturating_sub(preview_lines.len());

    let truncated_by_chars = pystr::char_len(&preview) > TOOL_RESULT_PREVIEW_CHARS;
    if truncated_by_chars {
        preview = pystr::char_prefix(&preview, TOOL_RESULT_PREVIEW_CHARS)
            .trim_end()
            .to_string();
    }

    if hidden_lines > 0 || truncated_by_chars {
        let mut details = Vec::new();
        if hidden_lines > 0 {
            let plural = if hidden_lines == 1 { "" } else { "s" };
            details.push(format!("{hidden_lines} more line{plural}"));
        }
        if truncated_by_chars {
            details.push("additional text".to_string());
        }
        preview = format!(
            "{preview}\n\n[Preview only: {} hidden from the TUI.]",
            details.join(", ")
        );
    }
    preview
}

fn string_argument<'a>(arguments: &'a JsonMap, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(JsonValue::as_str)
}

fn int_argument(arguments: &JsonMap, key: &str) -> Option<i64> {
    match arguments.get(key) {
        Some(JsonValue::Bool(_)) | None => None,
        Some(value) => value.as_i64(),
    }
}

fn number_argument(arguments: &JsonMap, key: &str) -> Option<f64> {
    match arguments.get(key) {
        Some(JsonValue::Bool(_)) | None => None,
        Some(value) => value.as_f64(),
    }
}

/// Python `%g`-style float formatting (trims trailing zeros).
fn format_g(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e16 {
        return format!("{value:.0}");
    }
    let mut text = format!("{value}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

/// Render a JSON object like Python's `str(dict)` for the fallback invocation.
///
/// tau formats `str(tool_call.arguments)` — a Python `dict` repr with single
/// quotes. Reproduced here so the truncated fallback line reads identically.
fn python_repr_map(map: &JsonMap) -> String {
    let mut parts = Vec::with_capacity(map.len());
    for (key, value) in map {
        parts.push(format!(
            "{}: {}",
            python_repr_str(key),
            python_repr_value(value)
        ));
    }
    format!("{{{}}}", parts.join(", "))
}

fn python_repr_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "None".to_string(),
        JsonValue::Bool(b) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => python_repr_str(s),
        JsonValue::Array(items) => {
            let inner: Vec<String> = items.iter().map(python_repr_value).collect();
            format!("[{}]", inner.join(", "))
        }
        JsonValue::Object(map) => python_repr_map(map),
    }
}

fn python_repr_str(s: &str) -> String {
    // Python prefers single quotes unless the string contains a single quote and
    // no double quote.
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let (quote, escape_quote) = if has_single && !has_double {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == escape_quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Expand `~` and lexically normalize a path (tau's
/// `Path(path).expanduser().resolve(strict=False)`, minus symlink resolution).
fn normalized_path(path: &str) -> PathBuf {
    let expanded = expanduser(path);
    let base = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(expanded)
    };
    let mut normalized = PathBuf::new();
    for component in base.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn expanduser(path: &str) -> PathBuf {
    if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
