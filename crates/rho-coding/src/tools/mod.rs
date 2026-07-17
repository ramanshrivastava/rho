//! Built-in filesystem and shell tools for rho coding sessions (port of tau's
//! `tau_coding/tools.py`).
//!
//! Four tools — `read`, `write`, `edit`, `bash` — created by
//! [`create_coding_tools`], in that order. Each is a [`ToolDefinition`] carrying
//! Pi-style prompt metadata and a hand-written JSON input schema (byte-matched
//! to tau), convertible to a portable [`AgentTool`] via
//! [`ToolDefinition::to_agent_tool`].
//!
//! Parity notes:
//! - Output is truncated to [`truncation::DEFAULT_MAX_OUTPUT_LINES`] /
//!   [`truncation::DEFAULT_MAX_OUTPUT_BYTES`] (head for `read`, tail for `bash`).
//! - `read` returns supported images (jpeg/png/gif/webp) as base64 `details`.
//! - Same-path writes/edits serialize through a per-path lock ([`DashMap`] of
//!   `tokio::Mutex`), matching tau's process-local `_file_locks`.
//! - `bash` runs a shell subprocess; on timeout/cancel the whole process group
//!   is killed (unix `killpg`), so pipeline/compound children don't survive.

mod difflib;
mod edit_match;
pub mod truncation;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use dashmap::DashMap;
use futures::future::BoxFuture;
use tokio::sync::Mutex as AsyncMutex;

use rho_agent::messages::{TextContent, ToolResultContent};
use rho_agent::provider::CancellationToken;
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolUpdateCallback};
use rho_agent::types::{JsonMap, JsonValue};

use edit_match::Edit;
use truncation::{
    DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_OUTPUT_LINES, format_size, truncate_head, truncate_tail,
};

/// Supported image MIME types returned as base64 by `read` (tau
/// `SUPPORTED_IMAGE_MIME_TYPES`).
const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// tau's `_file_locks`: a process-local per-resolved-path async lock so
/// concurrent write/edit of the same file cannot interleave.
static FILE_LOCKS: LazyLock<DashMap<PathBuf, Arc<AsyncMutex<()>>>> = LazyLock::new(DashMap::new);

fn file_lock(path: &Path) -> Arc<AsyncMutex<()>> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    FILE_LOCKS.entry(key).or_default().clone()
}

/// The `(arguments, signal)` executor a [`ToolDefinition`] wraps (tau's
/// `ToolDefinition.executor`).
type DefinitionExecutor = Arc<
    dyn Fn(
            JsonMap,
            Option<Arc<dyn CancellationToken>>,
        ) -> BoxFuture<'static, Result<AgentToolResult, ToolError>>
        + Send
        + Sync,
>;

/// A coding tool with prompt metadata, JSON schema, and executor (tau
/// `ToolDefinition`).
#[derive(Clone)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// User-facing description.
    pub description: String,
    /// Short prompt snippet listed under "Available tools".
    pub prompt_snippet: String,
    /// Prompt guidelines contributed by this tool.
    pub prompt_guidelines: Vec<String>,
    /// Hand-written JSON input schema.
    pub input_schema: JsonMap,
    executor: DefinitionExecutor,
}

impl ToolDefinition {
    /// Convert to the portable [`AgentTool`] the loop executes (tau
    /// `to_agent_tool`): the wrapper drops `tool_call_id`/`on_update`.
    #[must_use]
    pub fn to_agent_tool(&self) -> AgentTool {
        let executor = self.executor.clone();
        let mut tool = AgentTool::new(
            self.name.clone(),
            self.name.clone(),
            self.description.clone(),
            self.input_schema.clone(),
            Arc::new(
                move |_tool_call_id, arguments, signal, _on_update: ToolUpdateCallback| {
                    executor(arguments, signal)
                },
            ),
        );
        tool.prompt_snippet = Some(self.prompt_snippet.clone());
        tool.prompt_guidelines.clone_from(&self.prompt_guidelines);
        tool
    }
}

