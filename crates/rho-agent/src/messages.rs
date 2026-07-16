//! Pi-compatible content blocks and transcript message models.
//!
//! Port of tau's `tau_agent/messages.py` (`WireModel` and every message type).
//! The single hard requirement is **byte-identical** JSON to tau's Pydantic
//! output, so the serde idioms here are chosen to reproduce Pydantic's exact
//! wire shape rather than for idiomatic taste.
//!
//! ## Idiom map (tau Pydantic ⟶ rho serde)
//!
//! | tau `ConfigDict` option        | rho equivalent                                   |
//! |--------------------------------|--------------------------------------------------|
//! | `alias_generator=_to_camel`    | `#[serde(rename_all = "camelCase")]`             |
//! | `serialize_by_alias=True`      | (same attribute — serde renames both directions) |
//! | `extra="forbid"`               | `#[serde(deny_unknown_fields)]`                  |
//! | `exclude_none` (at dump time)  | `#[serde(skip_serializing_if = "Option::is_none")]` per optional field |
//! | discriminated union on `role`  | `#[serde(untagged)]` enum + `monostate::MustBe!` |
//!
//! ## Why untagged + monostate rather than `#[serde(tag = "role")]`
//!
//! serde's internally-tagged representation always emits the tag **first**. Most
//! message types put the discriminator first anyway, but session entries do not
//! (`id`/`parent_id`/`timestamp` precede `type`), and we want one uniform idiom
//! across the whole crate. An untagged enum serializes each variant struct in
//! declared field order — so the discriminator lands wherever we place it — and a
//! `monostate::MustBe!("user")` field makes that struct deserialize **only** when
//! the literal matches, giving exact discrimination without serde ever seeing an
//! internally-tagged enum. Variants are ordered most-frequent-first because
//! untagged deserialization tries them in order; correctness does not depend on
//! the order (monostate guarantees exactly one match), only speed.
//!
//! ## The `_to_camel` digit trap
//!
//! tau's `_to_camel` title-cases every non-first underscore segment, so
//! `cache_write_1h` becomes `cacheWrite1H` (capital `H`). serde's `camelCase`
//! only upper-cases the first *letter* of a segment, yielding `cacheWrite1h`.
//! Every multi-segment field was checked against the fixtures; `cache_write_1h`
//! is the only one serde gets wrong, so it carries an explicit `rename`.

use monostate::MustBe;
use serde::{Deserialize, Serialize};

use crate::types::{JsonMap, JsonValue};

/// Current Unix timestamp in milliseconds (tau's `current_timestamp_ms`).
///
/// Used only as a construction convenience; the wire format always carries an
/// explicit integer `timestamp`.
#[must_use]
pub fn current_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Deserialize shims reproducing tau's `mode="before"` validators
// ---------------------------------------------------------------------------
//
// tau's `WireModel` subclasses attach `@model_validator(mode="before")` methods
// that run on **every** deserialization (not just Python constructors). Two of
// them shape the wire we must accept:
//
//   * `AssistantMessage._normalize_convenient_content`: `usage is None → Usage()`
//     and string `content → blocks`.
//   * `ToolResultMessage` / `AgentToolResult`: string `content → blocks`.
//
// serde's plain `#[serde(default)]` only fires on an **absent** key, so a present
// `"usage": null` would fail the parse. These shims restore tau's behavior:
// accepting the convenience shapes on the wire, so parity is behavioral, not just
// byte-level on already-canonical output.

/// Deserialize `T`, mapping a JSON `null` (or absent, via `default`) to
/// `T::default()`. Mirrors tau's `usage is None → Usage()`.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A content-block type that a bare text string can normalize into. Lets one
/// deserializer serve every message whose `content` is a block list.
pub(crate) trait FromText {
    /// Wrap a text block into this content variant.
    fn from_text(text: TextContent) -> Self;
}

impl FromText for AssistantContent {
    fn from_text(text: TextContent) -> Self {
        Self::Text(text)
    }
}

impl FromText for ToolResultContent {
    fn from_text(text: TextContent) -> Self {
        Self::Text(text)
    }
}

