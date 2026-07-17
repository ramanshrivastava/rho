//! Session export helpers for human-readable transcript views (port of tau's
//! `tau_coding/session_export.py`).
//!
//! Two exporters render a session transcript (a `SessionEntry` sequence):
//!
//! * [`render_session_html`] / [`export_session_html`] — a self-contained HTML
//!   document with a branch-tree rail and an entry stream. The output is
//!   **byte-for-byte** identical to tau's, including the inline CSS/JS, the
//!   Pygments-highlighted JSON blocks, and `html.escape` semantics. The golden
//!   `fixtures/export/kitchen-sink.html` is the oracle.
//! * [`export_session_jsonl`] — one `model_dump_json()` line per entry. Unlike
//!   session *storage* (which uses `exclude_none`), this path **writes nulls**
//!   (tau's default `model_dump_json`, `by_alias=True`, no `exclude_none`), so it
//!   cannot reuse rho's `skip_serializing_if` wire form. [`densify_entry`]
//!   re-inserts the omitted `null` fields in pydantic field order.
//!
//! ## Determinism
//!
//! [`render_session_html`] stamps a "Generated:" instant. tau uses
//! `datetime.now(UTC)`; the golden was extracted with that clock frozen. rho
//! mirrors this: the public entry point uses the system clock, and the internal
//! [`render_html`] takes an explicit Unix-seconds instant so the golden test can
//! reproduce the frozen `2024-01-01T00:00:00+00:00`.
//!
//! ## Pygments port
//!
//! tau highlights the JSON payloads with `pygments` (`JsonLexer` +
//! `HtmlFormatter(nowrap=True)`). To avoid a dependency and match byte output we
//! port both faithfully: [`json_tokens`] reproduces `JsonLexer`'s key-detection
//! state machine and [`format_highlight`] reproduces `HtmlFormatter._format_lines`
//! (including its `&#39;` escape table, whitespace-token line splitting, and
//! same-class span coalescing). The sorted, `ensure_ascii` JSON source
//! ([`json_dump`]) matches `json.dumps(value, indent=2, sort_keys=True)`.

// `cast_possible_truncation`: `_format_timestamp` floors a Unix-seconds `f64` to
// `i64` (tau does the same). `if_not_else`: the Pygments `_format_lines` port
// mirrors tau's `if lspan != cspan` branches verbatim; flipping them would
// obscure the parity. `needless_raw_string_hashes`: the template/icon consts are
// machine-extracted with a uniform `r####"…"####` delimiter for robustness.
#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::if_not_else,
    clippy::needless_raw_string_hashes
)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize as _;
use serde_json::Serializer;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter};
use serde_json::{Map, Value};

use rho_agent::messages::AgentMessage;
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::tree::path_to_entry;

/// Default export title (tau `render_session_html` default).
pub const DEFAULT_EXPORT_TITLE: &str = "Tau Session Export";

/// Raised when a session cannot be exported (tau `SessionExportError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SessionExportError(String);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return the default HTML export path for a JSONL session file (tau
/// `default_session_export_path`): the session path with an `.html` suffix.
#[must_use]
pub fn default_session_export_path(session_path: &Path) -> PathBuf {
    session_path.with_extension("html")
}

/// Return the default user-facing export artifact path (tau
/// `default_session_export_artifact_path`): `<stem><suffix>` in `destination_dir`.
#[must_use]
pub fn default_session_export_artifact_path(
    session_path: &Path,
    destination_dir: &Path,
    format: &str,
) -> PathBuf {
    let stem = session_path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let suffix = export_suffix(format);
    destination_dir.join(format!("{stem}{suffix}"))
}

/// Normalize a session export format name (tau `normalize_export_format`).
///
/// # Errors
/// Returns [`SessionExportError`] for an unsupported format.
pub fn normalize_export_format(value: Option<&str>) -> Result<String, SessionExportError> {
    let normalized = value
        .unwrap_or("html")
        .trim()
        .to_lowercase()
        .trim_start_matches('.')
        .to_string();
    match normalized.as_str() {
        "htm" | "html" => Ok("html".to_string()),
        "jsonl" => Ok("jsonl".to_string()),
        _ => Err(SessionExportError(format!(
            "Unsupported export format: {}",
            value.unwrap_or("")
        ))),
    }
}

fn export_suffix(format: &str) -> &'static str {
    match normalize_export_format(Some(format)) {
        Ok(f) if f == "jsonl" => ".jsonl",
        _ => ".html",
    }
}

/// Write session entries to a JSONL export and return its path (tau
/// `export_session_jsonl`).
///
/// **Writes nulls** — this is tau's default `model_dump_json()`, *not* the
/// `exclude_none` storage form. See [`densify_entry`].
///
/// # Errors
/// Propagates filesystem errors from creating the parent directory or writing.
pub fn export_session_jsonl(entries: &[SessionEntry], output_path: &Path) -> io::Result<PathBuf> {
    ensure_parent(output_path)?;
    let lines: Vec<String> = entries.iter().map(dump_entry_line).collect();
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(output_path, body)?;
    Ok(output_path.to_path_buf())
}

/// Write a self-contained HTML session export and return its path (tau
/// `export_session_html`).
///
/// # Errors
/// Propagates filesystem errors from creating the parent directory or writing.
pub fn export_session_html(
    entries: &[SessionEntry],
    output_path: &Path,
    title: &str,
    source: Option<&str>,
) -> io::Result<PathBuf> {
    ensure_parent(output_path)?;
    fs::write(output_path, render_session_html(entries, title, source))?;
    Ok(output_path.to_path_buf())
}

/// Write a session export in the requested or inferred format (tau
/// `export_session_artifact`).
///
/// # Errors
/// Propagates the format error and any filesystem error.
pub fn export_session_artifact(
    entries: &[SessionEntry],
    output_path: &Path,
    title: &str,
    source: Option<&str>,
    format: Option<&str>,
) -> io::Result<PathBuf> {
    let inferred = format.map_or_else(
        || {
            output_path
                .extension()
                .map_or_else(String::new, |e| e.to_string_lossy().into_owned())
        },
        str::to_string,
    );
    let export_format = normalize_export_format(Some(&inferred))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    if export_format == "jsonl" {
        export_session_jsonl(entries, output_path)
    } else {
        export_session_html(entries, output_path, title, source)
    }
}

/// Render a session transcript/tree as standalone HTML (tau
/// `render_session_html`). The "Generated:" instant is the current system time.
#[must_use]
pub fn render_session_html(entries: &[SessionEntry], title: &str, source: Option<&str>) -> String {
    render_html(entries, title, source, now_unix_secs())
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn dump_entry_line(entry: &SessionEntry) -> String {
    let value = serde_json::to_value(entry).unwrap_or(Value::Null);
    to_compact_json(&densify_entry(&value))
}

/// A `serde_json` formatter that renders floats via Python's `float.__repr__`
/// (`5e-07`, not serde's `5e-7`), matching tau's `json.dumps` output. Only
/// `write_f64` diverges; the wrapped base handles structure (so `PrettyFormatter`
/// keeps its indentation). This is why the export re-serializes through a
/// `serde_json::Value` rather than the wire codec — the wire path writes
/// `exclude_none`, the export writes nulls, and both must keep Python float
/// shapes on small cost values.
struct PyFloat<F>(F);

impl<F: Formatter> Formatter for PyFloat<F> {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(crate::pystr::python_float_repr(value).as_bytes())
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }
}

/// Compact JSON with Python float shapes (tau `json.dumps` separators).
fn to_compact_json(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = Serializer::with_formatter(&mut buf, PyFloat(CompactFormatter));
    if value.serialize(&mut ser).is_err() {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
}

/// Pretty (2-space) JSON with Python float shapes.
fn to_pretty_json(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser =
        Serializer::with_formatter(&mut buf, PyFloat(PrettyFormatter::with_indent(b"  ")));
    if value.serialize(&mut ser).is_err() {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

fn render_html(
    entries: &[SessionEntry],
    title: &str,
    source: Option<&str>,
    generated_secs: i64,
) -> String {
    let active_leaf = active_leaf_id(entries);
    let active_path = active_path_ids(entries, active_leaf.as_deref());
    let visible: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| !matches!(e, SessionEntry::Leaf(_)))
        .collect();
    let tree_html = render_tree(&visible, &active_path, active_leaf.as_deref());
    let details_html = render_entry_details(&visible, &active_path, active_leaf.as_deref());
    let source_html = source.map_or_else(String::new, |s| {
        format!("<p class=\"source\">Source: <code>{}</code></p>", escape(s))
    });
    let generated_at = iso_utc(generated_secs);

    let title_e = escape(title);
    let mut out = String::new();
    out.push_str(HTML_1);
    out.push_str(&title_e);
    out.push_str(HTML_2);
    out.push_str(&title_e);
    out.push_str(HTML_3);
    out.push_str(&source_html);
    out.push_str(HTML_4);
    out.push_str(&attr(&generated_at));
    out.push_str(HTML_5);
    out.push_str(&escape(&generated_at));
    out.push_str(HTML_6);
    out.push_str(&tree_html);
    out.push_str(HTML_7);
    out.push_str(&details_html);
    out.push_str(HTML_8);
    out
}

fn active_leaf_id(entries: &[SessionEntry]) -> Option<String> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Leaf(leaf) = entry {
            return leaf.entry_id.clone();
        }
    }
    entries.last().map(|e| e.id().to_string())
}