/// Create the default coding-tool set for a project (tau `create_coding_tools`):
/// `read`, `write`, `edit`, `bash`, resolving relative paths against `cwd`.
#[must_use]
pub fn create_coding_tools(cwd: &Path, shell_command_prefix: Option<&str>) -> Vec<AgentTool> {
    vec![
        create_read_tool_definition(cwd).to_agent_tool(),
        create_write_tool_definition(cwd).to_agent_tool(),
        create_edit_tool_definition(cwd).to_agent_tool(),
        create_bash_tool_definition(cwd, shell_command_prefix).to_agent_tool(),
    ]
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// Create the `read` tool definition (tau `create_read_tool_definition`).
#[must_use]
pub fn create_read_tool_definition(cwd: &Path) -> ToolDefinition {
    let root = cwd.to_path_buf();
    let executor: DefinitionExecutor = Arc::new(move |arguments, _signal| {
        let root = root.clone();
        Box::pin(async move { read_execute(&root, &arguments) })
    });

    ToolDefinition {
        name: "read".into(),
        description: format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp). \
Images are returned as base64 metadata. For text files, output is truncated to \
{DEFAULT_MAX_OUTPUT_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large \
files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_OUTPUT_BYTES / 1024
        ),
        prompt_snippet: "Read file contents".into(),
        prompt_guidelines: vec!["Use read to examine files instead of cat or sed.".into()],
        input_schema: schema(serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to read"},
                "offset": {"type": "integer", "description": "Line number to start reading from"},
                "limit": {"type": "integer", "description": "Maximum number of lines to read"},
            },
            "required": ["path"],
        })),
        executor,
    }
}

fn read_execute(root: &Path, arguments: &JsonMap) -> Result<AgentToolResult, ToolError> {
    let raw_path = str_arg(arguments, "path")?;
    let path = path_arg(arguments, "path", root)?;
    let offset = optional_int_arg(arguments, "offset")?;
    let limit = optional_int_arg(arguments, "limit")?;

    if let Some(o) = offset {
        if o < 0 {
            return Err(ToolError("offset must be at least 0".into()));
        }
    }
    if let Some(l) = limit {
        if l < 1 {
            return Err(ToolError("limit must be at least 1".into()));
        }
    }
    if !path.exists() {
        return Err(ToolError(format!("File not found: {}", path.display())));
    }
    if path.is_dir() {
        return Err(ToolError(format!(
            "Path is a directory: {}",
            path.display()
        )));
    }

    if let Some(mime_type) = detect_supported_image_mime_type(&path) {
        let data = std::fs::read(&path).map_err(|e| ToolError(e.to_string()))?;
        let details = serde_json::json!({
            "path": path.display().to_string(),
            "mime_type": mime_type,
            "bytes": data.len(),
            "image_base64": base64_encode(&data),
        });
        return Ok(ok_result(format!("Read image file [{mime_type}]"), details));
    }

    let text = std::fs::read_to_string(&path).map_err(|e| ToolError(e.to_string()))?;
    let all_lines: Vec<&str> = text.split('\n').collect();
    let start_line = match offset {
        None | Some(0) => 0usize,
        Some(o) => usize::try_from(o - 1).unwrap_or(0),
    };
    if start_line >= all_lines.len() {
        let offset_display = offset.unwrap_or(0);
        return Err(ToolError(format!(
            "Offset {offset_display} is beyond end of file ({} lines total)",
            all_lines.len()
        )));
    }

    let mut user_limited_lines: Option<usize> = None;
    let selected = if let Some(l) = limit {
        let l = usize::try_from(l).unwrap_or(0);
        let end_line = (start_line + l).min(all_lines.len());
        user_limited_lines = Some(end_line - start_line);
        all_lines[start_line..end_line].join("\n")
    } else {
        all_lines[start_line..].join("\n")
    };

    let truncation = truncate_head(
        &selected,
        DEFAULT_MAX_OUTPUT_LINES,
        DEFAULT_MAX_OUTPUT_BYTES,
    );
    let start_display = start_line + 1;
    let details = serde_json::json!({
        "path": path.display().to_string(),
        "truncation": truncation.to_json(),
    });

    let output = if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        format!(
            "[Line {start_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n \
'{start_display}p' {raw_path} | head -c {DEFAULT_MAX_OUTPUT_BYTES}]",
            format_size(DEFAULT_MAX_OUTPUT_BYTES)
        )
    } else if truncation.truncated {
        let end_display = start_display + truncation.output_lines - 1;
        let next_offset = end_display + 1;
        let total = all_lines.len();
        if truncation.truncated_by.as_deref() == Some("lines") {
            format!(
                "{}\n\n[Showing lines {start_display}-{end_display} of {total}. Use offset={next_offset} to continue.]",
                truncation.content
            )
        } else {
            format!(
                "{}\n\n[Showing lines {start_display}-{end_display} of {total} ({} limit). Use offset={next_offset} to continue.]",
                truncation.content,
                format_size(DEFAULT_MAX_OUTPUT_BYTES)
            )
        }
    } else if let Some(ul) = user_limited_lines.filter(|ul| start_line + ul < all_lines.len()) {
        let remaining = all_lines.len() - (start_line + ul);
        let next_offset = start_line + ul + 1;
        format!(
            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
            truncation.content
        )
    } else {
        truncation.content.clone()
    };

    Ok(ok_result(output, details))
}

