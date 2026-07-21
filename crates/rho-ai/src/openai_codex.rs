//! `OpenAI` Codex subscription Responses provider (tau `tau_ai/openai_codex.py`).
//!
//! ChatGPT-subscription Codex speaks the Responses API over SSE behind an OAuth
//! bearer token + account id, resolved per request by
//! [`OpenAICodexCredentialResolver`]. The SSE framing differs from the plain
//! Responses adapter: events are multi-line `data:` blocks separated by blank
//! lines, and tool calls are correlated across `item_id` / `call_id` /
//! `output_index`. See `dev-notes/phase-3.md` for the OAuth design + the manual
//! live-flow checklist.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::messages::{AgentMessage, ToolCall, Usage};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::tools::AgentTool;
use serde_json::{Map, Value, json};

use crate::engine::{Feed, FetchError, ProviderParser, RetryPolicy, provider_stream, send_reqwest};
use crate::env::{OpenAICodexConfig, OpenAICodexCredentials};
use crate::model_limits::RuntimeModelLimits;
use crate::openai_compatible::parse_arguments;
use crate::retry::is_transient_status;
use crate::stream::{Delta, assistant_content, assistant_message};
use crate::util::{int_or_zero, loads_object};
use crate::wire::python_dumps;

/// Provider adapter for `ChatGPT` subscription Codex Responses over SSE.
#[derive(Clone)]
pub struct OpenAICodexProvider {
    config: Arc<OpenAICodexConfig>,
    client: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl OpenAICodexProvider {
    /// Build a provider with a fresh HTTP client and the system clock.
    #[must_use]
    pub fn new(config: OpenAICodexConfig) -> Self {
        let client = crate::http::create_client(config.timeout_seconds);
        Self {
            config: Arc::new(config),
            client,
            clock: system_clock(),
        }
    }

    /// Override the HTTP client (e.g. the mock provider).
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Override the clock (fixture reproduction / tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_retries: self.config.max_retries,
            max_retry_delay_seconds: self.config.max_retry_delay_seconds,
        }
    }

    /// Discover live limits for `model` from the authenticated Codex model
    /// catalog (tau `OpenAICodexProvider.discover_model_limits`). Returns `None`
    /// when the model is not advertised; propagates the fetch error otherwise so
    /// the caller can fall back to the static catalog.
    ///
    /// Unlike tau this does not memoize the fetched catalog on the provider (tau
    /// caches `_discovered_model_limits`); rho's provider is shared behind an
    /// `Arc` without interior mutability, and the caching is not observable in
    /// the wire/session format. The session is expected to cache the resolved
    /// `RuntimeModelLimits` per (provider, model) instead.
    pub async fn discover_model_limits(
        &self,
        model: &str,
    ) -> Result<Option<RuntimeModelLimits>, FetchError> {
        let limits = self.fetch_model_limits().await?;
        Ok(limits.get(model).cloned())
    }

    /// GET the authenticated Codex model catalog and parse per-model limits (tau
    /// `_fetch_model_limits`).
    async fn fetch_model_limits(&self) -> Result<HashMap<String, RuntimeModelLimits>, FetchError> {
        let credentials: OpenAICodexCredentials = (self.config.credential_resolver)()
            .await
            .map_err(|message| FetchError {
                message,
                retryable: false,
            })?;
        let mut headers = build_codex_headers(
            self.config.headers.as_ref(),
            &credentials.access_token,
            &credentials.account_id,
            &self.config.originator,
        );
        // tau: `headers["accept"] = "application/json"` then drop content-type.
        headers.retain(|(name, _)| name != "content-type");
        if let Some(entry) = headers.iter_mut().find(|(name, _)| name == "accept") {
            entry.1 = "application/json".to_string();
        } else {
            headers.push(("accept".to_string(), "application/json".to_string()));
        }

        let url = resolve_codex_models_url(&self.config.base_url);
        let mut request = self
            .client
            .get(&url)
            .query(&[("client_version", self.config.client_version.as_str())])
            .timeout(Duration::from_secs_f64(
                self.config.model_catalog_timeout_seconds,
            ));
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|err| FetchError {
                message: err.to_string(),
                retryable: false,
            })?
            .error_for_status()
            .map_err(|err| FetchError {
                message: err.to_string(),
                retryable: false,
            })?;
        let payload: Value = response.json().await.map_err(|err| FetchError {
            message: err.to_string(),
            retryable: false,
        })?;
        Ok(parse_codex_model_limits(&payload))
    }
}