/// Deserialize a content field that tau accepts as either a bare string or a
/// list of blocks. A non-empty string becomes a single text block; an empty
/// string becomes `[]` (tau: `[TextContent(text=content)] if content else []`).
pub(crate) fn string_or_blocks<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromText,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrBlocks<T> {
        Str(String),
        Blocks(Vec<T>),
    }
    Ok(match StrOrBlocks::<T>::deserialize(deserializer)? {
        StrOrBlocks::Str(s) if s.is_empty() => Vec::new(),
        StrOrBlocks::Str(s) => vec![T::from_text(TextContent::new(s))],
        StrOrBlocks::Blocks(v) => v,
    })
}

// ---------------------------------------------------------------------------
// Usage / cost
// ---------------------------------------------------------------------------

/// Billed response cost in USD (tau `UsageCost`). All fields are floats and are
/// always serialized (e.g. `0.0`), never omitted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageCost {
    /// Cost of input (prompt) tokens.
    #[serde(default)]
    pub input: f64,
    /// Cost of output (completion) tokens.
    #[serde(default)]
    pub output: f64,
    /// Cost of cache-read tokens.
    #[serde(default)]
    pub cache_read: f64,
    /// Cost of cache-write tokens.
    #[serde(default)]
    pub cache_write: f64,
    /// Total billed cost.
    #[serde(default)]
    pub total: f64,
}

/// Provider-reported token usage for one assistant response (tau `Usage`).
///
/// Token counts are integers; `cache_write_1h` and `reasoning` are optional and
/// omitted when absent. Note the explicit rename for the digit-segment trap.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    /// Input (prompt) token count.
    #[serde(default)]
    pub input: i64,
    /// Output (completion) token count.
    #[serde(default)]
    pub output: i64,
    /// Cache-read token count.
    #[serde(default)]
    pub cache_read: i64,
    /// Cache-write token count.
    #[serde(default)]
    pub cache_write: i64,
    /// 1-hour cache-write token count.
    ///
    /// `_to_camel("cache_write_1h") == "cacheWrite1H"` (capital H); serde's
    /// camelCase would emit `cacheWrite1h`, so the alias is pinned explicitly.
    #[serde(
        rename = "cacheWrite1H",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write_1h: Option<i64>,
    /// Reasoning token count, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<i64>,
    /// Total token count.
    #[serde(default)]
    pub total_tokens: i64,
    /// Billed cost breakdown.
    #[serde(default)]
    pub cost: UsageCost,
}

// ---------------------------------------------------------------------------
// Content blocks
// ---------------------------------------------------------------------------

/// A text content block (tau `TextContent`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextContent {
    #[serde(rename = "type")]
    kind: MustBe!("text"),
    /// The text.
    pub text: String,
    /// Optional provider signature over the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

impl TextContent {
    /// Build a plain text block.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            kind: MustBe!("text"),
            text: text.into(),
            text_signature: None,
        }
    }
}

/// A thinking / reasoning content block (tau `ThinkingContent`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThinkingContent {
    #[serde(rename = "type")]
    kind: MustBe!("thinking"),
    /// The reasoning text.
    pub thinking: String,
    /// Optional provider signature over the reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// Whether the reasoning was redacted by the provider.
    ///
    /// Defaults to `false` but is always serialized (tau does not wrap it in
    /// `Optional`), so it is a plain `bool`, not skipped.
    #[serde(default)]
    pub redacted: bool,
}

/// An image content block (tau `ImageContent`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageContent {
    #[serde(rename = "type")]
    kind: MustBe!("image"),
    /// Base64-encoded image bytes.
    pub data: String,
    /// The image MIME type (e.g. `image/png`).
    pub mime_type: String,
}

/// A tool call requested by the assistant (tau `ToolCall`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCall {
    #[serde(rename = "type")]
    kind: MustBe!("toolCall"),
    /// Provider-assigned call id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Call arguments.
    ///
    /// Free-form JSON object; always serialized (even `{}`) and preserves nested
    /// literal nulls / key order — hence `JsonMap`, not skipped.
    #[serde(default)]
    pub arguments: JsonMap,
    /// Optional provider signature over the tool "thought".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// User-authored content: either a bare string or a list of text/image blocks
/// (tau `UserContent = str | list[TextContent | ImageContent]`).
///
/// The string form is preserved on round-trip. This is genuinely true for
/// [`UserMessage`] and [`CustomMessage`], which carry **no** `mode="before"`
/// validator — a stored string stays a string. (Contrast [`AssistantMessage`]
/// and [`ToolResultMessage`], whose validators *do* normalize a string into
/// blocks on every deserialization; see their `content` fields.) An untagged
/// enum keeps both shapes; `Text` is tried first as the common case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// A plain string message.
    Text(String),
    /// An ordered list of text/image blocks.
    Blocks(Vec<UserContentBlock>),
}