/// Convert an `AgentTool` for reading text/images (tau `create_read_tool`).
#[must_use]
pub fn create_read_tool(cwd: &Path) -> AgentTool {
    create_read_tool_definition(cwd).to_agent_tool()
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

/// Create the `write` tool definition (tau `create_write_tool_definition`).
#[must_use]
pub fn create_write_tool_definition(cwd: &Path) -> ToolDefinition {
    let root = cwd.to_path_buf();
    let executor: DefinitionExecutor = Arc::new(move |arguments, _signal| {
        let root = root.clone();
        Box::pin(async move { write_execute(&root, &arguments).await })
    });

    ToolDefinition {
        name: "write".into(),
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites \
if it does. Automatically creates parent directories."
            .into(),
        prompt_snippet: "Create or overwrite files".into(),
        prompt_guidelines: vec!["Use write only for new files or complete rewrites.".into()],
        input_schema: schema(serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to write"},
                "content": {"type": "string", "description": "Content to write to the file"},
            },
            "required": ["path", "content"],
        })),
        executor,
    }
}

async fn write_execute(root: &Path, arguments: &JsonMap) -> Result<AgentToolResult, ToolError> {
    let path = path_arg(arguments, "path", root)?;
    let content = str_arg(arguments, "content")?;

    let lock = file_lock(&path);
    let _guard = lock.lock().await;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ToolError(e.to_string()))?;
    }
    std::fs::write(&path, content.as_bytes()).map_err(|e| ToolError(e.to_string()))?;

    let details = serde_json::json!({
        "path": path.display().to_string(),
        "characters": content.chars().count(),
    });
    Ok(ok_result(
        format!("Successfully wrote to {}.", path.display()),
        details,
    ))
}

/// Convert an `AgentTool` for writing text files (tau `create_write_tool`).
#[must_use]
pub fn create_write_tool(cwd: &Path) -> AgentTool {
    create_write_tool_definition(cwd).to_agent_tool()
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

/// Create the `edit` tool definition (tau `create_edit_tool_definition`).
#[must_use]
pub fn create_edit_tool_definition(cwd: &Path) -> ToolDefinition {
    let root = cwd.to_path_buf();
    let executor: DefinitionExecutor = Arc::new(move |arguments, _signal| {
        let root = root.clone();
        Box::pin(async move { edit_execute(&root, &arguments).await })
    });

    ToolDefinition {
        name: "edit".into(),
        description: "Edit a single file using exact text replacement. Every edits[].oldText \
must match a unique, non-overlapping region of the original file. If two changes affect the \
same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do \
not include large unchanged regions just to connect distant changes."
            .into(),
        prompt_snippet: "Make precise file edits with exact text replacement, including multiple \
disjoint edits in one call"
            .into(),
        prompt_guidelines: vec![
            "Use edit for precise changes (edits[].oldText must match exactly)".into(),
            "When changing multiple separate locations in one file, use one edit call with \
multiple entries in edits[] instead of multiple edit calls"
                .into(),
            "Each edits[].oldText is matched against the original file, not after earlier edits \
are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit."
                .into(),
            "Keep edits[].oldText as small as possible while still being unique in the file. Do \
not pad with large unchanged regions."
                .into(),
        ],
        input_schema: schema(serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to edit"},
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {"type": "string"},
                            "newText": {"type": "string"},
                        },
                        "required": ["oldText", "newText"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["path", "edits"],
            "additionalProperties": false,
        })),
        executor,
    }
}