impl ModelProvider for OpenAICodexProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        let payload = build_codex_payload(&self.config, model, system, messages, tools);
        let url = resolve_codex_url(&self.config.base_url);
        let config = self.config.clone();
        let client = self.client.clone();

        let fetch = move |_attempt: u32| {
            let config = config.clone();
            let client = client.clone();
            let payload = payload.clone();
            let url = url.clone();
            async move {
                let credentials: OpenAICodexCredentials = (config.credential_resolver)()
                    .await
                    .map_err(|message| FetchError {
                        message,
                        retryable: false,
                    })?;
                let headers = build_codex_headers(
                    config.headers.as_ref(),
                    &credentials.access_token,
                    &credentials.account_id,
                    &config.originator,
                );
                send_reqwest(&client, &url, &headers, &payload).await
            }
        };

        provider_stream(
            "openai-codex-responses",
            "openai-codex",
            self.config.provider_name.clone(),
            model,
            &self.clock,
            self.policy(),
            signal,
            fetch,
            CodexParser::new,
            is_retryable_status,
        )
    }
}

// ---------------------------------------------------------------------------
// Request payload + headers
// ---------------------------------------------------------------------------

/// Build the Codex Responses request body (tau `_build_codex_payload`).
#[must_use]
pub fn build_codex_payload(
    config: &OpenAICodexConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let mut payload = Map::new();
    payload.insert("model".into(), json!(model));
    payload.insert("store".into(), json!(false));
    payload.insert("stream".into(), json!(true));
    payload.insert(
        "instructions".into(),
        json!(if system.is_empty() {
            "You are a helpful assistant."
        } else {
            system
        }),
    );
    payload.insert(
        "input".into(),
        Value::Array(messages_to_responses_input(messages)),
    );
    payload.insert("text".into(), json!({"verbosity": "low"}));
    payload.insert("include".into(), json!(["reasoning.encrypted_content"]));
    payload.insert("tool_choice".into(), json!("auto"));
    payload.insert("parallel_tool_calls".into(), json!(true));
    if let Some(effort) = &config.reasoning_effort {
        payload.insert(
            "reasoning".into(),
            json!({"effort": effort, "summary": config.reasoning_summary}),
        );
    }
    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(tools.iter().map(tool_to_codex).collect()),
        );
    }
    Value::Object(payload)
}

fn messages_to_responses_input(messages: &[AgentMessage]) -> Vec<Value> {
    let mut items = Vec::new();
    let mut assistant_index = 0;
    for message in messages {
        match message {
            AgentMessage::User(m) => items.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": m.text()}],
            })),
            AgentMessage::Assistant(m) => {
                if !m.text().is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": m.text(), "annotations": []}],
                        "status": "completed",
                        "id": format!("msg_{assistant_index}"),
                    }));
                    assistant_index += 1;
                }
                for tool_call in m.tool_calls() {
                    let (call_id, item_id) = split_tool_call_id(&tool_call.id);
                    let mut item = Map::new();
                    item.insert("type".into(), json!("function_call"));
                    item.insert("call_id".into(), json!(call_id));
                    item.insert("name".into(), json!(tool_call.name));
                    item.insert(
                        "arguments".into(),
                        json!(python_dumps(&Value::Object(tool_call.arguments.clone()))),
                    );
                    if let Some(item_id) = item_id {
                        item.insert("id".into(), json!(item_id));
                    }
                    items.push(Value::Object(item));
                }
            }
            AgentMessage::ToolResult(m) => {
                let (call_id, _item_id) = split_tool_call_id(&m.tool_call_id);
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": m.text(),
                }));
            }
            _ => {}
        }
    }
    items
}