fn active_path_ids(entries: &[SessionEntry], active_leaf_id: Option<&str>) -> HashSet<String> {
    let Some(leaf) = active_leaf_id else {
        return HashSet::new();
    };
    let Ok(path) = path_to_entry(entries, leaf) else {
        let mut set = HashSet::new();
        set.insert(leaf.to_string());
        return set;
    };
    path.iter().map(|e| e.id().to_string()).collect()
}

fn render_tree(
    entries: &[&SessionEntry],
    active_path: &HashSet<String>,
    active_leaf: Option<&str>,
) -> String {
    if entries.is_empty() {
        return "<p class=\"empty\">No entries.</p>".to_string();
    }

    let entry_ids: HashSet<&str> = entries.iter().map(|e| e.id()).collect();
    let mut children_by_parent: HashMap<Option<&str>, Vec<&SessionEntry>> = HashMap::new();
    for entry in entries {
        children_by_parent
            .entry(entry.parent_id())
            .or_default()
            .push(*entry);
    }

    let mut roots: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.parent_id().is_none_or(|p| !entry_ids.contains(p)))
        .copied()
        .collect();
    if roots.is_empty() {
        roots = entries.to_vec();
    }

    let mut rendered_ids: HashSet<&str> = HashSet::new();
    let empty_ancestors: HashSet<&str> = HashSet::new();
    let mut nodes = String::new();
    for root in &roots {
        if !rendered_ids.contains(root.id()) {
            nodes.push_str(&render_tree_chain(
                root,
                &children_by_parent,
                active_path,
                active_leaf,
                &empty_ancestors,
                &mut rendered_ids,
            ));
        }
    }

    let mut dangling = String::new();
    for entry in entries {
        if !rendered_ids.contains(entry.id()) {
            dangling.push_str(&render_tree_chain(
                entry,
                &children_by_parent,
                active_path,
                active_leaf,
                &empty_ancestors,
                &mut rendered_ids,
            ));
        }
    }
    if !dangling.is_empty() {
        nodes.push_str(
            "<li><span class=\"node-link\"><span class=\"node-type\">Unreachable entries</span></span>",
        );
        let _ = write!(nodes, "<ol class=\"tree\">{dangling}</ol></li>");
    }

    format!("<ol class=\"tree\">{nodes}</ol>")
}

fn render_tree_chain<'a>(
    start: &'a SessionEntry,
    children_by_parent: &HashMap<Option<&'a str>, Vec<&'a SessionEntry>>,
    active_path: &HashSet<String>,
    active_leaf: Option<&str>,
    ancestors: &HashSet<&'a str>,
    rendered_ids: &mut HashSet<&'a str>,
) -> String {
    let mut chain: Vec<&SessionEntry> = Vec::new();
    let mut fork_children: Vec<&SessionEntry> = Vec::new();
    let mut chain_ancestors: HashSet<&str> = ancestors.clone();
    let mut current: Option<&SessionEntry> = Some(start);

    while let Some(cur) = current {
        rendered_ids.insert(cur.id());
        chain.push(cur);
        chain_ancestors.insert(cur.id());
        let kids: Vec<&SessionEntry> = children_by_parent
            .get(&Some(cur.id()))
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|c| !chain_ancestors.contains(c.id()))
                    .collect()
            })
            .unwrap_or_default();
        if kids.len() == 1 {
            current = Some(kids[0]);
        } else {
            fork_children = kids;
            current = None;
        }
    }

    let mut parts = String::new();
    let last = chain.len() - 1;
    for (position, node) in chain.iter().enumerate() {
        let mut nested_html = String::new();
        if position == last && !fork_children.is_empty() {
            let mut inner = String::new();
            for child in &fork_children {
                if !rendered_ids.contains(child.id()) {
                    inner.push_str(&render_tree_chain(
                        child,
                        children_by_parent,
                        active_path,
                        active_leaf,
                        &chain_ancestors,
                        rendered_ids,
                    ));
                }
            }
            nested_html = format!("<ol class=\"tree\">{inner}</ol>");
        }
        parts.push_str(&render_tree_node(
            node,
            &nested_html,
            active_path,
            active_leaf,
        ));
    }
    parts
}

fn render_tree_node(
    entry: &SessionEntry,
    nested_html: &str,
    active_path: &HashSet<String>,
    active_leaf: Option<&str>,
) -> String {
    let mut classes = String::from("tree-node");
    if active_path.contains(entry.id()) {
        classes.push_str(" active-path");
    }
    if Some(entry.id()) == active_leaf {
        classes.push_str(" active-leaf");
    }
    let summary = entry_summary(entry);
    let label = if summary.is_empty() {
        entry_title(entry)
    } else {
        format!("{}: {}", entry_title(entry), summary)
    };
    format!(
        "<li class=\"{}\"><a class=\"node-link\" href=\"#entry-{}\"><span class=\"icon\">{}</span><span class=\"node-type\">{}</span></a>{}</li>",
        classes,
        attr(entry.id()),
        entry_icon(entry),
        escape(&label),
        nested_html
    )
}

fn render_entry_details(
    entries: &[&SessionEntry],
    active_path: &HashSet<String>,
    active_leaf: Option<&str>,
) -> String {
    if entries.is_empty() {
        return "<article><p class=\"empty\">No session entries were found.</p></article>"
            .to_string();
    }
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        out.push_str(&render_entry_detail(
            index + 1,
            entry,
            active_path,
            active_leaf,
        ));
    }
    out
}

fn render_entry_detail(
    index: usize,
    entry: &SessionEntry,
    active_path: &HashSet<String>,
    active_leaf: Option<&str>,
) -> String {
    let mut classes = String::from("entry-card");
    let mut status_bits: Vec<&str> = Vec::new();
    if active_path.contains(entry.id()) {
        status_bits.push("active path");
    }
    if Some(entry.id()) == active_leaf {
        status_bits.push("active leaf");
    }
    if !status_bits.is_empty() {
        classes.push_str(" active-entry");
    }
    let status_html = if status_bits.is_empty() {
        String::new()
    } else {
        format!(
            "<span class=\"entry-status\">{}</span>",
            escape(&status_bits.join(" \u{b7} "))
        )
    };
    let body = render_entry_body(entry);
    format!(
        "<article id=\"entry-{}\" class=\"{}\"><p class=\"entry-index\"><span class=\"icon\">{}</span>{:02} \u{b7} {}{}</p><dl class=\"entry-meta\"><dt>id</dt><dd><code>{}</code></dd><dt>parent</dt><dd>{}</dd><dt>timestamp</dt><dd>{}</dd></dl>{}</article>",
        attr(entry.id()),
        classes,
        entry_icon(entry),
        index,
        escape(&entry_title(entry)),
        status_html,
        escape(entry.id()),
        entry_parent_html(entry),
        escape(&format_timestamp(entry_timestamp(entry))),
        body,
    )
}

fn render_entry_body(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message(e) => render_message_entry(&e.message),
        SessionEntry::ModelChange(e) => {
            format!("<p>Model changed to <code>{}</code>.</p>", escape(&e.model))
        }
        SessionEntry::ThinkingLevelChange(e) => {
            let level = e.thinking_level.as_deref().unwrap_or("off");
            format!(
                "<p>Thinking level changed to <code>{}</code>.</p>",
                escape(level)
            )
        }
        SessionEntry::Compaction(e) => {
            format!(
                "<p>Compaction summary:</p><pre>{}</pre>{}",
                escape(&e.summary),
                render_list("Replaces entries", &e.replaces_entry_ids)
            )
        }
        SessionEntry::BranchSummary(e) => {
            let branch_root = e.branch_root_id.as_deref().unwrap_or("none");
            format!(
                "<p>Branch root: <code>{}</code></p><pre>{}</pre>",
                escape(branch_root),
                escape(&e.summary)
            )
        }
        SessionEntry::Label(e) => {
            format!(
                "<p>Session label: <strong>{}</strong></p>",
                escape(&e.label)
            )
        }
        SessionEntry::Leaf(e) => {
            let leaf = e.entry_id.as_deref().unwrap_or("none");
            format!("<p>Active leaf pointer: <code>{}</code></p>", escape(leaf))
        }
        SessionEntry::SessionInfo(e) => {
            format!(
                "<p>Title: <strong>{}</strong></p><p>Working directory: <code>{}</code></p><p>Created: {}</p>",
                escape(e.title.as_deref().unwrap_or("Untitled")),
                escape(e.cwd.as_deref().unwrap_or("unknown")),
                escape(&format_timestamp(e.created_at)),
            )
        }
        SessionEntry::Custom(e) => {
            format!(
                "<p>Custom namespace: <code>{}</code></p>{}",
                escape(&e.namespace),
                render_json_block(&Value::Object(e.data.clone()))
            )
        }
    }
}

