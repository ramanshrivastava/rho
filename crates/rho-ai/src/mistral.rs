//! Mistral Conversations provider (tau `tau_ai/mistral.py`).

use std::collections::BTreeMap;
use std::sync::Arc;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::messages::{AgentMessage, ToolCall};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::tools::AgentTool;
use serde_json::{Map, Value, json};

use crate::engine::{Feed, ProviderParser, RetryPolicy, provider_stream, send_reqwest};
use crate::env::OpenAICompatibleConfig;
use crate::openai_compatible::parse_arguments;
use crate::retry::is_transient_status;
use crate::stream::{Delta, assistant_content, assistant_message};
use crate::util::{loads_object, non_empty_str, parse_sse_line};
use crate::wire::{message_text, python_dumps};

/// Provider adapter for Mistral's streaming chat API.
#[derive(Clone)]
pub struct MistralConversationsProvider {
    config: Arc<OpenAICompatibleConfig>,
    client: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl MistralConversationsProvider {
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

impl ModelProvider for MistralConversationsProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        let payload = build_mistral_payload(&self.config, model, system, messages, tools);
        let url = format!(
            "{}/chat/completions",
            mistral_base_url(&self.config.base_url)
        );
        let headers: crate::types::HeaderList = {
            let mut headers = self.config.headers.clone().unwrap_or_default();
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", self.config.api_key),
            ));
            headers
        };
        let client = self.client.clone();

        let fetch = move |_attempt: u32| {
            let client = client.clone();
            let payload = payload.clone();
            let url = url.clone();
            let headers = headers.clone();
            async move { send_reqwest(&client, &url, &headers, &payload).await }
        };

        provider_stream(
            "mistral-conversations",
            "mistral",
            self.config.provider_name.clone(),
            model,
            &self.clock,
            self.policy(),
            signal,
            fetch,
            MistralParser::new,
            |status, _body| is_transient_status(status),
        )
    }
}

// ---------------------------------------------------------------------------
// Request payload
// ---------------------------------------------------------------------------

/// Build the Mistral chat-completions request body (tau `_build_mistral_payload`).
#[must_use]
pub fn build_mistral_payload(
    config: &OpenAICompatibleConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let mut wire_messages: Vec<Value> = Vec::new();
    if !system.is_empty() {
        wire_messages.push(json!({"role": "system", "content": system}));
    }
    wire_messages.extend(messages.iter().map(message_to_mistral));

    let mut payload = Map::new();
    payload.insert("model".into(), json!(model));
    payload.insert("stream".into(), json!(true));
    payload.insert("messages".into(), Value::Array(wire_messages));
    if let Some(max_tokens) = config.max_tokens {
        payload.insert("max_tokens".into(), json!(max_tokens));
    }
    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(tools.iter().map(tool_to_mistral).collect()),
        );
    }
    if let Some(effort) = config.reasoning_effort.as_deref() {
        if effort != "none" {
            if uses_reasoning_effort(model) {
                payload.insert("reasoning_effort".into(), json!("high"));
            } else {
                payload.insert("prompt_mode".into(), json!("reasoning"));
            }
        }
    }
    Value::Object(payload)
}

fn message_to_mistral(message: &AgentMessage) -> Value {
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
                    Value::Array(tool_calls.iter().map(tool_call_to_mistral).collect()),
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

fn tool_to_mistral(tool: &AgentTool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": Value::Object(tool.input_schema().clone()),
            "strict": false,
        },
    })
}

fn tool_call_to_mistral(tool_call: &ToolCall) -> Value {
    json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.name,
            "arguments": python_dumps(&Value::Object(tool_call.arguments.clone())),
        },
    })
}

fn mistral_base_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/v1") {
        normalized.to_string()
    } else {
        format!("{normalized}/v1")
    }
}