fn tool_to_codex(tool: &AgentTool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": Value::Object(tool.input_schema().clone()),
        "strict": Value::Null,
    })
}

/// The Codex `User-Agent` header, reproducing tau's
/// `f"tau ({system()} {release()}; {machine()})"` where `system`/`release`/
/// `machine` are Python's `platform` values (`Darwin`/`Linux`, the kernel
/// release, `arm64`/`x86_64`). Rust's `std::env::consts` gives the wrong tokens
/// (`macos`/`unix`/`aarch64`), and `libc::uname` needs `unsafe` (forbidden), so
/// we shell out to `uname` and cache the result process-wide.
fn codex_user_agent() -> &'static str {
    use std::sync::OnceLock;
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        // `platform.system()` == `uname -s`, `.release()` == `uname -r`,
        // `.machine()` == `uname -m` on every POSIX platform Codex runs on.
        let field = |flag: &str, fallback: &str| -> String {
            std::process::Command::new("uname")
                .arg(flag)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback.to_string())
        };
        // Fallbacks map Rust's consts onto Python's platform vocabulary in case
        // `uname` is unavailable (e.g. Windows), so the header stays well-formed.
        let system = field(
            "-s",
            match std::env::consts::OS {
                "macos" => "Darwin",
                "linux" => "Linux",
                other => other,
            },
        );
        let release = field("-r", "");
        let machine = field(
            "-m",
            match std::env::consts::ARCH {
                "aarch64" => "arm64",
                other => other,
            },
        );
        format!("tau ({system} {release}; {machine})")
    })
}

fn build_codex_headers(
    configured: Option<&crate::types::HeaderList>,
    access_token: &str,
    account_id: &str,
    originator: &str,
) -> crate::types::HeaderList {
    let mut headers: crate::types::HeaderList = configured.cloned().unwrap_or_default();
    headers.push((
        "Authorization".to_string(),
        format!("Bearer {access_token}"),
    ));
    headers.push(("chatgpt-account-id".to_string(), account_id.to_string()));
    headers.push(("originator".to_string(), originator.to_string()));
    headers.push(("User-Agent".to_string(), codex_user_agent().to_string()));
    headers.push((
        "OpenAI-Beta".to_string(),
        "responses=experimental".to_string(),
    ));
    headers.push(("accept".to_string(), "text/event-stream".to_string()));
    headers.push(("content-type".to_string(), "application/json".to_string()));
    headers
}

fn resolve_codex_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

/// Resolve the authenticated model-catalog URL from the responses base URL (tau
/// `_resolve_codex_models_url`).
fn resolve_codex_models_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        let prefix = normalized
            .strip_suffix("/responses")
            .expect("suffix checked above");
        format!("{prefix}/models")
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/models")
    } else {
        format!("{normalized}/codex/models")
    }
}

/// Parse the Codex model-catalog payload into per-model runtime limits (tau
/// `_parse_codex_model_limits`). Malformed or non-positive entries are skipped.
fn parse_codex_model_limits(payload: &Value) -> HashMap<String, RuntimeModelLimits> {
    let mut parsed = HashMap::new();
    let Some(models) = payload.get("models").and_then(Value::as_array) else {
        return parsed;
    };
    for item in models {
        let Some(item) = item.as_object() else {
            continue;
        };
        let Some(model) = item.get("slug").and_then(Value::as_str) else {
            continue;
        };
        if model.is_empty() {
            continue;
        }
        let Some(context_window) = positive_int(item.get("context_window"))
            .or_else(|| positive_int(item.get("max_context_window")))
        else {
            continue;
        };
        let effective_percent =
            positive_int(item.get("effective_context_window_percent")).unwrap_or(100);
        if effective_percent > 100 {
            continue;
        }
        let Ok(limits) = RuntimeModelLimits::new(
            context_window,
            positive_int(item.get("max_output_tokens")),
            effective_percent,
            positive_int(item.get("auto_compact_token_limit")),
        ) else {
            continue;
        };
        parsed.insert(model.to_string(), limits);
    }
    parsed
}