fn render_message_entry(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(m) => format!(
            "<p class=\"message-role\"><span class=\"icon\">{}</span>user</p><pre>{}</pre>",
            ICON_USER,
            escape(&m.text())
        ),
        AgentMessage::Assistant(m) => {
            let tool_calls = m.tool_calls();
            let tool_calls_html = if tool_calls.is_empty() {
                String::new()
            } else {
                let mut inner = String::new();
                for call in &tool_calls {
                    let _ = write!(
                        inner,
                        "<li><code>{}</code> <code>{}</code>{}</li>",
                        escape(&call.name),
                        escape(&call.id),
                        render_json_block(&Value::Object(call.arguments.clone())),
                    );
                }
                format!("<h4>Tool calls</h4><ul>{inner}</ul>")
            };
            let text = m.text();
            let content = if text.is_empty() {
                "(no assistant text)".to_string()
            } else {
                text
            };
            format!(
                "<p class=\"message-role\"><span class=\"icon\">{}</span>assistant</p><pre>{}</pre>{}",
                ICON_ASSISTANT,
                escape(&content),
                tool_calls_html
            )
        }
        AgentMessage::ToolResult(m) => {
            let metadata = [
                ("tool", m.tool_name.clone()),
                ("tool_call_id", m.tool_call_id.clone()),
                ("is_error", python_bool(m.is_error)),
            ];
            let mut body = format!(
                "<p class=\"message-role\"><span class=\"icon\">{}</span>tool result</p>{}<pre>{}</pre>",
                ICON_TOOL,
                render_metadata(&metadata),
                escape(&m.text())
            );
            if let Some(details @ Value::Object(_)) = &m.details {
                let _ = write!(body, "<h4>Details</h4>{}", render_json_block(details));
            }
            body
        }
        // Unreachable for the golden/tests: tau falls back to
        // `model_dump_json(indent=2)` (its non-alias form). A `MessageEntry`
        // wrapping bash/custom/summary messages is not exercised here; render a
        // best-effort pretty dump. See the parity note in dev-notes.
        other => format!(
            "<pre>{}</pre>",
            escape(&serde_json::to_string_pretty(other).unwrap_or_default())
        ),
    }
}

fn render_metadata(items: &[(&str, String)]) -> String {
    let mut inner = String::new();
    for (key, value) in items {
        let _ = write!(
            inner,
            "<dt>{}</dt><dd><code>{}</code></dd>",
            escape(key),
            escape(value)
        );
    }
    format!("<dl class=\"entry-meta\">{inner}</dl>")
}

fn render_list(title: &str, values: &[String]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let mut inner = String::new();
    for value in values {
        let _ = write!(inner, "<li><code>{}</code></li>", escape(value));
    }
    format!("<h4>{}</h4><ul>{}</ul>", escape(title), inner)
}

// ---------------------------------------------------------------------------
// Per-entry metadata
// ---------------------------------------------------------------------------

fn entry_icon(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::Message(e) => match &e.message {
            AgentMessage::User(_) => ICON_USER,
            AgentMessage::Assistant(_) => ICON_ASSISTANT,
            AgentMessage::ToolResult(_) => ICON_TOOL,
            _ => ICON_GENERIC,
        },
        SessionEntry::ModelChange(_) | SessionEntry::ThinkingLevelChange(_) => ICON_MODEL,
        SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_) => ICON_BRANCH,
        SessionEntry::Label(_) => ICON_LABEL,
        SessionEntry::SessionInfo(_) => ICON_INFO,
        _ => ICON_GENERIC,
    }
}

fn entry_parent_html(entry: &SessionEntry) -> String {
    match entry.parent_id() {
        None => "<span class=\"empty\">root</span>".to_string(),
        Some(parent) => format!(
            "<a href=\"#entry-{}\"><code>{}</code></a>",
            attr(parent),
            escape(parent)
        ),
    }
}

fn entry_title(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message(e) => e.message.role().to_string(),
        SessionEntry::ModelChange(_) => "model change".to_string(),
        SessionEntry::ThinkingLevelChange(_) => "thinking level change".to_string(),
        SessionEntry::Compaction(_) => "compaction".to_string(),
        SessionEntry::BranchSummary(_) => "branch summary".to_string(),
        SessionEntry::Label(_) => "label".to_string(),
        SessionEntry::Leaf(_) => "leaf pointer".to_string(),
        SessionEntry::SessionInfo(_) => "session info".to_string(),
        SessionEntry::Custom(e) => format!("custom:{}", e.namespace),
    }
}

fn entry_summary(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message(e) => match &e.message {
            AgentMessage::ToolResult(m) => {
                format!("{}: {}", m.tool_name, summarize_text(&m.text()))
            }
            AgentMessage::Assistant(m) if !m.tool_calls().is_empty() => {
                let tool_names = m
                    .tool_calls()
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                let text = summarize_text(&m.text());
                let text = if text.is_empty() {
                    "tool call".to_string()
                } else {
                    text
                };
                format!("{text} [{tool_names}]")
            }
            other => summarize_text(&other.text()),
        },
        SessionEntry::ModelChange(e) => e.model.clone(),
        SessionEntry::ThinkingLevelChange(e) => e
            .thinking_level
            .clone()
            .unwrap_or_else(|| "off".to_string()),
        SessionEntry::Compaction(e) => summarize_text(&e.summary),
        SessionEntry::BranchSummary(e) => summarize_text(&e.summary),
        SessionEntry::Label(e) => e.label.clone(),
        SessionEntry::Leaf(e) => e.entry_id.clone().unwrap_or_else(|| "none".to_string()),
        SessionEntry::SessionInfo(e) => e
            .title
            .clone()
            .or_else(|| e.cwd.clone())
            .unwrap_or_else(|| "session metadata".to_string()),
        SessionEntry::Custom(e) => format!("{} field(s)", e.data.len()),
    }
}

fn entry_timestamp(entry: &SessionEntry) -> f64 {
    match entry {
        SessionEntry::Message(e) => e.timestamp,
        SessionEntry::ModelChange(e) => e.timestamp,
        SessionEntry::ThinkingLevelChange(e) => e.timestamp,
        SessionEntry::Compaction(e) => e.timestamp,
        SessionEntry::BranchSummary(e) => e.timestamp,
        SessionEntry::Label(e) => e.timestamp,
        SessionEntry::Leaf(e) => e.timestamp,
        SessionEntry::SessionInfo(e) => e.timestamp,
        SessionEntry::Custom(e) => e.timestamp,
    }
}

/// Collapse runs of whitespace (tau `" ".join(text.split())`) and truncate to a
/// 92-code-point budget with a trailing `...`. Length and slicing are by Unicode
/// code point, matching Python's `len`/slicing.
fn summarize_text(text: &str) -> String {
    const LIMIT: usize = 92;
    let summary: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = summary.chars().collect();
    if chars.len() <= LIMIT {
        return summary;
    }
    let head: String = chars[..LIMIT - 3].iter().collect();
    format!("{}...", head.trim_end())
}

fn python_bool(value: bool) -> String {
    if value { "True" } else { "False" }.to_string()
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

fn now_unix_secs() -> i64 {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_secs()).unwrap_or(i64::MAX)
}