impl Default for UserContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

/// A block permitted inside [`UserContent::Blocks`] and tool results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    /// Text block.
    Text(TextContent),
    /// Image block.
    Image(ImageContent),
}

/// A block inside an [`AssistantMessage`] (tau `AssistantContent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContent {
    /// Text block.
    Text(TextContent),
    /// Thinking block.
    Thinking(ThinkingContent),
    /// Tool call block.
    ToolCall(ToolCall),
}

/// A block inside a [`ToolResultMessage`] (tau `ToolResultContent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Text block.
    Text(TextContent),
    /// Image block.
    Image(ImageContent),
}

// ---------------------------------------------------------------------------
// Stop reason
// ---------------------------------------------------------------------------

/// Why the assistant stopped (tau `StopReason`). `camelCase` reproduces every
/// literal, including `toolUse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// Natural stop.
    #[default]
    Stop,
    /// Hit the max token length.
    Length,
    /// Stopped to run tools.
    ToolUse,
    /// Errored.
    Error,
    /// Aborted by the user.
    Aborted,
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Structured error attached to an assistant diagnostic (tau
/// `AssistantDiagnosticError`). `code` is `str | int | None`, modeled as an
/// optional free-form JSON value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantDiagnosticError {
    /// Error class name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Error message.
    pub message: String,
    /// Optional stack trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// Optional error code (string or integer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<JsonValue>,
}

/// A per-response diagnostic record (tau `AssistantMessageDiagnostic`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantMessageDiagnostic {
    /// Diagnostic kind (free-form, e.g. `retry`); serialized as `type`.
    #[serde(rename = "type")]
    pub diagnostic_type: String,
    /// Diagnostic timestamp (Unix ms).
    pub timestamp: i64,
    /// Optional structured error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AssistantDiagnosticError>,
    /// Optional free-form details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonMap>,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// A user message (tau `UserMessage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserMessage {
    role: MustBe!("user"),
    /// Message content (string or blocks).
    pub content: UserContent,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

/// An assistant message with ordered content blocks (tau `AssistantMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantMessage {
    role: MustBe!("assistant"),
    /// Ordered content blocks (text / thinking / tool calls). Always serialized.
    ///
    /// Accepts tau's convenience shape on input: a bare string normalizes to a
    /// single text block (empty string → `[]`).
    #[serde(default, deserialize_with = "string_or_blocks")]
    pub content: Vec<AssistantContent>,
    /// Provider API family (defaults to `unknown`).
    #[serde(default = "unknown")]
    pub api: String,
    /// Provider id (defaults to `unknown`).
    #[serde(default = "unknown")]
    pub provider: String,
    /// Requested model id (defaults to `unknown`).
    #[serde(default = "unknown")]
    pub model: String,
    /// Model id the provider actually responded with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Provider response id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Optional per-response diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    /// Token usage / cost.
    ///
    /// Accepts `null` on the wire (tau's validator maps `usage is None` to the
    /// default `Usage()`), not just an absent key.
    #[serde(default, deserialize_with = "null_to_default")]
    pub usage: Usage,
    /// Why generation stopped.
    #[serde(default)]
    pub stop_reason: StopReason,
    /// Human-readable error, when `stop_reason` is `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

fn unknown() -> String {
    "unknown".to_string()
}

/// A tool result message (tau `ToolResultMessage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResultMessage {
    role: MustBe!("toolResult"),
    /// The originating tool call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// Result content blocks. Always serialized (even `[]`).
    ///
    /// Accepts tau's convenience shape on input: a bare string normalizes to a
    /// single text block (empty string → `[]`).
    #[serde(default, deserialize_with = "string_or_blocks")]
    pub content: Vec<ToolResultContent>,
    /// Free-form details.
    ///
    /// Omitted entirely when null/None (top-level `exclude_none`), but an object
    /// value preserves its inner nulls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Tools this result dynamically added to the toolset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Whether the tool failed.
    #[serde(default)]
    pub is_error: bool,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