/// Return a strictly-positive JSON integer, else `None` (tau `_positive_int`).
/// Booleans and floats — including whole-number floats — are rejected, matching
/// tau's `isinstance(value, int) and not isinstance(value, bool)`.
fn positive_int(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    value.as_i64().filter(|&n| n > 0)
}

fn split_tool_call_id(value: &str) -> (String, Option<String>) {
    match value.split_once('|') {
        None => (value.to_string(), None),
        Some((call_id, item_id)) => (
            call_id.to_string(),
            if item_id.is_empty() {
                None
            } else {
                Some(item_id.to_string())
            },
        ),
    }
}

fn is_retryable_status(status_code: u16, body: &str) -> bool {
    if status_code == 429 && is_terminal_rate_limit(body) {
        return false;
    }
    is_transient_status(status_code)
}

fn is_terminal_rate_limit(body: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ];
    let normalized = body.to_lowercase();
    MARKERS.iter().any(|marker| normalized.contains(marker))
}

// ---------------------------------------------------------------------------
// SSE parser (multi-line data blocks + tool-call correlation)
// ---------------------------------------------------------------------------

struct CodexToolBuilder {
    call_id: String,
    item_id: Option<String>,
    name: String,
    arguments_parts: Vec<String>,
}

impl CodexToolBuilder {
    fn from_item(item: &Map<String, Value>) -> Self {
        Self {
            call_id: non_empty_str(item.get("call_id")).unwrap_or_else(|| "call_0".to_string()),
            item_id: non_empty_str(item.get("id")),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments_parts: Vec::new(),
        }
    }

    fn update_from_item(&mut self, item: &Map<String, Value>) {
        if let Some(call_id) = non_empty_str(item.get("call_id")) {
            self.call_id = call_id;
        }
        if let Some(item_id) = non_empty_str(item.get("id")) {
            self.item_id = Some(item_id);
        }
        if let Some(Value::String(name)) = item.get("name") {
            self.name.clone_from(name);
        }
    }

    fn build(&self) -> ToolCall {
        let arguments = parse_arguments(&self.arguments_parts.concat());
        let item_id = self
            .item_id
            .clone()
            .unwrap_or_else(|| format!("fc_{}", self.call_id));
        ToolCall::new(
            format!("{}|{item_id}", self.call_id),
            self.name.clone(),
            arguments,
        )
    }
}

/// Parser for `ChatGPT` Codex Responses SSE events (tau's `_codex_provider_events`).
pub struct CodexParser {
    emitted_content: bool,
    fatal: bool,
    buffer: Vec<String>,
    done: bool,
    content_parts: Vec<String>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    builders: Vec<CodexToolBuilder>,
    active: Vec<usize>,
    by_item_id: HashMap<String, usize>,
    by_call_id: HashMap<String, usize>,
    by_output_index: HashMap<i64, usize>,
}

impl CodexParser {
    #[must_use]
    /// Build a fresh Codex SSE parser.
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            fatal: false,
            buffer: Vec::new(),
            done: false,
            content_parts: Vec::new(),
            finish_reason: None,
            usage: None,
            builders: Vec::new(),
            active: Vec::new(),
            by_item_id: HashMap::new(),
            by_call_id: HashMap::new(),
            by_output_index: HashMap::new(),
        }
    }
}