/// tau `_format_timestamp`: `datetime.fromtimestamp(ts, UTC).replace(microsecond=0).isoformat()`.
fn format_timestamp(timestamp: f64) -> String {
    iso_utc(timestamp.floor() as i64)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SS+00:00` (UTC), matching Python's
/// `datetime.isoformat()` with `microsecond=0`. Civil-date conversion
/// (Howard Hinnant's algorithm).
fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
}

// ---------------------------------------------------------------------------
// HTML escaping (Python `html.escape`)
// ---------------------------------------------------------------------------

/// `html.escape(str(value), quote=False)` — escapes `&`, `<`, `>` only.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `html.escape(str(value), quote=True)` — also escapes `"` and `'`.
fn attr(value: &str) -> String {
    escape(value).replace('"', "&quot;").replace('\'', "&#x27;")
}

// ---------------------------------------------------------------------------
// JSON block: sorted `json.dumps(indent=2)` + Pygments highlight
// ---------------------------------------------------------------------------

fn render_json_block(value: &Value) -> String {
    let source = json_dump(value);
    format!(
        "<pre class=\"highlight\">{}</pre>",
        format_highlight(&source)
    )
}

/// `json.dumps(value, indent=2, sort_keys=True)`: recursively key-sorted,
/// 2-space indent, `ensure_ascii` (non-ASCII → `\uXXXX`).
fn json_dump(value: &Value) -> String {
    let sorted = sort_value(value);
    ensure_ascii(&to_pretty_json(&sorted))
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), sort_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

/// Escape non-ASCII scalars as lowercase `\uXXXX` (surrogate pairs for astral
/// code points), matching Python `json.dumps(ensure_ascii=True)`. Safe over the
/// whole document: only string-literal contents contain non-ASCII.
fn ensure_ascii(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let code = ch as u32;
        if code <= 0x7F {
            out.push(ch);
        } else if code <= 0xFFFF {
            let _ = write!(out, "\\u{code:04x}");
        } else {
            let c = code - 0x1_0000;
            let hi = 0xD800 + (c >> 10);
            let lo = 0xDC00 + (c & 0x3FF);
            let _ = write!(out, "\\u{hi:04x}\\u{lo:04x}");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pygments port: JsonLexer + HtmlFormatter(nowrap=True)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tok {
    Punctuation,
    Whitespace,
    NameTag,
    StringDouble,
    KeywordConstant,
    NumberInteger,
    NumberFloat,
}

impl Tok {
    /// CSS class (Pygments `STANDARD_TYPES`).
    fn css(self) -> &'static str {
        match self {
            Tok::Punctuation => "p",
            Tok::Whitespace => "w",
            Tok::NameTag => "nt",
            Tok::StringDouble => "s2",
            Tok::KeywordConstant => "kc",
            Tok::NumberInteger => "mi",
            Tok::NumberFloat => "mf",
        }
    }
}

/// Port of `JsonLexer.get_tokens_unprocessed` for well-formed JSON (the comment
/// and error branches are unreachable for `json.dumps` output). Strings are
/// queued so a following `:` can retag a key `String.Double` → `Name.Tag`.
fn json_tokens(text: &str) -> Vec<(Tok, String)> {
    const INTEGERS: &str = "-0123456789";
    const FLOATS: &str = ".eE+";
    const CONSTANTS: &str = "truefalsenull";
    const PUNCTUATIONS: &str = "{}[],";
    const WHITESPACES: &str = " \n\r\t";

    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(Tok, String)> = Vec::new();
    // Queue of (token, text) awaiting a possible key retag.
    let mut queue: Vec<(Tok, String)> = Vec::new();

    let mut in_string = false;
    let mut in_escape = false;
    let mut in_unicode_escape = 0u8;
    let mut in_whitespace = false;
    let mut in_constant = false;
    let mut in_number = false;
    let mut in_float = false;
    let mut in_punctuation = false;
    let mut start = 0usize;

    let slice = |a: usize, b: usize| -> String { chars[a..b].iter().collect() };

    let mut stop = 0usize;
    while stop < chars.len() {
        let character = chars[stop];

        if in_string {
            if in_unicode_escape > 0 {
                if character.is_ascii_hexdigit() {
                    in_unicode_escape -= 1;
                    if in_unicode_escape == 0 {
                        in_escape = false;
                    }
                } else {
                    in_unicode_escape = 0;
                    in_escape = false;
                }
            } else if in_escape {
                if character == 'u' {
                    in_unicode_escape = 4;
                } else {
                    in_escape = false;
                }
            } else if character == '\\' {
                in_escape = true;
            } else if character == '"' {
                queue.push((Tok::StringDouble, slice(start, stop + 1)));
                in_string = false;
                in_escape = false;
                in_unicode_escape = 0;
            }
            stop += 1;
            continue;
        } else if in_whitespace {
            if WHITESPACES.contains(character) {
                stop += 1;
                continue;
            }
            let text = slice(start, stop);
            if queue.is_empty() {
                out.push((Tok::Whitespace, text));
            } else {
                queue.push((Tok::Whitespace, text));
            }
            in_whitespace = false;
            // fall through
        } else if in_constant {
            if CONSTANTS.contains(character) {
                stop += 1;
                continue;
            }
            out.push((Tok::KeywordConstant, slice(start, stop)));
            in_constant = false;
            // fall through
        } else if in_number {
            if INTEGERS.contains(character) {
                stop += 1;
                continue;
            } else if FLOATS.contains(character) {
                in_float = true;
                stop += 1;
                continue;
            }
            out.push((
                if in_float {
                    Tok::NumberFloat
                } else {
                    Tok::NumberInteger
                },
                slice(start, stop),
            ));
            in_number = false;
            in_float = false;
            // fall through
        } else if in_punctuation {
            if PUNCTUATIONS.contains(character) {
                stop += 1;
                continue;
            }
            out.push((Tok::Punctuation, slice(start, stop)));
            in_punctuation = false;
            // fall through
        }

        start = stop;

        if character == '"' {
            in_string = true;
        } else if WHITESPACES.contains(character) {
            in_whitespace = true;
        } else if character == 'f' || character == 'n' || character == 't' {
            out.append(&mut queue);
            in_constant = true;
        } else if INTEGERS.contains(character) {
            out.append(&mut queue);
            in_number = true;
        } else if character == ':' {
            for (tok, text) in queue.drain(..) {
                if tok == Tok::StringDouble {
                    out.push((Tok::NameTag, text));
                } else {
                    out.push((tok, text));
                }
            }
            in_punctuation = true;
        } else if PUNCTUATIONS.contains(character) {
            out.append(&mut queue);
            in_punctuation = true;
        } else {
            // Unreachable for json.dumps output; keep parsing defensively.
            out.append(&mut queue);
        }
        stop += 1;
    }

    // Flush trailing state.
    out.append(&mut queue);
    if in_float {
        out.push((Tok::NumberFloat, slice(start, chars.len())));
    } else if in_number {
        out.push((Tok::NumberInteger, slice(start, chars.len())));
    } else if in_constant {
        out.push((Tok::KeywordConstant, slice(start, chars.len())));
    } else if in_whitespace {
        out.push((Tok::Whitespace, slice(start, chars.len())));
    } else if in_punctuation {
        out.push((Tok::Punctuation, slice(start, chars.len())));
    }
    out
}

/// Port of `HtmlFormatter._format_lines` (`nowrap=True`). Reproduces the
/// same-class coalescing, per-line span closing, and empty-part handling.
fn format_highlight(source: &str) -> String {
    let tokens = json_tokens(source);
    let lsep = "\n";
    let mut lspan = String::new();
    let mut line: Vec<String> = Vec::new();
    let mut out = String::new();

    for (tok, value) in &tokens {
        let cspan = format!("<span class=\"{}\">", tok.css());
        let escaped = format_escape(value);
        let parts: Vec<&str> = escaped.split('\n').collect();
        let n = parts.len();

        for part in &parts[..n - 1] {
            if !line.is_empty() {
                if lspan != cspan && !part.is_empty() {
                    if !lspan.is_empty() {
                        line.push("</span>".to_string());
                    }
                    line.push(cspan.clone());
                    line.push((*part).to_string());
                    if !cspan.is_empty() {
                        line.push("</span>".to_string());
                    }
                    line.push(lsep.to_string());
                } else {
                    line.push((*part).to_string());
                    if !lspan.is_empty() {
                        line.push("</span>".to_string());
                    }
                    line.push(lsep.to_string());
                }
                out.push_str(&line.concat());
                line.clear();
            } else if !part.is_empty() {
                out.push_str(&cspan);
                out.push_str(part);
                out.push_str("</span>");
                out.push_str(lsep);
            } else {
                out.push_str(lsep);
            }
        }

        let last = parts[n - 1];
        if !line.is_empty() && !last.is_empty() {
            if lspan != cspan {
                if !lspan.is_empty() {
                    line.push("</span>".to_string());
                }
                line.push(cspan.clone());
                line.push(last.to_string());
                lspan.clone_from(&cspan);
            } else {
                line.push(last.to_string());
            }
        } else if !last.is_empty() {
            line = vec![cspan.clone(), last.to_string()];
            lspan.clone_from(&cspan);
        }
    }

    if !line.is_empty() {
        if !lspan.is_empty() {
            line.push("</span>".to_string());
        }
        line.push(lsep.to_string());
        out.push_str(&line.concat());
    }
    out
}

/// Pygments `_escape_html_table`: note `'` → `&#39;` (distinct from
/// `html.escape`'s `&#x27;`).
fn format_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// JSONL export densification (re-insert `exclude_none`-omitted nulls)
// ---------------------------------------------------------------------------

fn field(map: &Map<String, Value>, key: &str) -> Value {
    map.get(key).cloned().unwrap_or(Value::Null)
}

fn ordered(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// Re-serialize one entry in tau's default `model_dump_json()` shape (nulls
/// included, `by_alias`), starting from rho's `exclude_none` `to_value` output.
fn densify_entry(value: &Value) -> Value {
    let Some(m) = value.as_object() else {
        return value.clone();
    };
    let ty = m.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = vec![
        ("id", field(m, "id")),
        ("parent_id", field(m, "parent_id")),
        ("timestamp", field(m, "timestamp")),
        ("type", field(m, "type")),
    ];
    match ty {
        "message" => out.push((
            "message",
            densify_message(m.get("message").unwrap_or(&Value::Null)),
        )),
        "model_change" => out.push(("model", field(m, "model"))),
        "thinking_level_change" => out.push(("thinking_level", field(m, "thinking_level"))),
        "compaction" => {
            out.push(("summary", field(m, "summary")));
            out.push(("replaces_entry_ids", field(m, "replaces_entry_ids")));
        }
        "branch_summary" => {
            out.push(("summary", field(m, "summary")));
            out.push(("branch_root_id", field(m, "branch_root_id")));
        }
        "label" => out.push(("label", field(m, "label"))),
        "leaf" => out.push(("entry_id", field(m, "entry_id"))),
        "session_info" => {
            out.push(("created_at", field(m, "created_at")));
            out.push(("cwd", field(m, "cwd")));
            out.push(("title", field(m, "title")));
        }
        "custom" => {
            out.push(("namespace", field(m, "namespace")));
            out.push(("data", field(m, "data")));
        }
        _ => return value.clone(),
    }
    ordered(out)
}

fn densify_message(value: &Value) -> Value {
    let Some(m) = value.as_object() else {
        return value.clone();
    };
    match m.get("role").and_then(Value::as_str).unwrap_or("") {
        "user" => ordered(vec![
            ("role", field(m, "role")),
            ("content", densify_content_field(m.get("content"))),
            ("timestamp", field(m, "timestamp")),
        ]),
        "assistant" => ordered(vec![
            ("role", field(m, "role")),
            ("content", densify_block_list(m.get("content"))),
            ("api", field(m, "api")),
            ("provider", field(m, "provider")),
            ("model", field(m, "model")),
            ("responseModel", field(m, "responseModel")),
            ("responseId", field(m, "responseId")),
            ("diagnostics", densify_diagnostics(m.get("diagnostics"))),
            ("usage", densify_usage(m.get("usage"))),
            ("stopReason", field(m, "stopReason")),
            ("errorMessage", field(m, "errorMessage")),
            ("timestamp", field(m, "timestamp")),
        ]),
        "toolResult" => ordered(vec![
            ("role", field(m, "role")),
            ("toolCallId", field(m, "toolCallId")),
            ("toolName", field(m, "toolName")),
            ("content", densify_block_list(m.get("content"))),
            ("details", field(m, "details")),
            ("addedToolNames", field(m, "addedToolNames")),
            ("isError", field(m, "isError")),
            ("timestamp", field(m, "timestamp")),
        ]),
        "bashExecution" => ordered(vec![
            ("role", field(m, "role")),
            ("command", field(m, "command")),
            ("output", field(m, "output")),
            ("exitCode", field(m, "exitCode")),
            ("cancelled", field(m, "cancelled")),
            ("truncated", field(m, "truncated")),
            ("fullOutputPath", field(m, "fullOutputPath")),
            ("timestamp", field(m, "timestamp")),
            ("excludeFromContext", field(m, "excludeFromContext")),
        ]),
        "custom" => ordered(vec![
            ("role", field(m, "role")),
            ("customType", field(m, "customType")),
            ("content", densify_content_field(m.get("content"))),
            ("display", field(m, "display")),
            ("details", field(m, "details")),
            ("timestamp", field(m, "timestamp")),
        ]),
        "branchSummary" => ordered(vec![
            ("role", field(m, "role")),
            ("summary", field(m, "summary")),
            ("fromId", field(m, "fromId")),
            ("timestamp", field(m, "timestamp")),
        ]),
        "compactionSummary" => ordered(vec![
            ("role", field(m, "role")),
            ("summary", field(m, "summary")),
            ("tokensBefore", field(m, "tokensBefore")),
            ("timestamp", field(m, "timestamp")),
        ]),
        _ => value.clone(),
    }
}

fn densify_content_field(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Array(items)) => {
            Value::Array(items.iter().map(densify_content_block).collect())
        }
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn densify_block_list(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Array(items)) => {
            Value::Array(items.iter().map(densify_content_block).collect())
        }
        Some(other) => other.clone(),
        None => Value::Array(Vec::new()),
    }
}

fn densify_content_block(value: &Value) -> Value {
    let Some(m) = value.as_object() else {
        return value.clone();
    };
    match m.get("type").and_then(Value::as_str).unwrap_or("") {
        "text" => ordered(vec![
            ("type", field(m, "type")),
            ("text", field(m, "text")),
            ("textSignature", field(m, "textSignature")),
        ]),
        "thinking" => ordered(vec![
            ("type", field(m, "type")),
            ("thinking", field(m, "thinking")),
            ("thinkingSignature", field(m, "thinkingSignature")),
            ("redacted", field(m, "redacted")),
        ]),
        "image" => ordered(vec![
            ("type", field(m, "type")),
            ("data", field(m, "data")),
            ("mimeType", field(m, "mimeType")),
        ]),
        "toolCall" => ordered(vec![
            ("type", field(m, "type")),
            ("id", field(m, "id")),
            ("name", field(m, "name")),
            ("arguments", field(m, "arguments")),
            ("thoughtSignature", field(m, "thoughtSignature")),
        ]),
        _ => value.clone(),
    }
}

fn densify_diagnostics(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Array(items)) => Value::Array(items.iter().map(densify_diagnostic).collect()),
        _ => Value::Null,
    }
}

