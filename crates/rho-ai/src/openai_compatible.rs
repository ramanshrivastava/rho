//! OpenAI-compatible chat/responses provider (tau `tau_ai/openai_compatible.py`).
//!
//! Serves `/chat/completions` for most models and transparently routes reasoning
//! models (`gpt-5.5`/`gpt-5.4`/`*-codex`) to `/v1/responses`. Two SSE parsers —
//! [`ChatStreamParser`] and [`ResponsesStreamParser`] — share the engine's retry
//! envelope; the request builders reproduce tau's exact payload key order.

use std::collections::BTreeMap;
use std::sync::Arc;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::messages::{AgentMessage, AssistantContent, ThinkingContent, ToolCall, Usage};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::tools::AgentTool;
use serde_json::{Map, Value, json};

use crate::engine::{Feed, ProviderParser, RetryPolicy, provider_stream, send_reqwest};
use crate::env::OpenAICompatibleConfig;
use crate::retry::is_transient_status;
use crate::stream::{Delta, assistant_content, assistant_message};
use crate::util::{int_or_none, int_or_zero, loads_object, parse_sse_line};
use crate::wire::{message_text, python_dumps};

const RESPONSES_ONLY_PREFIXES: [&str; 2] = ["gpt-5.5", "gpt-5.4"];

/// Whether `model` must be served over the Responses API (tau `_use_responses_api`).
#[must_use]
pub fn use_responses_api(model: &str) -> bool {
    let normalized = model.trim().to_lowercase();
    if normalized.contains("codex") {
        return true;
    }
    RESPONSES_ONLY_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

/// Provider adapter for OpenAI-compatible chat/responses APIs.
#[derive(Clone)]
pub struct OpenAICompatibleProvider {
    config: Arc<OpenAICompatibleConfig>,
    client: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl OpenAICompatibleProvider {
    /// Build a provider with a fresh HTTP client and the system clock.
    #[must_use]
    pub fn new(config: OpenAICompatibleConfig) -> Self {
        let client = crate::http::create_client(config.timeout_seconds);
        Self {
            config: Arc::new(config),
            client,
            clock: system_clock(),
        }
    }

    /// Override the HTTP client (e.g. to point at the mock provider).
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
}

impl ModelProvider for OpenAICompatibleProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        let use_responses = self.config.api == "openai-responses" || use_responses_api(model);
        let (payload, endpoint) = if use_responses {
            (
                build_responses_payload(&self.config, model, system, messages, tools),
                "/responses",
            )
        } else {
            (
                build_chat_payload(&self.config, model, system, messages, tools),
                "/chat/completions",
            )
        };
        let base_url = self.config.base_url.trim_end_matches('/').to_string();
        let default_url = format!("{base_url}{endpoint}");

        let config = self.config.clone();
        let client = self.client.clone();
        // tau resolves the OpenAI credential once, before the retry loop
        // (openai_compatible.py:206-220), so a refresh runs at most once per
        // response and every attempt reuses the same URL/headers. A OnceCell
        // memoizes it across the engine's per-attempt fetch calls. (Codex, which
        // tau resolves *inside* the loop, keeps its per-attempt resolution.)
        let endpoint = endpoint.to_string();
        let resolved: Arc<tokio::sync::OnceCell<(String, crate::types::HeaderList)>> =
            Arc::new(tokio::sync::OnceCell::new());
        let fetch = move |_attempt: u32| {
            let config = config.clone();
            let client = client.clone();
            let payload = payload.clone();
            let default_url = default_url.clone();
            let endpoint = endpoint.clone();
            let resolved = resolved.clone();
            async move {
                let (request_url, headers) = resolved
                    .get_or_try_init(|| resolve_openai_request(&config, &default_url, &endpoint))
                    .await?;
                send_reqwest(&client, request_url, headers, &payload).await
            }
        };

        let parser_factory = move || -> Box<dyn ProviderParser> {
            if use_responses {
                Box::new(ResponsesStreamParser::new())
            } else {
                Box::new(ChatStreamParser::new())
            }
        };

        provider_stream(
            self.config.api.clone(),
            self.config.provider_name.clone(),
            self.config.provider_name.clone(),
            model,
            &self.clock,
            self.policy(),
            signal,
            fetch,
            parser_factory,
            |status, _body| is_transient_status(status),
        )
    }
}