impl Default for CodexParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for CodexParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let stripped = line.trim();
        if stripped.is_empty() {
            if self.buffer.is_empty() {
                return Feed::empty();
            }
            let data = std::mem::take(&mut self.buffer).join("\n");
            return self.process(&data);
        }
        let Some(rest) = stripped.strip_prefix("data:") else {
            return Feed::empty();
        };
        let value = rest.trim();
        if value == "[DONE]" {
            self.done = true;
            return Feed::stop(Vec::new());
        }
        self.buffer.push(value.to_string());
        Feed::empty()
    }

    fn finalize(&mut self) -> Vec<Delta> {
        // Process any object still buffered without a trailing blank line (tau's
        // `_iter_sse_objects` post-loop flush).
        let mut deltas = Vec::new();
        if !self.done && !self.buffer.is_empty() {
            let data = std::mem::take(&mut self.buffer).join("\n");
            deltas.extend(self.process(&data).deltas);
            // If that flushed object was a terminal `error`/`response.failed`, tau
            // yields the error and `return`s — no trailing `ResponseEnd`. The
            // engine's pre-finalize `fatal()` check can't see this fatal (it only
            // materializes here), so guard it: emit the error alone.
            if self.fatal {
                return deltas;
            }
        }
        let message = assistant_message(
            assistant_content(&self.content_parts.concat(), Vec::new()),
            self.usage.clone().unwrap_or_default(),
            0,
        );
        deltas.push(Delta::End {
            message,
            finish_reason: self.finish_reason.clone(),
        });
        deltas
    }

    fn emitted_content(&self) -> bool {
        self.emitted_content
    }

    fn fatal(&self) -> bool {
        self.fatal
    }
}

impl CodexParser {
    #[allow(clippy::too_many_lines)]
    fn process(&mut self, data: &str) -> Feed {
        let Some(event) = loads_object(data) else {
            return Feed::empty();
        };
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Feed::empty();
        };