/// A recorded shell execution (tau `BashExecutionMessage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BashExecutionMessage {
    role: MustBe!("bashExecution"),
    /// The command that ran.
    pub command: String,
    /// Captured output.
    pub output: String,
    /// Process exit code, when the process completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Whether the command was cancelled.
    #[serde(default)]
    pub cancelled: bool,
    /// Whether the output was truncated.
    #[serde(default)]
    pub truncated: bool,
    /// Path to the full (untruncated) output, when spilled to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
    /// Whether to exclude this execution from model context.
    #[serde(default)]
    pub exclude_from_context: bool,
}

/// An extension/application-owned message (tau `CustomMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomMessage {
    role: MustBe!("custom"),
    /// The application-defined message subtype.
    pub custom_type: String,
    /// Message content (string or blocks).
    pub content: UserContent,
    /// Whether the frontend should display this message (defaults to `true`).
    #[serde(default = "default_true")]
    pub display: bool,
    /// Free-form details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

fn default_true() -> bool {
    true
}

/// A branch summary message (tau `BranchSummaryMessage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchSummaryMessage {
    role: MustBe!("branchSummary"),
    /// The summary text.
    pub summary: String,
    /// Entry id the summarized branch started from.
    pub from_id: String,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

/// A compaction summary message (tau `CompactionSummaryMessage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionSummaryMessage {
    role: MustBe!("compactionSummary"),
    /// The summary text.
    pub summary: String,
    /// Token count before compaction.
    pub tokens_before: i64,
    /// Timestamp (Unix ms).
    pub timestamp: i64,
}

/// The transcript message union (tau `AgentMessage`, discriminated on `role`).
///
/// Variants are ordered most-frequent-first (untagged tries them in order);
/// `monostate` on each `role` field guarantees exactly one variant matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    /// User turn.
    User(UserMessage),
    /// Assistant turn.
    Assistant(AssistantMessage),
    /// Tool result.
    ToolResult(ToolResultMessage),
    /// Recorded shell execution.
    BashExecution(BashExecutionMessage),
    /// Extension/application-owned message.
    Custom(CustomMessage),
    /// Branch summary.
    BranchSummary(BranchSummaryMessage),
    /// Compaction summary.
    CompactionSummary(CompactionSummaryMessage),
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------
//
// The wire structs keep their `role`/`type` discriminator fields private (only a
// `monostate::MustBe!` value is ever valid), so callers outside the crate cannot
// build them with a struct literal. These constructors mirror tau's keyword
// constructors: required fields are positional, optional fields fall back to the
// same defaults tau uses, and `timestamp` is injected from `current_timestamp_ms`
// (matching tau's `default_factory`). Optional fields can be adjusted afterwards
// (they are `pub`) or via `..Default::default()`.

impl From<&str> for UserContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl From<String> for UserContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<Vec<UserContentBlock>> for UserContent {
    fn from(blocks: Vec<UserContentBlock>) -> Self {
        Self::Blocks(blocks)
    }
}

impl UserMessage {
    /// Build a user message with the current timestamp.
    pub fn new(content: impl Into<UserContent>) -> Self {
        Self {
            role: MustBe!("user"),
            content: content.into(),
            timestamp: current_timestamp_ms(),
        }
    }
}