/// Resolve the request URL + headers once (tau's pre-loop credential resolution).
/// Runs the optional credential resolver, applies any base-URL/header overrides,
/// and adds the bearer `Authorization` unless suppressed or already present.
async fn resolve_openai_request(
    config: &OpenAICompatibleConfig,
    default_url: &str,
    endpoint: &str,
) -> Result<(String, crate::types::HeaderList), crate::engine::FetchError> {
    let mut api_key = config.api_key.clone();
    let mut request_url = default_url.to_string();
    let mut headers = config.headers.clone().unwrap_or_default();
    if let Some(resolver) = &config.credential_resolver {
        let auth = resolver()
            .await
            .map_err(|message| crate::engine::FetchError {
                message,
                retryable: false,
            })?;
        api_key = auth.api_key;
        if let Some(extra) = auth.headers {
            headers.extend(extra);
        }
        if let Some(base) = auth.base_url {
            request_url = format!("{}{endpoint}", base.trim_end_matches('/'));
        }
    }
    if !config.omit_authorization_header
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
    {
        headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
    }
    Ok((request_url, headers))
}

// ---------------------------------------------------------------------------
// Request payloads
// ---------------------------------------------------------------------------

/// Build the `/chat/completions` request body (tau `_build_chat_payload`).
#[must_use]
pub fn build_chat_payload(
    config: &OpenAICompatibleConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let compat = &config.compat;
    let supports_store = bool_compat(compat, "supportsStore", true);
    let supports_usage = bool_compat(compat, "supportsUsageInStreaming", true);
    let supports_reasoning_effort = bool_compat(compat, "supportsReasoningEffort", true);
    let max_tokens_field = string_compat(compat, "maxTokensField", "max_completion_tokens");

    let mut payload = Map::new();
    payload.insert("model".into(), json!(model));
    payload.insert("stream".into(), json!(true));
    let mut wire_messages = vec![system_message(system)];
    wire_messages.extend(messages.iter().map(message_to_openai));
    payload.insert("messages".into(), Value::Array(wire_messages));
    if supports_usage {
        payload.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if supports_store {
        payload.insert("store".into(), json!(false));
    }
    if let Some(max_tokens) = config.max_tokens {
        let field = if max_tokens_field == "max_tokens" {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        payload.insert(field.into(), json!(max_tokens));
    }
    if let Some(Value::Object(_)) = compat.get("openrouterProvider") {
        payload.insert("provider".into(), compat["openrouterProvider"].clone());
    }
    apply_chat_reasoning(
        &mut payload,
        if supports_reasoning_effort {
            config.reasoning_effort.as_deref()
        } else {
            None
        },
        &config.reasoning_effort_parameter,
        &config.thinking_format,
        config.include_reasoning_effort_none,
    );
    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(tools.iter().map(tool_to_openai).collect()),
        );
        if compat.get("zaiToolStream") == Some(&Value::Bool(true)) {
            payload.insert("tool_stream".into(), json!(true));
        }
    }
    Value::Object(payload)
}

