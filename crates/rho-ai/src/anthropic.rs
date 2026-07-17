//! Anthropic Messages API provider (tau `tau_ai/anthropic.py`).

use std::collections::BTreeMap;
use std::sync::Arc;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::messages::{AgentMessage, ToolCall, Usage};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::tools::AgentTool;
use serde_json::{Map, Value, json};

use crate::engine::{Feed, ProviderParser, RetryPolicy, provider_stream, send_reqwest};
use crate::env::AnthropicConfig;
use crate::openai_compatible::parse_arguments;
use crate::retry::is_transient_status;
use crate::stream::{Delta, assistant_content, assistant_message};
use crate::util::{int_or_none, loads_object, parse_sse_line_no_lstrip, string_or_empty};
use crate::wire::message_text;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: i64 = 4096;

/// Provider adapter for Anthropic's streaming Messages API.
#[derive(Clone)]
pub struct AnthropicProvider {
    config: Arc<AnthropicConfig>,
    client: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl AnthropicProvider {
    /// Build a provider with a fresh HTTP client and the system clock.
    #[must_use]
    pub fn new(config: AnthropicConfig) -> Self {
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
}

impl ModelProvider for AnthropicProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        let payload = build_messages_payload(&self.config, model, system, messages, tools);
        let config = self.config.clone();
        let client = self.client.clone();
        // tau resolves the Anthropic credential once, before the retry loop
        // (anthropic.py:95-102); memoize it across the engine's per-attempt fetch.
        let resolved: Arc<tokio::sync::OnceCell<(String, crate::types::HeaderList)>> =
            Arc::new(tokio::sync::OnceCell::new());

        let fetch = move |_attempt: u32| {
            let config = config.clone();
            let client = client.clone();
            let payload = payload.clone();
            let resolved = resolved.clone();
            async move {
                let (url, headers) = resolved
                    .get_or_try_init(|| resolve_anthropic_request(&config))
                    .await?;
                send_reqwest(&client, url, headers, &payload).await
            }
        };

        provider_stream(
            "anthropic-messages",
            "anthropic",
            self.config.provider_name.clone(),
            model,
            &self.clock,
            self.policy(),
            signal,
            fetch,
            AnthropicParser::new,
            |status, _body| is_transient_status(status),
        )
    }
}

/// Resolve the Anthropic request URL + headers once (tau's pre-loop resolution).
async fn resolve_anthropic_request(
    config: &AnthropicConfig,
) -> Result<(String, crate::types::HeaderList), crate::engine::FetchError> {
    let mut api_key = config.api_key.clone();
    let mut base_url = config.base_url.clone();
    let mut headers = config.headers.clone().unwrap_or_default();
    if let Some(resolver) = &config.credential_resolver {
        let auth = resolver()
            .await
            .map_err(|message| crate::engine::FetchError {
                message,
                retryable: false,
            })?;
        api_key = auth.api_key;
        if let Some(base) = auth.base_url {
            let mut b = base.trim_end_matches('/').to_string();
            if !b.ends_with("/v1") {
                b = format!("{b}/v1");
            }
            base_url = b;
        }
        if let Some(extra) = auth.headers {
            headers.extend(extra);
        }
    }
    // tau builds headers in this order: anthropic-version, content-type,
    // configured headers, auth headers, then x-api-key / Authorization.
    let mut request_headers: crate::types::HeaderList = vec![
        (
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        ),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    request_headers.extend(headers);
    if config.bearer_auth {
        if !request_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        {
            request_headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
        }
    } else {
        request_headers.push(("x-api-key".to_string(), api_key));
    }
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    Ok((url, request_headers))
}

// ---------------------------------------------------------------------------
// Request payload
// ---------------------------------------------------------------------------

/// Build the Anthropic Messages request body (tau `_build_messages_payload`).
#[must_use]
pub fn build_messages_payload(
    config: &AnthropicConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let mut resolved_max_tokens = config.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if resolved_max_tokens == 0 {
        // tau: `max_tokens or DEFAULT_MAX_TOKENS` — a 0/None falls back.
        resolved_max_tokens = DEFAULT_MAX_TOKENS;
    }
    if let Some(budget) = config.thinking_budget_tokens {
        resolved_max_tokens = resolved_max_tokens.max(budget + 1024);
    }

    let mut payload = Map::new();
    payload.insert("model".into(), json!(model));
    payload.insert("max_tokens".into(), json!(resolved_max_tokens));
    payload.insert("stream".into(), json!(true));
    if let Some(oauth) = config
        .oauth_system_prompt
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        payload.insert(
            "system".into(),
            json!([
                {"type": "text", "text": oauth},
                {"type": "text", "text": system},
            ]),
        );
    } else {
        payload.insert("system".into(), json!(system));
    }
    payload.insert(
        "messages".into(),
        Value::Array(messages.iter().map(anthropic_message).collect()),
    );

    if config.thinking_mode == "adaptive" && config.thinking_effort.is_some() {
        payload.insert(
            "thinking".into(),
            json!({"type": "adaptive", "display": "summarized"}),
        );
        payload.insert(
            "output_config".into(),
            json!({"effort": config.thinking_effort}),
        );
    } else if let Some(budget) = config.thinking_budget_tokens {
        payload.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    }

    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(tools.iter().map(anthropic_tool).collect()),
        );
    }
    Value::Object(payload)
}