fn densify_diagnostic(value: &Value) -> Value {
    let Some(m) = value.as_object() else {
        return value.clone();
    };
    ordered(vec![
        ("type", field(m, "type")),
        ("timestamp", field(m, "timestamp")),
        ("error", densify_diagnostic_error(m.get("error"))),
        ("details", field(m, "details")),
    ])
}

fn densify_diagnostic_error(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(m)) => ordered(vec![
            ("name", field(m, "name")),
            ("message", field(m, "message")),
            ("stack", field(m, "stack")),
            ("code", field(m, "code")),
        ]),
        _ => Value::Null,
    }
}

fn densify_usage(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(m)) => ordered(vec![
            ("input", field(m, "input")),
            ("output", field(m, "output")),
            ("cacheRead", field(m, "cacheRead")),
            ("cacheWrite", field(m, "cacheWrite")),
            ("cacheWrite1H", field(m, "cacheWrite1H")),
            ("reasoning", field(m, "reasoning")),
            ("totalTokens", field(m, "totalTokens")),
            ("cost", densify_cost(m.get("cost"))),
        ]),
        _ => Value::Null,
    }
}

fn densify_cost(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(m)) => ordered(vec![
            ("input", field(m, "input")),
            ("output", field(m, "output")),
            ("cacheRead", field(m, "cacheRead")),
            ("cacheWrite", field(m, "cacheWrite")),
            ("total", field(m, "total")),
        ]),
        _ => Value::Null,
    }
}

// ===========================================================================
// Static template segments + icon SVGs (machine-extracted; byte-exact)
// ===========================================================================

const ICON_USER: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="5" r="2.75" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M2.5 14c.6-3 2.9-4.5 5.5-4.5s4.9 1.5 5.5 4.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"####;
const ICON_ASSISTANT: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.5" y="3.5" width="11" height="8" rx="1.8" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M5.5 7.2h0M10.5 7.2h0" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/><path d="M8 1.5v2M5.5 13.5v1M10.5 13.5v1" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"####;
const ICON_TOOL: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><rect x="8.7" y="1.6" width="3.2" height="5.6" rx=".7" transform="rotate(45 10.3 4.4)" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M8.1 6.8 3 11.9c-.6.6-.6 1.5.0 2.1.6.6 1.5.6 2.1.0l5.1-5.1" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/></svg>"####;
const ICON_BRANCH: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="4.5" cy="3.5" r="1.5" fill="none" stroke="currentColor" stroke-width="1.2"/><circle cx="4.5" cy="12.5" r="1.5" fill="none" stroke="currentColor" stroke-width="1.2"/><circle cx="11.5" cy="8" r="1.5" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M4.5 5v3.5c0 1.1.9 2 2 2h3.5M4.5 8.5V5" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>"####;
const ICON_LABEL: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 4.2c0-.9.8-1.7 1.7-1.7h4.4c.5.0.9.2 1.2.5l4 4c.6.6.6 1.7.0 2.4l-4.4 4.4c-.6.6-1.7.6-2.4.0l-4-4c-.3-.3-.5-.7-.5-1.2Z" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/><circle cx="5.6" cy="5.6" r="1" fill="currentColor"/></svg>"####;
const ICON_INFO: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="5.75" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M8 7.2v3.4M8 5.2h0" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>"####;
const ICON_MODEL: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 1.8 13.5 5v6L8 14.2 2.5 11V5Z" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/><path d="M2.5 5 8 8l5.5-3M8 8v6.2" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>"####;
const ICON_GENERIC: &str = r####"<svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.5" y="2.5" width="11" height="11" rx="1.6" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M5 5.5h6M5 8h6M5 10.5h4" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/></svg>"####;

// GENERATED from fixtures/export/kitchen-sink.html by extract_template.py.
// Byte-exact static template segments; do not hand-edit.

const HTML_1: &str = r####"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>"####;