fn apply_chat_reasoning(
    payload: &mut Map<String, Value>,
    reasoning_effort: Option<&str>,
    reasoning_effort_parameter: &str,
    thinking_format: &str,
    include_reasoning_effort_none: bool,
) {
    let reasoning_enabled = reasoning_effort.is_some_and(|e| e != "none");
    match thinking_format {
        "zai" | "qwen" => {
            payload.insert("enable_thinking".into(), json!(reasoning_enabled));
        }
        "qwen-chat-template" => {
            payload.insert(
                "chat_template_kwargs".into(),
                json!({"enable_thinking": reasoning_enabled, "preserve_thinking": true}),
            );
        }
        "deepseek" => {
            payload.insert(
                "thinking".into(),
                json!({"type": if reasoning_enabled { "enabled" } else { "disabled" }}),
            );
            if reasoning_enabled {
                payload.insert("reasoning_effort".into(), json!(reasoning_effort));
            }
        }
        // tau checks openrouter / `reasoning.effort` BEFORE `together`
        // (openai_compatible.py:698-708), so the guard arm must precede the
        // `together` literal — otherwise `thinking_format="together"` with
        // `reasoning_effort_parameter="reasoning.effort"` would take the wrong
        // branch. A guarded `_ if …` arm placed first falls through to
        // `"together"` only when its condition is false.
        _ if thinking_format == "openrouter"
            || reasoning_effort_parameter == "reasoning.effort" =>
        {
            if reasoning_enabled {
                payload.insert("reasoning".into(), json!({"effort": reasoning_effort}));
            } else if include_reasoning_effort_none {
                payload.insert("reasoning".into(), json!({"effort": "none"}));
            }
        }
        "together" => {
            payload.insert("reasoning".into(), json!({"enabled": reasoning_enabled}));
            if reasoning_enabled {
                payload.insert("reasoning_effort".into(), json!(reasoning_effort));
            }
        }
        _ => {
            if reasoning_enabled || include_reasoning_effort_none {
                payload.insert(
                    "reasoning_effort".into(),
                    json!(reasoning_effort.unwrap_or("none")),
                );
            }
        }
    }
}

/// Build the `/v1/responses` request body (tau `_build_responses_payload`).
#[must_use]
pub fn build_responses_payload(
    config: &OpenAICompatibleConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let mut payload = Map::new();
    payload.insert("model".into(), json!(model));
    payload.insert("stream".into(), json!(true));
    payload.insert("store".into(), json!(false));
    payload.insert("instructions".into(), json!(system));
    payload.insert(
        "input".into(),
        Value::Array(messages_to_responses_input(messages)),
    );
    if let Some(max_tokens) = config.max_tokens {
        payload.insert("max_output_tokens".into(), json!(max_tokens));
    }
    if let Some(effort) = normalize_responses_effort(config.reasoning_effort.as_deref()) {
        payload.insert(
            "reasoning".into(),
            json!({"effort": effort, "summary": "auto"}),
        );
    }
    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(tools.iter().map(tool_to_responses).collect()),
        );
    }
    Value::Object(payload)
}

fn normalize_responses_effort(reasoning_effort: Option<&str>) -> Option<String> {
    let normalized = reasoning_effort?.trim().to_lowercase();
    if normalized.is_empty() || normalized == "none" {
        None
    } else {
        Some(normalized)
    }
}

fn system_message(system: &str) -> Value {
    json!({"role": "system", "content": system})
}

fn message_to_openai(message: &AgentMessage) -> Value {
    match message {
        AgentMessage::User(m) => json!({"role": "user", "content": m.text()}),
        AgentMessage::Assistant(m) => {
            let mut item = Map::new();
            item.insert("role".into(), json!("assistant"));
            item.insert("content".into(), json!(m.text()));
            let tool_calls = m.tool_calls();
            if !tool_calls.is_empty() {
                item.insert(
                    "tool_calls".into(),
                    Value::Array(tool_calls.iter().map(tool_call_to_openai).collect()),
                );
            }
            Value::Object(item)
        }
        AgentMessage::ToolResult(m) => json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id,
            "name": m.tool_name,
            "content": m.text(),
        }),
        other => json!({"role": "user", "content": message_text(other)}),
    }
}

fn tool_to_openai(tool: &AgentTool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": Value::Object(tool.input_schema().clone()),
        },
    })
}

fn tool_call_to_openai(tool_call: &ToolCall) -> Value {
    json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.name,
            "arguments": python_dumps(&Value::Object(tool_call.arguments.clone())),
        },
    })
}