impl AssistantMessage {
    /// Build an assistant message from content blocks, with tau's defaults
    /// (`api`/`provider`/`model` = `"unknown"`, `stop_reason` = `stop`) and the
    /// current timestamp.
    pub fn new(content: Vec<AssistantContent>) -> Self {
        Self {
            role: MustBe!("assistant"),
            content,
            api: unknown(),
            provider: unknown(),
            model: unknown(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: current_timestamp_ms(),
        }
    }

    // Fluent builders for the optional fields. External callers cannot use
    // struct-update syntax (the `role` discriminator is private), so these are
    // the ergonomic way to set optionals; in-crate code may still mutate the
    // `pub` fields directly or use `..Default::default()`.

    /// Set the requested model id.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set the provider API family.
    #[must_use]
    pub fn with_api(mut self, api: impl Into<String>) -> Self {
        self.api = api.into();
        self
    }

    /// Set the provider id.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Set token usage / cost.
    #[must_use]
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    /// Set the stop reason.
    #[must_use]
    pub fn with_stop_reason(mut self, stop_reason: StopReason) -> Self {
        self.stop_reason = stop_reason;
        self
    }

    /// Set the error message (and does not itself change `stop_reason`).
    #[must_use]
    pub fn with_error_message(mut self, error_message: impl Into<String>) -> Self {
        self.error_message = Some(error_message.into());
        self
    }

    /// Set the provider response id.
    #[must_use]
    pub fn with_response_id(mut self, response_id: impl Into<String>) -> Self {
        self.response_id = Some(response_id.into());
        self
    }
}

impl Default for AssistantMessage {
    // Spelled out (not `#[derive(Default)]`) because tau's defaults are *not* the
    // field-type defaults: `api`/`provider`/`model` are `"unknown"`, not `""`.
    fn default() -> Self {
        Self {
            role: MustBe!("assistant"),
            content: Vec::new(),
            api: unknown(),
            provider: unknown(),
            model: unknown(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }
}

impl ToolResultMessage {
    /// Build a tool-result message with the current timestamp.
    pub fn new(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: Vec<ToolResultContent>,
    ) -> Self {
        Self {
            role: MustBe!("toolResult"),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content,
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: current_timestamp_ms(),
        }
    }
}

impl BashExecutionMessage {
    /// Build a bash-execution message with the current timestamp.
    pub fn new(command: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            role: MustBe!("bashExecution"),
            command: command.into(),
            output: output.into(),
            exit_code: None,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: current_timestamp_ms(),
            exclude_from_context: false,
        }
    }
}

impl CustomMessage {
    /// Build a custom message with the current timestamp (`display` defaults to
    /// `true`, matching tau).
    pub fn new(custom_type: impl Into<String>, content: impl Into<UserContent>) -> Self {
        Self {
            role: MustBe!("custom"),
            custom_type: custom_type.into(),
            content: content.into(),
            display: true,
            details: None,
            timestamp: current_timestamp_ms(),
        }
    }
}

impl Default for CustomMessage {
    // Spelled out because tau's `display` default is `true`, not `bool::default()`.
    fn default() -> Self {
        Self {
            role: MustBe!("custom"),
            custom_type: String::new(),
            content: UserContent::default(),
            display: true,
            details: None,
            timestamp: 0,
        }
    }
}

impl BranchSummaryMessage {
    /// Build a branch-summary message with the current timestamp.
    pub fn new(summary: impl Into<String>, from_id: impl Into<String>) -> Self {
        Self {
            role: MustBe!("branchSummary"),
            summary: summary.into(),
            from_id: from_id.into(),
            timestamp: current_timestamp_ms(),
        }
    }
}

impl CompactionSummaryMessage {
    /// Build a compaction-summary message with the current timestamp.
    pub fn new(summary: impl Into<String>, tokens_before: i64) -> Self {
        Self {
            role: MustBe!("compactionSummary"),
            summary: summary.into(),
            tokens_before,
            timestamp: current_timestamp_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from tau's `tests/test_agent_types.py` (the wire-shape and
    //! union-discrimination cases). Skips are noted in `dev-notes/phase-1.md`
    //! (e.g. the `AgentTool` executor test — behavior, not wire format, lands in
    //! M2; and the non-`by_alias` `model_dump` shape, which rho does not model
    //! because only the exclude-none wire path exists here). Note the convenience
    //! normalizations (string `content → blocks`, `usage: null → default`) are
    //! `mode="before"` validators that run on **every** deserialization, so rho
    //! reproduces them on the wire — see `convenience_shapes_normalize_on_parse`.
    use super::*;

    fn args(json: serde_json::Value) -> JsonMap {
        match json {
            serde_json::Value::Object(map) => map,
            _ => JsonMap::new(),
        }
    }

    #[test]
    fn user_message_serializes_with_pi_wire_shape() {
        let m = UserMessage {
            role: MustBe!("user"),
            content: UserContent::Text("hello".into()),
            timestamp: 123,
        };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"role":"user","content":"hello","timestamp":123}"#
        );
    }