fn uses_reasoning_effort(model: &str) -> bool {
    matches!(
        model,
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

struct MistralToolCallBuilder {
    id: String,
    name: String,
    arguments_parts: Vec<String>,
}

impl MistralToolCallBuilder {
    fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            arguments_parts: Vec::new(),
        }
    }

    fn add_delta(&mut self, delta: &Map<String, Value>) {
        if let Some(Value::String(id)) = delta.get("id") {
            if id != "null" {
                self.id.clone_from(id);
            }
        }
        let Some(Value::Object(function)) = delta.get("function") else {
            return;
        };
        if let Some(Value::String(name)) = function.get("name") {
            self.name.clone_from(name);
        }
        match function.get("arguments") {
            Some(Value::String(arguments)) => self.arguments_parts.push(arguments.clone()),
            Some(value @ Value::Object(_)) => self.arguments_parts.push(python_dumps(value)),
            _ => {}
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

/// Parser for Mistral chat-completions SSE chunks (tau `_MistralStreamParser`).
pub struct MistralParser {
    emitted_content: bool,
    content_parts: Vec<String>,
    tool_call_builders: BTreeMap<i64, MistralToolCallBuilder>,
    finish_reason: Option<String>,
}

impl MistralParser {
    #[must_use]
    /// Build a fresh Mistral SSE parser.
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            content_parts: Vec::new(),
            tool_call_builders: BTreeMap::new(),
            finish_reason: None,
        }
    }
}

impl Default for MistralParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for MistralParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let Some(payload) = parse_sse_line(line) else {
            return Feed::empty();
        };
        if payload == "[DONE]" {
            return Feed::stop(Vec::new());
        }
        let Some(chunk) = loads_object(&payload) else {
            return Feed::empty();
        };
        let Some(choice) = first_choice(&chunk) else {
            return Feed::empty();
        };
        // tau: `finish_reason or finishReason or self._finish_reason` — the `or`
        // chain skips falsy (empty-string) values, so a `""` on `finish_reason`
        // falls through to `finishReason`, then to the previously captured value.
        if let Some(reason) = non_empty_str(choice.get("finish_reason"))
            .or_else(|| non_empty_str(choice.get("finishReason")))
        {
            self.finish_reason = Some(reason);
        }
        let Some(Value::Object(delta)) = choice.get("delta") else {
            return Feed::empty();
        };

        let mut deltas = Vec::new();
        for content in content_deltas(delta) {
            self.emitted_content = true;
            self.content_parts.push(content.clone());
            deltas.push(Delta::Text(content));
        }
        for thinking in thinking_deltas(delta) {
            self.emitted_content = true;
            deltas.push(Delta::Thinking(thinking));
        }
        for tool_call_delta in tool_call_deltas(delta) {
            self.emitted_content = true;
            let index = tool_call_delta
                .get("index")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.tool_call_builders
                .entry(index)
                .or_insert_with(MistralToolCallBuilder::new)
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
        let has_tools = !tool_calls.is_empty();
        let mut deltas: Vec<Delta> = tool_calls.iter().cloned().map(Delta::ToolCall).collect();
        let finish_reason = self
            .finish_reason
            .clone()
            .unwrap_or_else(|| if has_tools { "tool_calls" } else { "stop" }.to_string());
        let message = assistant_message(
            assistant_content(&self.content_parts.concat(), tool_calls),
            rho_agent::messages::Usage::default(),
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
        false
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

fn content_deltas(delta: &Map<String, Value>) -> Vec<String> {
    match delta.get("content") {
        Some(Value::String(content)) if !content.is_empty() => vec![content.clone()],
        Some(Value::Array(items)) => {
            let mut output = Vec::new();
            for item in items {
                match item {
                    Value::String(s) if !s.is_empty() => output.push(s.clone()),
                    Value::Object(map)
                        if map.get("type") == Some(&Value::String("text".into())) =>
                    {
                        if let Some(Value::String(text)) = map.get("text") {
                            if !text.is_empty() {
                                output.push(text.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            output
        }
        _ => Vec::new(),
    }
}

fn thinking_deltas(delta: &Map<String, Value>) -> Vec<String> {
    let Some(Value::Array(items)) = delta.get("content") else {
        return Vec::new();
    };
    let mut output = Vec::new();
    for item in items {
        let Value::Object(map) = item else {
            continue;
        };
        if map.get("type") != Some(&Value::String("thinking".into())) {
            continue;
        }
        match map.get("thinking") {
            Some(Value::String(thinking)) if !thinking.is_empty() => output.push(thinking.clone()),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Value::Object(part) = part {
                        if let Some(Value::String(text)) = part.get("text") {
                            if !text.is_empty() {
                                output.push(text.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    output
}

fn tool_call_deltas(delta: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    let tool_calls = delta.get("tool_calls").or_else(|| delta.get("toolCalls"));
    match tool_calls {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|tc| match tc {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