async fn edit_execute(root: &Path, arguments: &JsonMap) -> Result<AgentToolResult, ToolError> {
    let prepared = prepare_edit_arguments(arguments);
    let path = path_arg(&prepared, "path", root)?;
    let edits = edits_arg(&prepared)?;

    if !path.exists() {
        return Err(ToolError(format!(
            "Could not edit file: {}. File not found.",
            path.display()
        )));
    }
    if path.is_dir() {
        return Err(ToolError(format!(
            "Could not edit file: {}. Path is a directory.",
            path.display()
        )));
    }

    let path_str = path.display().to_string();
    let lock = file_lock(&path);
    let _guard = lock.lock().await;

    let raw_content = std::fs::read_to_string(&path).map_err(|e| ToolError(e.to_string()))?;
    let (bom, content) = edit_match::strip_bom(&raw_content);
    let original_ending = edit_match::detect_line_ending(&content);
    let normalized = edit_match::normalize_to_lf(&content);
    let (base_content, new_content) =
        edit_match::apply_edits_to_normalized_content(&normalized, &edits, &path_str)
            .map_err(|e| ToolError(e.0))?;
    let final_content = format!(
        "{bom}{}",
        edit_match::restore_line_endings(&new_content, original_ending)
    );
    std::fs::write(&path, final_content.as_bytes()).map_err(|e| ToolError(e.to_string()))?;

    let (diff_text, first_changed_line) =
        edit_match::generate_diff_string(&base_content, &new_content);
    let unified_patch = edit_match::generate_unified_patch(&path_str, &base_content, &new_content);
    let details = serde_json::json!({
        "path": path_str,
        "edits": edits.len(),
        "diff": diff_text,
        "patch": unified_patch,
        "first_changed_line": first_changed_line,
    });
    Ok(ok_result(
        format!(
            "Successfully replaced {} block(s) in {}.",
            edits.len(),
            path.display()
        ),
        details,
    ))
}

/// Convert an `AgentTool` for exact-match edits (tau `create_edit_tool`).
#[must_use]
pub fn create_edit_tool(cwd: &Path) -> AgentTool {
    create_edit_tool_definition(cwd).to_agent_tool()
}

fn prepare_edit_arguments(arguments: &JsonMap) -> JsonMap {
    let mut prepared = arguments.clone();

    // A JSON-string `edits` value is parsed into a list.
    if let Some(JsonValue::String(s)) = prepared.get("edits") {
        if let Ok(JsonValue::Array(parsed)) = serde_json::from_str::<JsonValue>(s) {
            prepared.insert("edits".into(), JsonValue::Array(parsed));
        }
    }

    // Legacy top-level oldText/newText fold into the edits list.
    let old_text = prepared.get("oldText").cloned();
    let new_text = prepared.get("newText").cloned();
    if let (Some(JsonValue::String(old)), Some(JsonValue::String(new))) = (old_text, new_text) {
        let mut edit_list = match prepared.get("edits") {
            Some(JsonValue::Array(list)) => list.clone(),
            _ => Vec::new(),
        };
        edit_list.push(serde_json::json!({"oldText": old, "newText": new}));
        prepared.insert("edits".into(), JsonValue::Array(edit_list));
        prepared.remove("oldText");
        prepared.remove("newText");
    }
    prepared
}