        match event_type {
            "error" => {
                self.fatal = true;
                let mut map = Map::new();
                map.insert("event".into(), Value::Object(event.clone()));
                Feed::stop(vec![Delta::Error {
                    message: error_message(&event, "OpenAI Codex returned an error"),
                    data: Some(map),
                }])
            }
            "response.failed" => {
                self.fatal = true;
                let mut map = Map::new();
                map.insert("event".into(), Value::Object(event.clone()));
                Feed::stop(vec![Delta::Error {
                    message: response_error_message(&event),
                    data: Some(map),
                }])
            }
            "response.output_item.added" => {
                if let Some(Value::Object(item)) = event.get("item") {
                    if item.get("type") == Some(&Value::String("function_call".into())) {
                        let builder = CodexToolBuilder::from_item(item);
                        self.track(builder, &event);
                    }
                }
                Feed::empty()
            }
            "response.function_call_arguments.delta" => {
                if let Some(idx) = self.builder_for_event(&event) {
                    if let Some(Value::String(delta)) = event.get("delta") {
                        self.builders[idx].arguments_parts.push(delta.clone());
                    }
                }
                Feed::empty()
            }
            "response.function_call_arguments.done" => {
                if let Some(idx) = self.builder_for_event(&event) {
                    if let Some(Value::String(arguments)) = event.get("arguments") {
                        self.builders[idx].arguments_parts = vec![arguments.clone()];
                    }
                }
                Feed::empty()
            }
            "response.output_text.delta" => {
                if let Some(Value::String(delta)) = event.get("delta") {
                    if !delta.is_empty() {
                        self.emitted_content = true;
                        self.content_parts.push(delta.clone());
                        return Feed::deltas(vec![Delta::Text(delta.clone())]);
                    }
                }
                Feed::empty()
            }
            "response.reasoning.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta" => {
                if let Some(Value::String(delta)) = event.get("delta") {
                    if !delta.is_empty() {
                        return Feed::deltas(vec![Delta::Thinking(delta.clone())]);
                    }
                }
                Feed::empty()
            }
            "response.output_item.done" | "response.output_item.completed" => {
                let Some(Value::Object(item)) = event.get("item") else {
                    return Feed::empty();
                };
                if item.get("type") == Some(&Value::String("function_call".into())) {
                    let idx = if let Some(idx) = self.builder_for_event(&event) {
                        self.builders[idx].update_from_item(item);
                        idx
                    } else {
                        let builder = CodexToolBuilder::from_item(item);
                        self.track(builder, &event)
                    };
                    if let Some(Value::String(arguments)) = item.get("arguments") {
                        self.builders[idx].arguments_parts = vec![arguments.clone()];
                    }
                    let tool_call = self.builders[idx].build();
                    self.untrack(idx);
                    self.emitted_content = true;
                    Feed::deltas(vec![Delta::ToolCall(tool_call)])
                } else if item.get("type") == Some(&Value::String("message".into()))
                    && self.content_parts.is_empty()
                {
                    let text = text_from_done_message(item);
                    if text.is_empty() {
                        Feed::empty()
                    } else {
                        // tau yields a ProviderTextDeltaEvent here, which the outer
                        // loop counts as emitted content (gating mid-stream retry).
                        self.emitted_content = true;
                        self.content_parts.push(text.clone());
                        Feed::deltas(vec![Delta::Text(text)])
                    }
                } else {
                    Feed::empty()
                }
            }
            "response.done" | "response.completed" | "response.incomplete" => {
                self.finish_reason = finish_reason_from_response(&event);
                if let Some(usage) = usage_from_response(&event) {
                    self.usage = Some(usage);
                }
                Feed::stop(Vec::new())
            }
            _ => Feed::empty(),
        }
    }

    fn track(&mut self, builder: CodexToolBuilder, event: &Map<String, Value>) -> usize {
        let item_id = builder.item_id.clone();
        let call_id = builder.call_id.clone();
        let idx = self.builders.len();
        self.builders.push(builder);
        self.active.push(idx);
        if let Some(item_id) = item_id {
            self.by_item_id.insert(item_id, idx);
        }
        if !call_id.is_empty() {
            self.by_call_id.insert(call_id, idx);
        }
        if let Some(output_index) = event_output_index(event) {
            self.by_output_index.insert(output_index, idx);
        }
        idx
    }

    fn untrack(&mut self, idx: usize) {
        self.active.retain(|&i| i != idx);
        self.by_item_id.retain(|_, &mut v| v != idx);
        self.by_call_id.retain(|_, &mut v| v != idx);
        self.by_output_index.retain(|_, &mut v| v != idx);
    }

    fn builder_for_event(&self, event: &Map<String, Value>) -> Option<usize> {
        if let Some(item_id) = event_item_id(event) {
            if let Some(&idx) = self.by_item_id.get(&item_id) {
                return Some(idx);
            }
        }
        if let Some(call_id) = event_call_id(event) {
            if let Some(&idx) = self.by_call_id.get(&call_id) {
                return Some(idx);
            }
        }
        if let Some(output_index) = event_output_index(event) {
            if let Some(&idx) = self.by_output_index.get(&output_index) {
                return Some(idx);
            }
        }
        if self.active.len() == 1 {
            return Some(self.active[0]);
        }
        None
    }
}