fn messages_to_responses_input(messages: &[AgentMessage]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message {
            AgentMessage::User(m) => items.push(json!({"role": "user", "content": m.text()})),
            AgentMessage::Assistant(m) => {
                if !m.text().is_empty() {
                    items.push(json!({"role": "assistant", "content": m.text()}));
                }
                for tool_call in m.tool_calls() {
                    items.push(json!({
                        "type": "function_call",
                        "call_id": tool_call.id,
                        "name": tool_call.name,
                        "arguments": python_dumps(&Value::Object(tool_call.arguments.clone())),
                    }));
                }
            }
            AgentMessage::ToolResult(m) => items.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id,
                "output": m.text(),
            })),
            _ => {}
        }
    }
    items
}

fn tool_to_responses(tool: &AgentTool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": Value::Object(tool.input_schema().clone()),
    })
}

fn bool_compat(compat: &Map<String, Value>, key: &str, default: bool) -> bool {
    // tau: `bool(compat.get(key, default))`. An **absent** key yields `default`
    // (already a bool); a **present** value — including an explicit `null` —
    // takes Python truthiness, so `null`/`false`/`0`/`""`/`[]`/`{}` → `false`.
    match compat.get(key) {
        None => default,
        Some(value) => !is_falsy(value),
    }
}

fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

fn string_compat(compat: &Map<String, Value>, key: &str, default: &str) -> String {
    match compat.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Chat-completions parser
// ---------------------------------------------------------------------------

struct ChatToolCallBuilder {
    id: String,
    name: String,
    arguments_parts: Vec<String>,
}

impl ChatToolCallBuilder {
    fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            arguments_parts: Vec::new(),
        }
    }

    fn add_delta(&mut self, delta: &Map<String, Value>) {
        if let Some(Value::String(id)) = delta.get("id") {
            self.id.clone_from(id);
        }
        if let Some(Value::Object(function)) = delta.get("function") {
            if let Some(Value::String(name)) = function.get("name") {
                self.name.clone_from(name);
            }
            if let Some(Value::String(arguments)) = function.get("arguments") {
                self.arguments_parts.push(arguments.clone());
            }
        }
    }

    fn build(self, index: i64) -> ToolCall {
        let arguments_text: String = self.arguments_parts.concat();
        let arguments = parse_arguments(&arguments_text);
        let id = if self.id.is_empty() {
            format!("tool-call-{index}")
        } else {
            self.id
        };
        ToolCall::new(id, self.name, arguments)
    }
}

/// Parser for `OpenAI` `/chat/completions` SSE chunks (tau `_ChatStreamParser`).
pub struct ChatStreamParser {
    emitted_content: bool,
    fatal: bool,
    content_parts: Vec<String>,
    thinking_parts: Vec<String>,
    thinking_signature: Option<String>,
    tool_call_builders: BTreeMap<i64, ChatToolCallBuilder>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

impl ChatStreamParser {
    #[must_use]
    /// Build a fresh chat-completions SSE parser.
    /// Build a fresh responses SSE parser.
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            fatal: false,
            content_parts: Vec::new(),
            thinking_parts: Vec::new(),
            thinking_signature: None,
            tool_call_builders: BTreeMap::new(),
            finish_reason: None,
            usage: None,
        }
    }
}