fn edits_arg(arguments: &JsonMap) -> Result<Vec<Edit>, ToolError> {
    let value = match arguments.get("edits") {
        Some(JsonValue::Array(list)) if !list.is_empty() => list,
        _ => {
            return Err(ToolError(
                "Edit tool input is invalid. edits must contain at least one replacement.".into(),
            ));
        }
    };

    let mut edits = Vec::new();
    for (index, item) in value.iter().enumerate() {
        let JsonValue::Object(obj) = item else {
            return Err(ToolError(format!("edits[{index}] must be an object")));
        };
        let old_text = obj.get("oldText");
        let new_text = obj.get("newText");
        match (old_text, new_text) {
            (Some(JsonValue::String(old)), Some(JsonValue::String(new))) => edits.push(Edit {
                old_text: old.clone(),
                new_text: new.clone(),
            }),
            _ => {
                return Err(ToolError(format!(
                    "edits[{index}].oldText and edits[{index}].newText must be strings"
                )));
            }
        }
    }
    Ok(edits)
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/// Create the `bash` tool definition (tau `create_bash_tool_definition`).
#[must_use]
pub fn create_bash_tool_definition(
    cwd: &Path,
    shell_command_prefix: Option<&str>,
) -> ToolDefinition {
    let root = cwd.to_path_buf();
    let prefix = shell_command_prefix
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let executor_prefix = prefix.clone();
    let executor: DefinitionExecutor = Arc::new(move |arguments, signal| {
        let root = root.clone();
        let prefix = executor_prefix.clone();
        Box::pin(async move { bash_execute(&root, prefix.as_deref(), &arguments, signal).await })
    });

    ToolDefinition {
        name: "bash".into(),
        description: format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. \
Output is truncated to last {DEFAULT_MAX_OUTPUT_LINES} lines or {}KB (whichever is hit first). \
If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_OUTPUT_BYTES / 1024
        ),
        prompt_snippet: "Execute bash commands (ls, grep, find, etc.)".into(),
        prompt_guidelines: vec![],
        input_schema: schema(serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Bash command to execute"},
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds (optional, no default timeout)",
                },
            },
            "required": ["command"],
        })),
        executor,
    }
}

async fn bash_execute(
    root: &Path,
    prefix: Option<&str>,
    arguments: &JsonMap,
    signal: Option<Arc<dyn CancellationToken>>,
) -> Result<AgentToolResult, ToolError> {
    let command = str_arg(arguments, "command")?;
    let shell_command = prefixed_shell_command(&command, prefix);
    let timeout = optional_float_arg(arguments, "timeout")?;
    if let Some(t) = timeout {
        if t <= 0.0 {
            return Err(ToolError("timeout must be greater than 0".into()));
        }
    }
    if signal.as_ref().is_some_and(|s| s.is_cancelled()) {
        return Err(ToolError("Command cancelled".into()));
    }

    let start = Instant::now();
    let bash_exec = subprocess::run_shell(
        &shell_command,
        root,
        timeout,
        signal.as_deref(),
        prefix.is_some(),
    )
    .await
    .map_err(ToolError)?;

    let output = String::from_utf8_lossy(&bash_exec.output_bytes).into_owned();
    let truncation = truncate_tail(&output, DEFAULT_MAX_OUTPUT_LINES, DEFAULT_MAX_OUTPUT_BYTES);
    let mut full_output_path: Option<String> = None;
    let mut output_text = if truncation.content.is_empty() {
        "(no output)".to_string()
    } else {
        truncation.content.clone()
    };

    if truncation.truncated {
        let path = write_temp_output(&output).map_err(ToolError)?;
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        if truncation.last_line_partial {
            let _ = write!(
                output_text,
                "\n\n[Showing last {} of line {end_line}. Full output: {path}]",
                format_size(truncation.output_bytes)
            );
        } else if truncation.truncated_by.as_deref() == Some("lines") {
            let _ = write!(
                output_text,
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
                truncation.total_lines
            );
        } else {
            let _ = write!(
                output_text,
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_OUTPUT_BYTES)
            );
        }
        full_output_path = Some(path);
    }

    let exit_code = bash_exec.exit_code;
    let status: Option<String> = if bash_exec.timed_out {
        Some(match timeout {
            Some(t) => format!(
                "Command timed out after {} seconds",
                crate::fmt_util::format_g(t)
            ),
            None => "Command timed out".to_string(),
        })
    } else if bash_exec.cancelled {
        Some("Command cancelled".to_string())
    } else if !matches!(exit_code, Some(0) | None) {
        Some(format!(
            "Command exited with code {}",
            exit_code.expect("checked non-None")
        ))
    } else {
        None
    };
    if let Some(status) = status {
        output_text = append_status_block(&output_text, &status);
    }

    #[allow(clippy::cast_precision_loss)]
    let duration = round3(start.elapsed().as_secs_f64());
    let details = serde_json::json!({
        "command": command,
        "exit_code": exit_code,
        "timed_out": bash_exec.timed_out,
        "cancelled": bash_exec.cancelled,
        "duration_seconds": duration,
        "truncation": truncation.to_json(),
        "full_output_path": full_output_path,
        "shell_command_prefix_applied": prefix.is_some(),
    });
    Ok(ok_result(output_text, details))
}