fn non_empty_str(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn event_item_id(event: &Map<String, Value>) -> Option<String> {
    if let Some(item_id) = non_empty_str(event.get("item_id")) {
        return Some(item_id);
    }
    if let Some(Value::Object(item)) = event.get("item") {
        return non_empty_str(item.get("id"));
    }
    None
}

fn event_call_id(event: &Map<String, Value>) -> Option<String> {
    if let Some(call_id) = non_empty_str(event.get("call_id")) {
        return Some(call_id);
    }
    if let Some(Value::Object(item)) = event.get("item") {
        return non_empty_str(item.get("call_id"));
    }
    None
}

fn event_output_index(event: &Map<String, Value>) -> Option<i64> {
    match event.get("output_index") {
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

fn text_from_done_message(item: &Map<String, Value>) -> String {
    let Some(Value::Array(content)) = item.get("content") else {
        return String::new();
    };
    let mut parts = String::new();
    for part in content {
        let Value::Object(part) = part else {
            continue;
        };
        match part.get("type").and_then(Value::as_str) {
            Some("output_text") => {
                if let Some(Value::String(text)) = part.get("text") {
                    parts.push_str(text);
                }
            }
            Some("refusal") => {
                if let Some(Value::String(refusal)) = part.get("refusal") {
                    parts.push_str(refusal);
                }
            }
            _ => {}
        }
    }
    parts
}

fn finish_reason_from_response(event: &Map<String, Value>) -> Option<String> {
    if let Some(Value::Object(response)) = event.get("response") {
        if let Some(Value::String(status)) = response.get("status") {
            return Some(status.clone());
        }
    }
    None
}

fn usage_from_response(event: &Map<String, Value>) -> Option<Usage> {
    let Some(Value::Object(response)) = event.get("response") else {
        return None;
    };
    let Some(Value::Object(raw)) = response.get("usage") else {
        return None;
    };
    let cache_read = match raw.get("input_tokens_details") {
        Some(Value::Object(details)) => int_or_zero(details.get("cached_tokens")),
        _ => 0,
    };
    let reasoning = match raw.get("output_tokens_details") {
        Some(Value::Object(details)) => Some(int_or_zero(details.get("reasoning_tokens"))),
        _ => None,
    };
    Some(Usage {
        input: (int_or_zero(raw.get("input_tokens")) - cache_read).max(0),
        output: int_or_zero(raw.get("output_tokens")),
        cache_read,
        cache_write: 0,
        reasoning,
        total_tokens: int_or_zero(raw.get("total_tokens")),
        ..Usage::default()
    })
}

fn response_error_message(event: &Map<String, Value>) -> String {
    if let Some(Value::Object(response)) = event.get("response") {
        if let Some(Value::Object(error)) = response.get("error") {
            if let Some(Value::String(message)) = error.get("message") {
                if !message.is_empty() {
                    return message.clone();
                }
            }
            if let Some(Value::String(code)) = error.get("code") {
                if !code.is_empty() {
                    return format!("OpenAI Codex response failed: {code}");
                }
            }
        }
    }
    "OpenAI Codex response failed".to_string()
}

fn error_message(event: &Map<String, Value>, fallback: &str) -> String {
    if let Some(Value::String(message)) = event.get("message") {
        if !message.is_empty() {
            return message.clone();
        }
    }
    if let Some(Value::String(code)) = event.get("code") {
        if !code.is_empty() {
            return code.clone();
        }
    }
    fallback.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ProviderParser;

    /// A terminal `response.failed` frame with **no** trailing blank line is
    /// processed by the EOF flush in `finalize`. tau yields the error and
    /// returns without a `ResponseEnd`, so `finalize` must emit the error alone —
    /// never a contradictory `done` after `error` (Codex review P1).
    #[test]
    fn eof_flush_of_terminal_error_emits_error_without_end() {
        let mut parser = CodexParser::new();
        let feed = parser.feed_line(
            r#"data: {"type":"response.failed","response":{"error":{"message":"boom"}}}"#,
        );
        assert!(
            feed.deltas.is_empty(),
            "buffered, not processed until flush"
        );
        let deltas = parser.finalize();
        assert!(parser.fatal(), "the failed frame is fatal");
        assert_eq!(deltas.len(), 1, "only the error delta, no End");
        assert!(
            matches!(&deltas[0], Delta::Error { message, .. } if message == "boom"),
            "expected the provider error, got {:?}",
            deltas[0]
        );
    }

    /// Fallback text from a completed `message` item counts as emitted content,
    /// so a subsequent network drop is not retried (matching tau, where it is a
    /// `ProviderTextDeltaEvent`) — Codex review.
    #[test]
    fn fallback_message_text_marks_emitted_content() {
        let mut parser = CodexParser::new();
        parser.feed_line(
            r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"hi"}]}}"#,
        );
        // Trigger the blank-line flush that processes the buffered object.
        let feed = parser.feed_line("");
        assert!(matches!(feed.deltas.as_slice(), [Delta::Text(t)] if t == "hi"));
        assert!(
            parser.emitted_content(),
            "fallback text must set emitted_content"
        );
    }

    /// The User-Agent reproduces tau's `tau (system release; machine)` shape.
    #[test]
    fn user_agent_has_tau_platform_shape() {
        let ua = codex_user_agent();
        assert!(ua.starts_with("tau ("), "got {ua}");
        assert!(ua.ends_with(')'), "got {ua}");
        assert!(ua.contains("; "), "machine separator missing: {ua}");
        // The wrong Rust-consts tokens must not appear (Python uses Darwin/Linux,
        // the kernel release, and arm64/x86_64 — never `unix`/`aarch64`).
        assert!(!ua.contains("unix"), "FAMILY leaked into UA: {ua}");
    }

    /// A non-terminal object flushed at EOF still gets a trailing `End`.
    #[test]
    fn eof_flush_of_completed_emits_end() {
        let mut parser = CodexParser::new();
        parser
            .feed_line(r#"data: {"type":"response.completed","response":{"status":"completed"}}"#);
        let deltas = parser.finalize();
        assert!(!parser.fatal());
        assert_eq!(deltas.len(), 1);
        assert!(matches!(&deltas[0], Delta::End { .. }));
    }

    #[test]
    fn resolve_models_url_handles_each_base_shape() {
        assert_eq!(
            resolve_codex_models_url("https://chatgpt.com/backend-api/codex/responses"),
            "https://chatgpt.com/backend-api/codex/models"
        );
        assert_eq!(
            resolve_codex_models_url("https://chatgpt.com/backend-api/codex/"),
            "https://chatgpt.com/backend-api/codex/models"
        );
        assert_eq!(
            resolve_codex_models_url("https://example.com/v1"),
            "https://example.com/v1/codex/models"
        );
    }

    #[test]
    fn positive_int_rejects_bool_float_and_non_positive() {
        assert_eq!(positive_int(Some(&json!(272_000))), Some(272_000));
        assert_eq!(positive_int(Some(&json!(true))), None);
        assert_eq!(positive_int(Some(&json!(0))), None);
        assert_eq!(positive_int(Some(&json!(-5))), None);
        // Whole-number floats are floats in JSON; tau's isinstance(int) rejects them.
        assert_eq!(positive_int(Some(&json!(272_000.0))), None);
        assert_eq!(positive_int(None), None);
    }

    #[test]
    fn parse_model_limits_reads_slug_and_context_window() {
        let payload = json!({
            "models": [
                {"slug": "gpt-5.6", "context_window": 400_000, "max_output_tokens": 128_000},
                {"slug": "gpt-5.6-luna", "max_context_window": 300_000,
                 "effective_context_window_percent": 90},
                {"slug": "", "context_window": 100},
                {"context_window": 100},
                {"slug": "bad-percent", "context_window": 100,
                 "effective_context_window_percent": 150},
                {"slug": "no-window"},
            ]
        });
        let parsed = parse_codex_model_limits(&payload);
        assert_eq!(parsed.len(), 2);
        let gpt = &parsed["gpt-5.6"];
        assert_eq!(gpt.context_window(), 400_000);
        assert_eq!(gpt.max_output_tokens(), Some(128_000));
        // Falls back to max_context_window and applies the headroom percent.
        let luna = &parsed["gpt-5.6-luna"];
        assert_eq!(luna.context_window(), 300_000);
        assert_eq!(luna.effective_context_window(), 270_000);
    }

    #[test]
    fn parse_model_limits_empty_on_missing_models() {
        assert!(parse_codex_model_limits(&json!({})).is_empty());
        assert!(parse_codex_model_limits(&json!({"models": "nope"})).is_empty());
    }
}