    #[test]
    fn assistant_message_keeps_ordered_content_blocks() {
        let tc = ToolCall {
            kind: MustBe!("toolCall"),
            id: "call-1".into(),
            name: "read".into(),
            arguments: args(serde_json::json!({"path": "README.md"})),
            thought_signature: None,
        };
        let m = AssistantMessage {
            role: MustBe!("assistant"),
            content: vec![
                AssistantContent::Text(TextContent::new("I'll read that.")),
                AssistantContent::ToolCall(tc),
            ],
            api: "unknown".into(),
            provider: "unknown".into(),
            model: "fake".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 123,
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        // The second block is the tool call, in wire (exclude-none) shape — note
        // that unlike tau's non-alias `model_dump`, `thoughtSignature` is omitted.
        assert_eq!(
            value["content"][1],
            serde_json::json!({
                "type": "toolCall",
                "id": "call-1",
                "name": "read",
                "arguments": {"path": "README.md"},
            })
        );
    }

    #[test]
    fn tool_result_message_records_canonical_output() {
        let m = ToolResultMessage {
            role: MustBe!("toolResult"),
            tool_call_id: "call-1".into(),
            tool_name: "read".into(),
            content: vec![ToolResultContent::Text(TextContent::new("file contents"))],
            details: Some(serde_json::json!({"bytes": 13})),
            added_tool_names: None,
            is_error: false,
            timestamp: 123,
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(value["toolCallId"], "call-1");
        assert_eq!(value["details"]["bytes"], 13);
    }

    #[test]
    fn union_discriminates_every_role() {
        let cases = [
            (r#"{"role":"user","content":"x","timestamp":1}"#, "User"),
            (
                r#"{"role":"toolResult","toolCallId":"c","toolName":"t","content":[],"isError":false,"timestamp":1}"#,
                "ToolResult",
            ),
            (
                r#"{"role":"bashExecution","command":"ls","output":"","cancelled":false,"truncated":false,"timestamp":1,"excludeFromContext":false}"#,
                "BashExecution",
            ),
            (
                r#"{"role":"branchSummary","summary":"s","fromId":"e1","timestamp":1}"#,
                "BranchSummary",
            ),
        ];
        for (json, want) in cases {
            let m: AgentMessage = serde_json::from_str(json).unwrap();
            let got = match m {
                AgentMessage::User(_) => "User",
                AgentMessage::Assistant(_) => "Assistant",
                AgentMessage::ToolResult(_) => "ToolResult",
                AgentMessage::BashExecution(_) => "BashExecution",
                AgentMessage::Custom(_) => "Custom",
                AgentMessage::BranchSummary(_) => "BranchSummary",
                AgentMessage::CompactionSummary(_) => "CompactionSummary",
            };
            assert_eq!(got, want, "for {json}");
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // tau's `extra="forbid"` ⟶ serde `deny_unknown_fields`. Verify it holds
        // both on a leaf struct and through the untagged union.
        let bad = r#"{"role":"user","content":"hello","timestamp":1,"unexpected":true}"#;
        assert!(serde_json::from_str::<UserMessage>(bad).is_err());
        assert!(serde_json::from_str::<AgentMessage>(bad).is_err());
    }

    #[test]
    fn string_user_content_round_trips_as_string() {
        // A user string is preserved: UserMessage has no `mode="before"`
        // validator, so — unlike assistant/toolResult — the string is not
        // normalized into blocks, on the wire or otherwise.
        let json = r#"{"role":"user","content":"plain","timestamp":1}"#;
        let m: AgentMessage = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&m).unwrap(), json);
    }

    #[test]
    fn convenience_shapes_normalize_on_parse() {
        // tau's `mode="before"` validators run on every deserialization, so rho
        // accepts these convenience shapes on the wire (not just in constructors).

        // Assistant: string content → one text block; `usage: null` → default.
        let a: AgentMessage = serde_json::from_str(
            r#"{"role":"assistant","content":"hi","usage":null,"model":"m","timestamp":1}"#,
        )
        .unwrap();
        let AgentMessage::Assistant(a) = a else {
            panic!("expected assistant")
        };
        assert_eq!(
            a.content,
            vec![AssistantContent::Text(TextContent::new("hi"))]
        );
        assert_eq!(a.usage, Usage::default());

        // Assistant: empty string content → no blocks.
        let empty: AgentMessage =
            serde_json::from_str(r#"{"role":"assistant","content":"","model":"m","timestamp":1}"#)
                .unwrap();
        let AgentMessage::Assistant(empty) = empty else {
            panic!("expected assistant")
        };
        assert!(empty.content.is_empty());

        // ToolResult: string content → one text block.
        let t: AgentMessage = serde_json::from_str(
            r#"{"role":"toolResult","toolCallId":"c","toolName":"t","content":"out","timestamp":1}"#,
        )
        .unwrap();
        let AgentMessage::ToolResult(t) = t else {
            panic!("expected toolResult")
        };
        assert_eq!(
            t.content,
            vec![ToolResultContent::Text(TextContent::new("out"))]
        );
    }
}