fn anthropic_message(message: &AgentMessage) -> Value {
    match message {
        AgentMessage::User(m) => json!({"role": "user", "content": m.text()}),
        AgentMessage::Assistant(m) => {
            let mut content: Vec<Value> = Vec::new();
            if !m.text().is_empty() {
                content.push(json!({"type": "text", "text": m.text()}));
            }
            for tool_call in m.tool_calls() {
                content.push(json!({
                    "type": "tool_use",
                    "id": tool_call.id,
                    "name": tool_call.name,
                    "input": Value::Object(tool_call.arguments.clone()),
                }));
            }
            json!({"role": "assistant", "content": content})
        }
        AgentMessage::ToolResult(m) => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": m.tool_call_id,
                "content": m.text(),
                "is_error": m.is_error,
            }],
        }),
        other => json!({"role": "user", "content": message_text(other)}),
    }
}

fn anthropic_tool(tool: &AgentTool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": Value::Object(tool.input_schema().clone()),
    })
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

struct AnthropicToolBuilder {
    id: String,
    name: String,
    arguments_parts: Vec<String>,
}

impl AnthropicToolBuilder {
    fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            arguments_parts: Vec::new(),
        }
    }

    fn build(self, index: i64) -> ToolCall {
        let arguments = parse_arguments(&self.arguments_parts.concat());
        let id = if self.id.is_empty() {
            format!("tool-call-{index}")
        } else {
            self.id
        };
        ToolCall::new(id, self.name, arguments)
    }
}

/// Parser for Anthropic Messages SSE events (tau's `_stream_provider_events`
/// inner loop + `_AnthropicToolBuilder`).
pub struct AnthropicParser {
    emitted_content: bool,
    fatal: bool,
    content_parts: Vec<String>,
    tool_builders: BTreeMap<i64, AnthropicToolBuilder>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

impl AnthropicParser {
    #[must_use]
    /// Build a fresh Anthropic SSE parser.
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            fatal: false,
            content_parts: Vec::new(),
            tool_builders: BTreeMap::new(),
            finish_reason: None,
            usage: None,
        }
    }
}