impl Default for ChatStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for ChatStreamParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let Some(payload) = parse_sse_line(line) else {
            return Feed::empty();
        };
        if payload == "[DONE]" {
            return Feed::stop(Vec::new());
        }
        let Some(chunk) = loads_object(&payload) else {
            self.fatal = true;
            return Feed::stop(vec![Delta::Error {
                message: "Provider returned invalid JSON chunk".to_string(),
                data: None,
            }]);
        };

        let chunk_usage = matches!(chunk.get("usage"), Some(Value::Object(_)));
        if let Some(Value::Object(usage)) = chunk.get("usage") {
            self.usage = Some(parse_chunk_usage(usage));
        }

        let Some(choice) = first_choice(&chunk) else {
            return Feed::empty();
        };

        if !chunk_usage {
            if let Some(Value::Object(usage)) = choice.get("usage") {
                self.usage = Some(parse_chunk_usage(usage));
            }
        }

        // tau: `choice.get("finish_reason") or self._finish_reason` — a falsy
        // (empty-string) reason must not overwrite a previously captured one.
        if let Some(reason) = str_or_none(choice.get("finish_reason")) {
            self.finish_reason = Some(reason);
        }

        let Some(Value::Object(delta)) = choice.get("delta") else {
            return Feed::empty();
        };

        let mut deltas = Vec::new();
        if let Some(Value::String(content)) = delta.get("content") {
            if !content.is_empty() {
                self.emitted_content = true;
                self.content_parts.push(content.clone());
                deltas.push(Delta::Text(content.clone()));
            }
        }
        if let Some((field_name, text)) = thinking_delta(delta) {
            self.emitted_content = true;
            self.thinking_parts.push(text.clone());
            // tau: `self._thinking_signature = self._thinking_signature or
            // field_name` — the first reasoning channel seen wins.
            if self.thinking_signature.is_none() {
                self.thinking_signature = Some(field_name.to_string());
            }
            deltas.push(Delta::Thinking(text));
        }
        for tool_call_delta in tool_call_deltas(delta) {
            self.emitted_content = true;
            let index = tool_call_delta
                .get("index")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.tool_call_builders
                .entry(index)
                .or_insert_with(ChatToolCallBuilder::new)
                .add_delta(tool_call_delta);
        }
        Feed::deltas(deltas)
    }

    fn finalize(&mut self) -> Vec<Delta> {
        let builders = std::mem::take(&mut self.tool_call_builders);
        let tool_calls: Vec<ToolCall> = builders
            .into_iter()
            .map(|(index, builder)| builder.build(index))
            .collect();
        let mut deltas: Vec<Delta> = tool_calls.iter().cloned().map(Delta::ToolCall).collect();
        let mut content = assistant_content(&self.content_parts.concat(), tool_calls);
        // tau `_ChatStreamParser.finalize`: prepend the accumulated reasoning as
        // a `ThinkingContent`, carrying the reasoning-channel name as the replay
        // signature so `_copy_replay_metadata` can stamp it on the canonical
        // message.
        if !self.thinking_parts.is_empty() {
            let mut thinking = ThinkingContent::new(self.thinking_parts.concat());
            thinking
                .thinking_signature
                .clone_from(&self.thinking_signature);
            content.insert(0, AssistantContent::Thinking(thinking));
        }
        let message = assistant_message(content, self.usage.clone().unwrap_or_default(), 0);
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

// ---------------------------------------------------------------------------
// Responses parser
// ---------------------------------------------------------------------------

struct ResponsesToolCallBuilder {
    call_id: String,
    name: String,
    output_index: i64,
    arguments_parts: Vec<String>,
    arguments_final: Option<String>,
}

impl ResponsesToolCallBuilder {
    fn new() -> Self {
        Self {
            call_id: String::new(),
            name: String::new(),
            output_index: 0,
            arguments_parts: Vec::new(),
            arguments_final: None,
        }
    }

    fn add_arguments_delta(&mut self, delta: Option<&Value>) {
        if let Some(Value::String(s)) = delta {
            self.arguments_parts.push(s.clone());
        }
    }

    fn set_final(
        &mut self,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
        output_index: Option<i64>,
    ) {
        if let Some(call_id) = call_id {
            if !call_id.is_empty() {
                self.call_id = call_id.to_string();
            }
        }
        if let Some(name) = name {
            if !name.is_empty() {
                self.name = name.to_string();
            }
        }
        if let Some(arguments) = arguments {
            self.arguments_final = Some(arguments.to_string());
        }
        if let Some(output_index) = output_index {
            self.output_index = output_index;
        }
    }

    fn build(self, index: i64) -> ToolCall {
        let arguments_text = self
            .arguments_final
            .unwrap_or_else(|| self.arguments_parts.concat());
        let arguments = parse_arguments(&arguments_text);
        let id = if self.call_id.is_empty() {
            format!("tool-call-{index}")
        } else {
            self.call_id
        };
        ToolCall::new(id, self.name, arguments)
    }
}

/// Parser for `OpenAI` `/v1/responses` SSE events (tau `_ResponsesStreamParser`).
pub struct ResponsesStreamParser {
    emitted_content: bool,
    fatal: bool,
    content_parts: Vec<String>,
    tool_call_builders: Vec<(String, ResponsesToolCallBuilder)>,
    status: Option<String>,
    usage: Option<Usage>,
}

impl ResponsesStreamParser {
    /// Build a fresh responses SSE parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            fatal: false,
            content_parts: Vec::new(),
            tool_call_builders: Vec::new(),
            status: None,
            usage: None,
        }
    }

    fn builder_mut(&mut self, item_id: &str) -> &mut ResponsesToolCallBuilder {
        if let Some(pos) = self
            .tool_call_builders
            .iter()
            .position(|(id, _)| id == item_id)
        {
            &mut self.tool_call_builders[pos].1
        } else {
            self.tool_call_builders
                .push((item_id.to_string(), ResponsesToolCallBuilder::new()));
            &mut self.tool_call_builders.last_mut().expect("just pushed").1
        }
    }
}