const HTML_2: &str = r####"</title>
  <style>
    :root {
      color-scheme: light;
      --canvas: #ffffff;
      --surface: #ffffff;
      --surface-muted: #f6f8fc;
      --text: #13213c;
      --muted: #54607a;
      --line: #dce4f2;
      --line-strong: #c9d6ee;
      --accent: #1b3fa0;
      --code-bg: #f6f8fc;
      --serif: Charter, "Iowan Old Style", Georgia, ui-serif, serif;
      --sans: "Space Grotesk", ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
      --mono: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
      font-family: var(--serif);
    }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme="light"]) {
        color-scheme: dark;
        --canvas: #0f1420;
        --surface: #141a29;
        --surface-muted: #1a2133;
        --text: #e7ecf7;
        --muted: #9aa5c0;
        --line: #262f47;
        --line-strong: #333f5c;
        --accent: #7fa0f0;
        --code-bg: #171e30;
      }
    }
    :root[data-theme="dark"] {
      color-scheme: dark;
      --canvas: #0f1420;
      --surface: #141a29;
      --surface-muted: #1a2133;
      --text: #e7ecf7;
      --muted: #9aa5c0;
      --line: #262f47;
      --line-strong: #333f5c;
      --accent: #7fa0f0;
      --code-bg: #171e30;
    }
    * { box-sizing: border-box; }
    html { scroll-behavior: smooth; }
    body {
      margin: 0;
      background: var(--canvas);
      color: var(--text);
      line-height: 1.55;
    }
    header {
      max-width: 1280px;
      margin: 0 auto;
      padding: 32px clamp(18px, 4vw, 48px) 20px;
    }
    h1, h2, h3, h4 { margin: 0; line-height: 1.25; font-family: var(--sans); }
    h1 {
      font-size: clamp(1.5rem, 2.4vw, 1.9rem);
      font-weight: 500;
      letter-spacing: -0.01em;
    }
    h2 {
      color: var(--muted);
      font-size: 0.7rem;
      font-weight: 500;
      letter-spacing: 0.12em;
      margin-bottom: 12px;
      text-transform: uppercase;
    }
    h3 {
      font-size: 0.66rem;
      font-weight: 500;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }
    h4 {
      font-size: 0.7rem;
      font-weight: 500;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
      margin-top: 16px;
    }
    code, pre {
      font-family: var(--mono);
      font-size: 0.85em;
    }
    p { margin: 0; }
    pre {
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      background: var(--code-bg);
      border: 1px solid var(--line);
      border-radius: 4px;
      padding: 12px 14px;
      margin: 10px 0 0;
    }
    .header-top {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
    }
    .eyebrow {
      font-family: var(--sans);
      color: var(--muted);
      font-size: 0.7rem;
      font-weight: 500;
      letter-spacing: 0.14em;
      margin: 0;
      text-transform: uppercase;
    }
    .theme-toggle {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 28px;
      height: 28px;
      padding: 0;
      color: var(--muted);
      background: none;
      border: 1px solid var(--line-strong);
      border-radius: 50%;
      cursor: pointer;
      transition: color .15s, border-color .15s;
    }
    .theme-toggle:hover { color: var(--accent); border-color: var(--accent); }
    .theme-toggle .icon { width: 14px; height: 14px; }
    .theme-toggle .theme-icon-dark { display: none; }
    :root[data-theme="dark"] .theme-toggle .theme-icon-light { display: none; }
    :root[data-theme="dark"] .theme-toggle .theme-icon-dark { display: inline-block; }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme="light"]) .theme-toggle .theme-icon-light { display: none; }
      :root:not([data-theme="light"]) .theme-toggle .theme-icon-dark { display: inline-block; }
    }
    .source, .generated {
      margin: 6px 0 0;
      color: var(--muted);
      font-size: 0.85rem;
      font-family: var(--sans);
    }
    .export-meta {
      border-top: 1px solid var(--line);
      display: flex;
      flex-wrap: wrap;
      gap: 4px 18px;
      margin-top: 20px;
      padding-top: 14px;
    }
    main {
      display: grid;
      grid-template-columns: minmax(240px, 320px) minmax(0, 1fr);
      gap: 40px;
      max-width: 1280px;
      margin: 0 auto;
      padding: 18px clamp(18px, 4vw, 48px) 56px;
    }
    aside {
      position: sticky;
      top: 18px;
      align-self: start;
      max-height: calc(100vh - 32px);
      overflow: auto;
      padding: 2px 16px 4px 0;
      border-right: 1px solid var(--line);
    }
    .icon {
      display: inline-block;
      flex: 0 0 auto;
      width: 13px;
      height: 13px;
      color: var(--muted);
    }
    .icon svg { display: block; width: 100%; height: 100%; }
    article {
      margin: 0;
      padding: 18px 0;
      border-bottom: 1px solid var(--line);
    }
    article:first-child { padding-top: 0; }
    article:last-child { border-bottom: 0; }
    article.active-entry {
      background: var(--surface-muted);
      margin: 0 -16px;
      padding: 18px 16px;
    }
    article.active-entry:first-child { padding-top: 18px; }
    .entry-index {
      display: flex;
      align-items: center;
      gap: 7px;
      font-family: var(--sans);
      font-size: 0.68rem;
      font-weight: 500;
      letter-spacing: 0.08em;
      color: var(--muted);
      text-transform: uppercase;
    }
    .entry-index .icon { color: var(--muted); }
    .entry-status {
      margin-left: auto;
      color: var(--accent);
      font-weight: 500;
      letter-spacing: 0.04em;
      text-transform: none;
    }
    .tree {
      list-style: none;
      margin: 0;
      padding-left: 0;
    }
    .tree .tree {
      margin-left: 8px;
      padding-left: 13px;
      border-left: 1px solid var(--line);
    }
    .tree li { margin: 1px 0; }
    .node-link {
      display: flex;
      align-items: center;
      gap: 7px;
      color: var(--text);
      text-decoration: none;
      border-radius: 4px;
      padding: 5px 8px;
    }
    .node-link:hover { background: var(--surface-muted); }
    .active-path > .node-link { color: var(--accent); }
    .active-leaf > .node-link {
      background: var(--surface-muted);
      font-weight: 500;
    }
    .node-link .icon { color: var(--muted); }
    .active-path > .node-link .icon { color: var(--accent); }
    .node-type {
      display: block;
      font-family: var(--sans);
      font-size: 0.76rem;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .entry-meta {
      display: grid;
      grid-template-columns: max-content minmax(0, 1fr);
      gap: 3px 10px;
      margin: 10px 0 0;
      font-family: var(--sans);
      color: var(--muted);
      font-size: 0.78rem;
    }
    .entry-meta dt {
      font-weight: 500;
      text-transform: uppercase;
      letter-spacing: 0.06em;
      font-size: 0.65rem;
      align-self: baseline;
      padding-top: 2px;
    }
    .entry-meta dd { margin: 0; overflow-wrap: anywhere; }
    .message-role {
      display: flex;
      align-items: center;
      gap: 6px;
      margin: 0 0 4px;
      font-family: var(--sans);
      font-size: 0.7rem;
      font-weight: 500;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }
    .empty {
      color: var(--muted);
      font-style: italic;
    }
    pre.highlight { padding: 12px 14px; }
    .highlight .p { color: var(--muted); }
    .highlight .nt { color: var(--accent); }
    .highlight .s2, .highlight .s1 { color: #2f7a4f; }
    .highlight .mi, .highlight .mf { color: #a05a12; }
    .highlight .kc { color: #a02f6b; font-weight: 500; }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme="light"]) .highlight .s2,
      :root:not([data-theme="light"]) .highlight .s1 { color: #7fd08a; }
      :root:not([data-theme="light"]) .highlight .mi,
      :root:not([data-theme="light"]) .highlight .mf { color: #e0a95e; }
      :root:not([data-theme="light"]) .highlight .kc { color: #e58fc0; }
    }
    :root[data-theme="dark"] .highlight .s2,
    :root[data-theme="dark"] .highlight .s1 { color: #7fd08a; }
    :root[data-theme="dark"] .highlight .mi,
    :root[data-theme="dark"] .highlight .mf { color: #e0a95e; }
    :root[data-theme="dark"] .highlight .kc { color: #e58fc0; }
    @media (max-width: 820px) {
      main { grid-template-columns: 1fr; }
      aside {
        position: static;
        max-height: none;
        border-right: 0;
        border-bottom: 1px solid var(--line);
        padding: 2px 0 20px;
      }
      article.active-entry { margin: 0 -18px; padding: 18px 18px; }
    }
  </style>
</head>
<body>
  <header>
    <div class="header-top">
      <p class="eyebrow">Tau session export</p>
      <button
        type="button"
        class="theme-toggle"
        id="themeToggle"
        aria-label="Toggle light/dark theme"
      >
        <span class="icon theme-icon-light"><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="3" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M8 1.6v2M8 12.4v2M1.6 8h2M12.4 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg></span>
        <span class="icon theme-icon-dark"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13.2 9.8A5.6 5.6 0 0 1 6.2 2.8a5.6 5.6 0 1 0 7 7Z" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/></svg></span>
      </button>
    </div>
    <h1>"####;

const HTML_3: &str = r####"</h1>
    <div class="export-meta">
      "####;

const HTML_4: &str = r####"
      <p class="generated">
        Generated: <time datetime=""####;

const HTML_5: &str = r####"">"####;

const HTML_6: &str = r####"</time>
      </p>
    </div>
  </header>
  <main class="session-shell">
    <aside class="tree-rail">
      <h2>Session</h2>
      "####;

const HTML_7: &str = r####"
    </aside>
    <section class="entry-stream" aria-label="Session entries">
      <h2>Transcript</h2>
      "####;

const HTML_8: &str = r####"
    </section>
  </main>
  <script>
    (function () {
      var root = document.documentElement;
      var stored = null;
      try {
        stored = window.localStorage.getItem("tau-session-export-theme");
      } catch (err) {
        stored = null;
      }
      if (stored === "light" || stored === "dark") {
        root.setAttribute("data-theme", stored);
      }
      var toggle = document.getElementById("themeToggle");
      if (!toggle) {
        return;
      }
      toggle.addEventListener("click", function () {
        var prefersDark =
          window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
        var current = root.getAttribute("data-theme") || (prefersDark ? "dark" : "light");
        var next = current === "dark" ? "light" : "dark";
        root.setAttribute("data-theme", next);
        try {
          window.localStorage.setItem("tau-session-export-theme", next);
        } catch (err) {
          /* localStorage unavailable; theme choice just won't persist. */
        }
      });
    })();
  </script>
</body>
</html>
"####;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn export_json_uses_python_float_repr_not_serde_scientific() {
        // tau's export goes through `json.dumps`, whose `float.__repr__` renders a
        // small cost as `5e-07`; serde's default emits `5e-7`. The custom
        // formatter must preserve the Python shape (and keep `0.0` / whole-number
        // floats) so exports stay byte-compatible with tau (Codex PR#8 P2).
        let value = serde_json::json!({
            "cost": 0.000_000_5_f64,
            "whole": 0.0_f64,
            "ts": 1_731_234_567.0_f64,
        });
        let compact = to_compact_json(&value);
        assert_eq!(compact, r#"{"cost":5e-07,"whole":0.0,"ts":1731234567.0}"#);
        assert!(
            !compact.contains("5e-7,"),
            "no bare serde exponent: {compact}"
        );
        let pretty = to_pretty_json(&value);
        assert!(pretty.contains("\"cost\": 5e-07"), "{pretty}");
        assert!(pretty.contains("\"whole\": 0.0"));
    }

    use rho_agent::messages::{
        AgentMessage, AssistantContent, AssistantMessage, TextContent, ToolCall, ToolResultContent,
        ToolResultMessage, UserMessage,
    };
    use rho_agent::session::entries::{CompactionEntry, LeafEntry, MessageEntry};
    use rho_agent::session::jsonl::entries_from_json_lines;
    use serde_json::{Map, Value, json};

    // GENERATED by gen_testdata.py from tau; byte-exact. Do not hand-edit.

    const KITCHEN_JSONL_EXPECTED: &str = r####"{"id":"k0","parent_id":null,"timestamp":1731234567.0,"type":"session_info","created_at":1731234567.0,"cwd":"/work","title":"Everything"}
{"id":"k1","parent_id":"k0","timestamp":1731234568.0,"type":"label","label":"My Session"}
{"id":"k2","parent_id":"k1","timestamp":1731234569.0,"type":"model_change","model":"claude-sonnet"}
{"id":"k3","parent_id":"k2","timestamp":1731234570.0,"type":"thinking_level_change","thinking_level":"high"}
{"id":"k4","parent_id":"k3","timestamp":1731234571.0,"type":"message","message":{"role":"user","content":"hi 🌍","timestamp":1731234567000}}
{"id":"k5","parent_id":"k4","timestamp":1731234572.0,"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"hello 世界","textSignature":null}],"api":"unknown","provider":"unknown","model":"claude-sonnet","responseModel":null,"responseId":null,"diagnostics":null,"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cacheWrite1H":null,"reasoning":null,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","errorMessage":null,"timestamp":1731234567000}}
{"id":"k6","parent_id":"k5","timestamp":1731234573.0,"type":"custom","namespace":"ext.todo","data":{"items":["a","b"],"done":false}}
{"id":"k7","parent_id":"k6","timestamp":1731234574.0,"type":"thinking_level_change","thinking_level":null}
{"id":"k8","parent_id":"k7","timestamp":1731234575.0,"type":"leaf","entry_id":"k5"}
"####;

    const JSONL_INPUT: &str = r####"{"id":"a0","timestamp":1731234567.0,"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm","thinkingSignature":"sig","redacted":true},{"type":"text","text":"hi","textSignature":"ts"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"R.md","nested":{"x":null}},"thoughtSignature":"th"}],"api":"anthropic","provider":"anthropic","model":"m","responseModel":"m2","responseId":"r1","diagnostics":[{"type":"retry","timestamp":123,"error":{"name":"E","message":"boom","stack":"st","code":429},"details":{"k":null}}],"usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"cacheWrite1H":5,"reasoning":6,"totalTokens":7,"cost":{"input":0.1,"output":0.2,"cacheRead":0.0,"cacheWrite":0.0,"total":0.3}},"stopReason":"toolUse","errorMessage":"err","timestamp":111}}
{"id":"a1","parent_id":"a0","timestamp":1731234568.0,"type":"message","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"out"},{"type":"image","data":"BASE64","mimeType":"image/png"}],"details":{"bytes":13,"z":null},"addedToolNames":["grep"],"isError":true,"timestamp":222}}
{"id":"a2","parent_id":"a1","timestamp":1731234569.0,"type":"message","message":{"role":"bashExecution","command":"ls","output":"file","exitCode":0,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/o","timestamp":333,"excludeFromContext":true}}
{"id":"a3","parent_id":"a2","timestamp":1731234570.0,"type":"message","message":{"role":"custom","customType":"note","content":[{"type":"text","text":"n"}],"display":false,"details":{"a":1},"timestamp":444}}
{"id":"a4","parent_id":"a3","timestamp":1731234571.0,"type":"message","message":{"role":"branchSummary","summary":"s","fromId":"a0","timestamp":555}}
{"id":"a5","parent_id":"a4","timestamp":1731234572.0,"type":"message","message":{"role":"compactionSummary","summary":"cs","tokensBefore":99,"timestamp":666}}
{"id":"a6","parent_id":"a5","timestamp":1731234573.0,"type":"branch_summary","summary":"bs","branch_root_id":"a0"}
{"id":"a7","parent_id":"a6","timestamp":1731234574.0,"type":"compaction","summary":"comp","replaces_entry_ids":[]}
{"id":"a8","parent_id":"a7","timestamp":1731234575.0,"type":"leaf"}"####;

    const JSONL_EXPECTED: &str = r####"{"id":"a0","parent_id":null,"timestamp":1731234567.0,"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm","thinkingSignature":"sig","redacted":true},{"type":"text","text":"hi","textSignature":"ts"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"R.md","nested":{"x":null}},"thoughtSignature":"th"}],"api":"anthropic","provider":"anthropic","model":"m","responseModel":"m2","responseId":"r1","diagnostics":[{"type":"retry","timestamp":123,"error":{"name":"E","message":"boom","stack":"st","code":429},"details":{"k":null}}],"usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"cacheWrite1H":5,"reasoning":6,"totalTokens":7,"cost":{"input":0.1,"output":0.2,"cacheRead":0.0,"cacheWrite":0.0,"total":0.3}},"stopReason":"toolUse","errorMessage":"err","timestamp":111}}
{"id":"a1","parent_id":"a0","timestamp":1731234568.0,"type":"message","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"out","textSignature":null},{"type":"image","data":"BASE64","mimeType":"image/png"}],"details":{"bytes":13,"z":null},"addedToolNames":["grep"],"isError":true,"timestamp":222}}
{"id":"a2","parent_id":"a1","timestamp":1731234569.0,"type":"message","message":{"role":"bashExecution","command":"ls","output":"file","exitCode":0,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/o","timestamp":333,"excludeFromContext":true}}
{"id":"a3","parent_id":"a2","timestamp":1731234570.0,"type":"message","message":{"role":"custom","customType":"note","content":[{"type":"text","text":"n","textSignature":null}],"display":false,"details":{"a":1},"timestamp":444}}
{"id":"a4","parent_id":"a3","timestamp":1731234571.0,"type":"message","message":{"role":"branchSummary","summary":"s","fromId":"a0","timestamp":555}}
{"id":"a5","parent_id":"a4","timestamp":1731234572.0,"type":"message","message":{"role":"compactionSummary","summary":"cs","tokensBefore":99,"timestamp":666}}
{"id":"a6","parent_id":"a5","timestamp":1731234573.0,"type":"branch_summary","summary":"bs","branch_root_id":"a0"}
{"id":"a7","parent_id":"a6","timestamp":1731234574.0,"type":"compaction","summary":"comp","replaces_entry_ids":[]}
{"id":"a8","parent_id":"a7","timestamp":1731234575.0,"type":"leaf","entry_id":null}
"####;

    const JSON_BLOCK_EXPECTED: &str = r####"<pre class="highlight"><span class="p">{</span>
<span class="w">  </span><span class="nt">&quot;b&quot;</span><span class="p">:</span><span class="w"> </span><span class="kc">true</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;big&quot;</span><span class="p">:</span><span class="w"> </span><span class="mf">100000.0</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;f&quot;</span><span class="p">:</span><span class="w"> </span><span class="mf">1.5</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;n&quot;</span><span class="p">:</span><span class="w"> </span><span class="mi">123</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;neg&quot;</span><span class="p">:</span><span class="w"> </span><span class="mi">-4</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;nested&quot;</span><span class="p">:</span><span class="w"> </span><span class="p">{</span>
<span class="w">    </span><span class="nt">&quot;k&quot;</span><span class="p">:</span><span class="w"> </span><span class="p">[</span>
<span class="w">      </span><span class="mi">1</span><span class="p">,</span>
<span class="w">      </span><span class="mi">2</span>
<span class="w">    </span><span class="p">]</span>
<span class="w">  </span><span class="p">},</span>
<span class="w">  </span><span class="nt">&quot;nl&quot;</span><span class="p">:</span><span class="w"> </span><span class="s2">&quot;x\ny\tz&quot;</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;q&quot;</span><span class="p">:</span><span class="w"> </span><span class="s2">&quot;a\&quot;b&quot;</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;unicode&quot;</span><span class="p">:</span><span class="w"> </span><span class="s2">&quot;\u4e16\u754c \ud83c\udf0d&quot;</span><span class="p">,</span>
<span class="w">  </span><span class="nt">&quot;z&quot;</span><span class="p">:</span><span class="w"> </span><span class="kc">null</span>
<span class="p">}</span>
</pre>"####;

    // block value (unicode kept raw; matches block_value in gen_testdata.py):
    // {"unicode":"世界 🌍","q":"a\"b","nl":"x\ny\tz","n":123,"f":1.5,"big":100000.0,"b":true,"z":null,"neg":-4,"nested":{"k":[1,2]}}
    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    fn load_entries(rel: &str) -> Vec<SessionEntry> {
        let text = std::fs::read_to_string(fixtures_dir().join(rel)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        entries_from_json_lines(&lines).unwrap()
    }

    fn args(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    fn message_entry(id: &str, parent: Option<&str>, message: AgentMessage) -> SessionEntry {
        let mut entry = MessageEntry::new(message);
        entry.id = id.to_string();
        entry.parent_id = parent.map(str::to_string);
        SessionEntry::Message(entry)
    }

    // -- oracle: byte-for-byte HTML against the golden --------------------

    #[test]
    fn kitchen_sink_html_matches_golden() {
        let entries = load_entries("sessions/kitchen-sink.jsonl");
        // Frozen "generated at" = 2024-01-01T00:00:00Z (as the extractor pinned).
        let html = render_html(
            &entries,
            "Kitchen Sink Session",
            Some("fixtures/sessions/kitchen-sink.jsonl"),
            1_704_067_200,
        );
        let golden =
            std::fs::read_to_string(fixtures_dir().join("export/kitchen-sink.html")).unwrap();
        assert_eq!(
            html, golden,
            "HTML export must match the golden byte-for-byte"
        );
    }

    #[test]
    fn frozen_instant_formats_like_the_golden() {
        assert_eq!(iso_utc(1_704_067_200), "2024-01-01T00:00:00+00:00");
    }

    // -- JSONL export writes nulls (not exclude_none) ---------------------

    #[test]
    fn kitchen_sink_jsonl_export_matches_tau() {
        let entries = load_entries("sessions/kitchen-sink.jsonl");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("k.jsonl");
        export_session_jsonl(&entries, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            KITCHEN_JSONL_EXPECTED
        );
    }

    #[test]
    fn jsonl_densify_covers_all_message_and_content_shapes() {
        let lines: Vec<&str> = JSONL_INPUT.lines().collect();
        let entries = entries_from_json_lines(&lines).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("c.jsonl");
        export_session_jsonl(&entries, &out).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), JSONL_EXPECTED);
    }

    #[test]
    fn jsonl_export_empty_is_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("e.jsonl");
        export_session_jsonl(&[], &out).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "");
    }

    // -- Pygments JSON highlight parity -----------------------------------

    #[test]
    fn json_block_highlight_matches_pygments() {
        let value = json!({
            "unicode": "世界 🌍",
            "q": "a\"b",
            "nl": "x\ny\tz",
            "n": 123,
            "f": 1.5,
            "big": 100_000.0,
            "b": true,
            "z": null,
            "neg": -4,
            "nested": {"k": [1, 2]},
        });
        assert_eq!(render_json_block(&value), JSON_BLOCK_EXPECTED);
    }

    // -- ported from tau/tests/test_session_export.py ---------------------

    #[test]
    fn render_session_html_preserves_branch_tree() {
        let entries = vec![
            message_entry(
                "root",
                None,
                AgentMessage::User(UserMessage::new("Start <session>")),
            ),
            message_entry(
                "left",
                Some("root"),
                AgentMessage::Assistant(AssistantMessage::new(vec![AssistantContent::Text(
                    TextContent::new("Left branch"),
                )])),
            ),
            message_entry(
                "right",
                Some("root"),
                AgentMessage::Assistant(AssistantMessage::new(vec![
                    AssistantContent::Text(TextContent::new("Right branch")),
                    AssistantContent::ToolCall(ToolCall::new(
                        "call-1",
                        "read",
                        args(&json!({"path": "README.md"})),
                    )),
                ])),
            ),
            message_entry(
                "tool",
                Some("right"),
                AgentMessage::ToolResult({
                    let mut m = ToolResultMessage::new(
                        "call-1",
                        "read",
                        vec![ToolResultContent::Text(TextContent::new("File contents"))],
                    );
                    m.details = Some(json!({"bytes": 13}));
                    m
                }),
            ),
            {
                let mut c = CompactionEntry::new(
                    "The right branch was compacted.",
                    vec!["root".to_string(), "right".to_string(), "tool".to_string()],
                );
                c.id = "compact".to_string();
                c.parent_id = Some("tool".to_string());
                SessionEntry::Compaction(c)
            },
            {
                let mut l = LeafEntry::new(Some("compact".to_string()));
                l.id = "leaf".to_string();
                l.parent_id = Some("compact".to_string());
                SessionEntry::Leaf(l)
            },
        ];

        let html = render_session_html(&entries, "Test Export", Some("/tmp/session.jsonl"));

        assert!(html.contains("<title>Test Export</title>"));
        assert!(html.contains("Source: <code>/tmp/session.jsonl</code>"));
        assert!(html.contains("id=\"entry-root\""));
        assert!(html.contains("id=\"entry-left\""));
        assert!(html.contains("id=\"entry-right\""));
        assert!(html.contains("id=\"entry-compact\""));
        assert!(html.contains("Start &lt;session&gt;"));
        assert!(html.contains("Right branch [read]"));
        assert!(html.contains("active-path"));
        assert!(html.contains("active-leaf"));
        assert!(html.contains("Replaces entries"));
    }

    #[test]
    fn render_session_html_uses_static_document_layout() {
        let entries = vec![message_entry(
            "root",
            None,
            AgentMessage::User(UserMessage::new("Export layout")),
        )];
        let html = render_session_html(&entries, "Layout Export", None);

        assert!(html.contains("<p class=\"eyebrow\">Tau session export</p>"));
        assert!(html.contains("<main class=\"session-shell\">"));
        assert!(html.contains("<aside class=\"tree-rail\">"));
        assert!(html.contains("<section class=\"entry-stream\" aria-label=\"Session entries\">"));
        assert!(html.contains("class=\"entry-card active-entry\""));
        assert!(html.contains("Session"));
        assert!(html.contains("Transcript"));
        assert!(html.contains("border-right: 1px solid var(--line);"));
        assert!(html.contains("id=\"themeToggle\""));
        assert!(!html.to_lowercase().contains("<link"));
        assert!(!html.contains("http://") && !html.contains("https://"));
    }

    #[test]
    fn render_session_html_syntax_highlights_tool_call_arguments() {
        let entries = vec![message_entry(
            "root",
            None,
            AgentMessage::Assistant(AssistantMessage::new(vec![
                AssistantContent::Text(TextContent::new("Reading a file")),
                AssistantContent::ToolCall(ToolCall::new(
                    "call-1",
                    "read",
                    args(&json!({"path": "README.md"})),
                )),
            ])),
        )];
        let html = render_session_html(&entries, "Highlight Export", None);
        assert!(html.contains("class=\"highlight\""));
        assert!(html.contains("<span class=\"nt\">") || html.contains("<span class=\"s2\">"));
    }

    #[test]
    fn render_session_html_includes_theme_toggle_script() {
        let entries = vec![message_entry(
            "root",
            None,
            AgentMessage::User(UserMessage::new("Hello")),
        )];
        let html = render_session_html(&entries, "Toggle Export", None);
        assert!(html.contains("id=\"themeToggle\""));
        assert!(html.contains("localStorage"));
        assert!(html.contains("data-theme"));
    }

    #[test]
    fn export_session_html_writes_file() {
        let entries = vec![message_entry(
            "root",
            None,
            AgentMessage::User(UserMessage::new("Hello")),
        )];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("session.html");
        let result = export_session_html(&entries, &out, "Session", None).unwrap();
        assert_eq!(result, out);
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .starts_with("<!doctype html>")
        );
    }
}