impl Default for AnthropicParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for AnthropicParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let Some(payload) = parse_sse_line_no_lstrip(line) else {
            return Feed::empty();
        };
        let Some(chunk) = loads_object(&payload) else {
            self.fatal = true;
            return Feed::stop(vec![Delta::Error {
                message: "Provider returned invalid JSON chunk".to_string(),
                data: None,
            }]);
        };

        let event_type = chunk.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "message_start" => {
                if let Some(Value::Object(message)) = chunk.get("message") {
                    self.usage = Some(usage_from_message_start(message.get("usage")));
                }
            }
            "content_block_start" => {
                if let Some(Value::Object(block)) = chunk.get("content_block") {
                    if block.get("type") == Some(&Value::String("tool_use".into())) {
                        let index = chunk.get("index").and_then(Value::as_i64).unwrap_or(0);
                        let builder = self
                            .tool_builders
                            .entry(index)
                            .or_insert_with(AnthropicToolBuilder::new);
                        builder.id = string_or_empty(block.get("id"));
                        builder.name = string_or_empty(block.get("name"));
                        self.emitted_content = true;
                    }
                }
            }
            "content_block_delta" => {
                let Some(Value::Object(delta)) = chunk.get("delta") else {
                    return Feed::empty();
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = string_or_empty(delta.get("text"));
                        if !text.is_empty() {
                            self.emitted_content = true;
                            self.content_parts.push(text.clone());
                            return Feed::deltas(vec![Delta::Text(text)]);
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = string_or_empty(delta.get("thinking"));
                        if !thinking.is_empty() {
                            self.emitted_content = true;
                            return Feed::deltas(vec![Delta::Thinking(thinking)]);
                        }
                    }
                    Some("input_json_delta") => {
                        let index = chunk.get("index").and_then(Value::as_i64).unwrap_or(0);
                        self.tool_builders
                            .entry(index)
                            .or_insert_with(AnthropicToolBuilder::new)
                            .arguments_parts
                            .push(string_or_empty(delta.get("partial_json")));
                        self.emitted_content = true;
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(Value::Object(delta)) = chunk.get("delta") {
                    let reason = string_or_empty(delta.get("stop_reason"));
                    if !reason.is_empty() {
                        self.finish_reason = Some(reason);
                    }
                }
                self.usage = apply_message_delta_usage(self.usage.take(), chunk.get("usage"));
            }
            "error" => {
                let message = match chunk.get("error") {
                    Some(Value::Object(error)) => {
                        let m = string_or_empty(error.get("message"));
                        if m.is_empty() {
                            "Provider returned an error".to_string()
                        } else {
                            m
                        }
                    }
                    _ => "Provider returned an error".to_string(),
                };
                self.fatal = true;
                return Feed::stop(vec![Delta::Error {
                    message,
                    data: Some(chunk.clone()),
                }]);
            }
            _ => {}
        }
        Feed::empty()
    }

    fn finalize(&mut self) -> Vec<Delta> {
        let builders = std::mem::take(&mut self.tool_builders);
        let tool_calls: Vec<ToolCall> = builders
            .into_iter()
            .map(|(index, builder)| builder.build(index))
            .collect();
        let mut deltas: Vec<Delta> = tool_calls.iter().cloned().map(Delta::ToolCall).collect();
        let message = assistant_message(
            assistant_content(&self.content_parts.concat(), tool_calls),
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

fn usage_from_message_start(raw: Option<&Value>) -> Usage {
    let data = match raw {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    let cache_write_1h = match data.get("cache_creation") {
        Some(Value::Object(cache_creation)) => {
            int_or_none(cache_creation.get("ephemeral_1h_input_tokens"))
        }
        _ => None,
    };
    let input = int_or_none(data.get("input_tokens")).unwrap_or(0);
    let output = int_or_none(data.get("output_tokens")).unwrap_or(0);
    let cache_read = int_or_none(data.get("cache_read_input_tokens")).unwrap_or(0);
    let cache_write = int_or_none(data.get("cache_creation_input_tokens")).unwrap_or(0);
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h,
        total_tokens: input + output + cache_read + cache_write,
        ..Usage::default()
    }
}

fn apply_message_delta_usage(usage: Option<Usage>, raw: Option<&Value>) -> Option<Usage> {
    let Some(Value::Object(raw)) = raw else {
        return usage;
    };
    let mut usage = usage.unwrap_or_default();
    if let Some(value) = int_or_none(raw.get("input_tokens")) {
        usage.input = value;
    }
    if let Some(value) = int_or_none(raw.get("output_tokens")) {
        usage.output = value;
    }
    if let Some(value) = int_or_none(raw.get("cache_read_input_tokens")) {
        usage.cache_read = value;
    }
    if let Some(value) = int_or_none(raw.get("cache_creation_input_tokens")) {
        usage.cache_write = value;
    }
    if let Some(Value::Object(details)) = raw.get("output_tokens_details") {
        if let Some(thinking) = int_or_none(details.get("thinking_tokens")) {
            usage.reasoning = Some(thinking);
        }
    }
    usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
    Some(usage)
}