impl Default for ResponsesStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for ResponsesStreamParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let Some(payload) = parse_sse_line(line) else {
            return Feed::empty();
        };
        if payload == "[DONE]" {
            return Feed::empty();
        }
        let Some(chunk) = loads_object(&payload) else {
            return Feed::empty();
        };
        let Some(Value::String(chunk_type)) = chunk.get("type") else {
            return Feed::empty();
        };
        let chunk_type = chunk_type.as_str();

        match chunk_type {
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(Value::String(delta)) = chunk.get("delta") {
                    if !delta.is_empty() {
                        self.emitted_content = true;
                        self.content_parts.push(delta.clone());
                        return Feed::deltas(vec![Delta::Text(delta.clone())]);
                    }
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(Value::String(delta)) = chunk.get("delta") {
                    if !delta.is_empty() {
                        self.emitted_content = true;
                        return Feed::deltas(vec![Delta::Thinking(delta.clone())]);
                    }
                }
            }
            "response.output_item.added" => {
                register_responses_item(self, chunk.get("item"), chunk.get("output_index"));
            }
            "response.function_call_arguments.delta" => {
                if let Some(Value::String(item_id)) = chunk.get("item_id") {
                    let item_id = item_id.clone();
                    let delta = chunk.get("delta").cloned();
                    self.builder_mut(&item_id)
                        .add_arguments_delta(delta.as_ref());
                    self.emitted_content = true;
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(Value::String(item_id)) = chunk.get("item_id") {
                    let item_id = item_id.clone();
                    let arguments = chunk
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    self.builder_mut(&item_id)
                        .set_final(None, None, arguments.as_deref(), None);
                }
            }
            "response.output_item.done" => {
                finalize_responses_item(self, chunk.get("item"), chunk.get("output_index"));
            }
            "response.completed" | "response.incomplete" => {
                self.status = responses_finish_reason(&chunk);
                if let Some(usage) = usage_from_responses_event(&chunk) {
                    self.usage = Some(usage);
                }
                return Feed::stop(Vec::new());
            }
            "response.failed" => {
                self.fatal = true;
                return Feed::stop(vec![responses_failure_delta(&chunk)]);
            }
            "error" => {
                self.fatal = true;
                let mut data = Map::new();
                data.insert("event".into(), Value::Object(chunk.clone()));
                return Feed::stop(vec![Delta::Error {
                    message: responses_error_message(&chunk),
                    data: Some(data),
                }]);
            }
            _ => {}
        }
        Feed::empty()
    }

    fn finalize(&mut self) -> Vec<Delta> {
        let mut builders = std::mem::take(&mut self.tool_call_builders);
        builders.sort_by_key(|(_, b)| b.output_index);
        let tool_calls: Vec<ToolCall> = builders
            .into_iter()
            .enumerate()
            .map(|(index, (_, builder))| builder.build(i64::try_from(index).unwrap_or(i64::MAX)))
            .collect();
        let has_tools = !tool_calls.is_empty();
        let mut deltas: Vec<Delta> = tool_calls.iter().cloned().map(Delta::ToolCall).collect();
        let finish_reason = normalize_responses_finish_reason(self.status.as_deref(), has_tools);
        let message = assistant_message(
            assistant_content(&self.content_parts.concat(), tool_calls),
            self.usage.clone().unwrap_or_default(),
            0,
        );
        deltas.push(Delta::End {
            message,
            finish_reason: Some(finish_reason),
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

fn register_responses_item(
    parser: &mut ResponsesStreamParser,
    item: Option<&Value>,
    output_index: Option<&Value>,
) {
    let Some(Value::Object(item)) = item else {
        return;
    };
    if item.get("type") != Some(&Value::String("function_call".into())) {
        return;
    }
    let Some(Value::String(item_id)) = item.get("id") else {
        return;
    };
    let item_id = item_id.clone();
    let call_id = str_or_none(item.get("call_id"));
    let name = str_or_none(item.get("name"));
    let raw_arguments = match item.get("arguments") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let output_index = output_index.and_then(Value::as_i64);
    parser.builder_mut(&item_id).set_final(
        call_id.as_deref(),
        name.as_deref(),
        raw_arguments.as_deref(),
        output_index,
    );
}

fn finalize_responses_item(
    parser: &mut ResponsesStreamParser,
    item: Option<&Value>,
    output_index: Option<&Value>,
) {
    let Some(Value::Object(item)) = item else {
        return;
    };
    if item.get("type") != Some(&Value::String("function_call".into())) {
        return;
    }
    let Some(Value::String(item_id)) = item.get("id") else {
        return;
    };
    let item_id = item_id.clone();
    let call_id = str_or_none(item.get("call_id"));
    let name = str_or_none(item.get("name"));
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .map(str::to_string);
    let output_index = output_index.and_then(Value::as_i64);
    parser.builder_mut(&item_id).set_final(
        call_id.as_deref(),
        name.as_deref(),
        arguments.as_deref(),
        output_index,
    );
}

fn responses_finish_reason(chunk: &Map<String, Value>) -> Option<String> {
    if let Some(Value::Object(response)) = chunk.get("response") {
        if let Some(Value::String(status)) = response.get("status") {
            return Some(status.clone());
        }
    }
    None
}

fn normalize_responses_finish_reason(status: Option<&str>, has_tools: bool) -> String {
    if has_tools {
        "tool_calls".to_string()
    } else if status == Some("incomplete") {
        "length".to_string()
    } else {
        "stop".to_string()
    }
}

fn responses_failure_delta(chunk: &Map<String, Value>) -> Delta {
    let mut message = "Provider response failed".to_string();
    if let Some(Value::Object(response)) = chunk.get("response") {
        if let Some(Value::Object(error)) = response.get("error") {
            if let Some(Value::String(m)) = error.get("message") {
                if !m.is_empty() {
                    message.clone_from(m);
                }
            }
        }
    }
    let mut data = Map::new();
    data.insert("event".into(), Value::Object(chunk.clone()));
    Delta::Error {
        message,
        data: Some(data),
    }
}

fn responses_error_message(chunk: &Map<String, Value>) -> String {
    if let Some(Value::String(message)) = chunk.get("message") {
        if !message.is_empty() {
            return message.clone();
        }
    }
    if let Some(Value::Object(error)) = chunk.get("error") {
        if let Some(Value::String(nested)) = error.get("message") {
            if !nested.is_empty() {
                return nested.clone();
            }
        }
    }
    "Provider stream error".to_string()
}

fn str_or_none(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Usage parsing
// ---------------------------------------------------------------------------

fn parse_chunk_usage(raw: &Map<String, Value>) -> Usage {
    let prompt_tokens = int_or_zero(raw.get("prompt_tokens"));
    let prompt_details = raw.get("prompt_tokens_details");
    let mut cached_tokens: Option<i64> = None;
    let mut cache_write = 0;
    if let Some(Value::Object(details)) = prompt_details {
        cached_tokens = int_or_none(details.get("cached_tokens"));
        cache_write = int_or_zero(details.get("cache_write_tokens"));
    }
    if cached_tokens.is_none() {
        cached_tokens = int_or_none(raw.get("prompt_cache_hit_tokens"));
    }
    let cache_read = cached_tokens.unwrap_or(0);
    let fresh_input = (prompt_tokens - cache_read - cache_write).max(0);
    let output = int_or_zero(raw.get("completion_tokens"));
    let reasoning = match raw.get("completion_tokens_details") {
        Some(Value::Object(details)) => Some(int_or_zero(details.get("reasoning_tokens"))),
        _ => None,
    };
    Usage {
        input: fresh_input,
        output,
        cache_read,
        cache_write,
        reasoning,
        total_tokens: fresh_input + output + cache_read + cache_write,
        ..Usage::default()
    }
}

fn usage_from_responses_event(chunk: &Map<String, Value>) -> Option<Usage> {
    let Some(Value::Object(response)) = chunk.get("response") else {
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

// ---------------------------------------------------------------------------
// Shared small helpers
// ---------------------------------------------------------------------------

/// Parse tool-argument text, falling back to `{"_raw_arguments": text}` when the
/// text is non-empty but not valid JSON (tau's builders). Empty text → `{}`.
pub(crate) fn parse_arguments(text: &str) -> Map<String, Value> {
    if text.is_empty() {
        return Map::new();
    }
    if let Some(map) = loads_object(text) {
        map
    } else {
        let mut map = Map::new();
        map.insert("_raw_arguments".into(), json!(text));
        map
    }
}

fn first_choice(chunk: &Map<String, Value>) -> Option<&Map<String, Value>> {
    match chunk.get("choices") {
        Some(Value::Array(choices)) => match choices.first() {
            Some(Value::Object(choice)) => Some(choice),
            _ => None,
        },
        _ => None,
    }
}

/// Extract a reasoning delta and the channel it arrived on (tau
/// `_thinking_delta`): returns the field name (`reasoning_content` / `reasoning`
/// / `thinking`) and the text, or `None` when this chunk carries no reasoning.
fn thinking_delta(delta: &Map<String, Value>) -> Option<(&'static str, String)> {
    for field in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(Value::String(value)) = delta.get(field) {
            if !value.is_empty() {
                return Some((field, value.clone()));
            }
        }
    }
    None
}

fn tool_call_deltas(delta: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    match delta.get("tool_calls") {
        Some(Value::Array(tool_calls)) => tool_calls
            .iter()
            .filter_map(|tc| match tc {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tau checks openrouter / `reasoning.effort` before `together`
    /// (openai_compatible.py:698-708). With both set, the openrouter shape wins.
    #[test]
    fn reasoning_effort_parameter_precedes_together() {
        let mut config = OpenAICompatibleConfig::new("k");
        config.thinking_format = "together".to_string();
        config.reasoning_effort_parameter = "reasoning.effort".to_string();
        config.reasoning_effort = Some("high".to_string());

        let payload = build_chat_payload(&config, "m", "sys", &[], &[]);
        assert_eq!(payload["reasoning"], serde_json::json!({"effort": "high"}));
        // The `together` shape (`reasoning.enabled` + top-level `reasoning_effort`)
        // must NOT be emitted.
        assert!(payload.get("reasoning_effort").is_none());
    }

    /// Without the `reasoning.effort` parameter, `together` keeps its own shape.
    #[test]
    fn together_shape_used_without_reasoning_effort_parameter() {
        let mut config = OpenAICompatibleConfig::new("k");
        config.thinking_format = "together".to_string();
        config.reasoning_effort = Some("high".to_string());

        let payload = build_chat_payload(&config, "m", "sys", &[], &[]);
        assert_eq!(payload["reasoning"], serde_json::json!({"enabled": true}));
        assert_eq!(payload["reasoning_effort"], serde_json::json!("high"));
    }

    /// A present `null` compat flag is falsy (tau `bool(None)` → False), while an
    /// absent flag falls back to the default.
    #[test]
    fn bool_compat_null_is_false_absent_is_default() {
        let mut compat = Map::new();
        compat.insert("supportsStore".into(), Value::Null);
        assert!(!bool_compat(&compat, "supportsStore", true), "null → false");
        assert!(bool_compat(&compat, "missing", true), "absent → default");
    }
}