/// Convert an `AgentTool` for shell execution (tau `create_bash_tool`).
#[must_use]
pub fn create_bash_tool(cwd: &Path, shell_command_prefix: Option<&str>) -> AgentTool {
    create_bash_tool_definition(cwd, shell_command_prefix).to_agent_tool()
}

fn prefixed_shell_command(command: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}\n{command}"),
        None => command.to_string(),
    }
}

/// Append a status line after a blank line when output exists (tau
/// `append_status_block`).
fn append_status_block(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

fn write_temp_output(output: &str) -> Result<String, String> {
    use std::io::Write;
    let file = tempfile::Builder::new()
        .prefix("tau-bash-")
        .suffix(".log")
        .tempfile()
        .map_err(|e| e.to_string())?;
    let (mut handle, path) = file.keep().map_err(|e| e.to_string())?;
    handle
        .write_all(output.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

// ---------------------------------------------------------------------------
// argument + encoding helpers
// ---------------------------------------------------------------------------

fn schema(value: JsonValue) -> JsonMap {
    match value {
        JsonValue::Object(map) => map,
        _ => JsonMap::new(),
    }
}

fn ok_result(text: String, details: JsonValue) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContent::Text(TextContent::new(text))],
        details: Some(details),
        added_tool_names: None,
        terminate: None,
    }
}

fn str_arg(arguments: &JsonMap, name: &str) -> Result<String, ToolError> {
    match arguments.get(name) {
        Some(JsonValue::String(s)) => Ok(s.clone()),
        _ => Err(ToolError(format!("{name} must be a string"))),
    }
}

fn path_arg(arguments: &JsonMap, name: &str, cwd: &Path) -> Result<PathBuf, ToolError> {
    let value = str_arg(arguments, name)?;
    let expanded = expanduser(&value);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(cwd.join(path))
    }
}

fn optional_int_arg(arguments: &JsonMap, name: &str) -> Result<Option<i64>, ToolError> {
    match arguments.get(name) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Number(n)) if n.is_i64() || n.is_u64() => {
            Ok(Some(n.as_i64().ok_or_else(|| {
                ToolError(format!("{name} must be an integer"))
            })?))
        }
        _ => Err(ToolError(format!("{name} must be an integer"))),
    }
}

fn optional_float_arg(arguments: &JsonMap, name: &str) -> Result<Option<f64>, ToolError> {
    match arguments.get(name) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Number(n)) => {
            Ok(Some(n.as_f64().ok_or_else(|| {
                ToolError(format!("{name} must be a number"))
            })?))
        }
        _ => Err(ToolError(format!("{name} must be a number"))),
    }
}

fn expanduser(value: &str) -> String {
    if value == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    } else if let Some(rest) = value.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    value.to_string()
}

fn detect_supported_image_mime_type(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    SUPPORTED_IMAGE_MIME_TYPES.contains(&mime).then_some(mime)
}

/// Standard base64 with padding (Python `base64.b64encode`).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

mod subprocess;

#[cfg(test)]
mod tests;
