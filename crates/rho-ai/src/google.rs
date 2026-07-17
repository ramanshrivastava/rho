//! Google Generative AI provider (tau `tau_ai/google.py`).

use std::sync::Arc;

use rho_agent::clock::{Clock, system_clock};
use rho_agent::messages::{AgentMessage, ToolCall, Usage};
use rho_agent::provider::{AssistantEventStream, CancellationToken, ModelProvider};
use rho_agent::tools::AgentTool;
use serde_json::{Map, Value, json};

use crate::engine::{Feed, ProviderParser, RetryPolicy, provider_stream, send_reqwest};
use crate::env::OpenAICompatibleConfig;
use crate::retry::is_transient_status;
use crate::stream::{Delta, assistant_content, assistant_message};
use crate::util::{loads_object, parse_sse_line, string_or_default};
use crate::wire::message_text;

/// Provider adapter for Google's Generative Language streaming API.
#[derive(Clone)]
pub struct GoogleGenerativeAIProvider {
    config: Arc<OpenAICompatibleConfig>,
    client: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl GoogleGenerativeAIProvider {
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

impl ModelProvider for GoogleGenerativeAIProvider {
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream {
        let payload = build_google_payload(&self.config, model, system, messages, tools);
        let base_url = self.config.base_url.trim_end_matches('/').to_string();
        let url = format!(
            "{base_url}/models/{model}:streamGenerateContent?alt=sse&key={}",
            self.config.api_key
        );
        let headers: crate::types::HeaderList = {
            let mut headers = self.config.headers.clone().unwrap_or_default();
            headers.push(("content-type".to_string(), "application/json".to_string()));
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
            "google-generative-ai",
            "google",
            self.config.provider_name.clone(),
            model,
            &self.clock,
            self.policy(),
            signal,
            fetch,
            GoogleParser::new,
            |status, _body| is_transient_status(status),
        )
    }
}

// ---------------------------------------------------------------------------
// Request payload
// ---------------------------------------------------------------------------

/// Build the Gemini `streamGenerateContent` request body (tau
/// `_build_google_payload`). `reasoning_effort`/`max_tokens` extras follow tau.
#[must_use]
pub fn build_google_payload(
    config: &OpenAICompatibleConfig,
    model: &str,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentTool],
) -> Value {
    let mut config_map = Map::new();
    let mut payload = Map::new();
    payload.insert(
        "contents".into(),
        Value::Array(messages.iter().map(message_to_google).collect()),
    );
    if !system.is_empty() {
        payload.insert(
            "systemInstruction".into(),
            json!({"parts": [{"text": system}]}),
        );
    }
    if let Some(max_tokens) = config.max_tokens {
        config_map.insert("maxOutputTokens".into(), json!(max_tokens));
    }
    if let Some(thinking) = google_thinking_config(model, config.reasoning_effort.as_deref()) {
        config_map.insert("thinkingConfig".into(), thinking);
    }
    if !config_map.is_empty() {
        payload.insert("generationConfig".into(), Value::Object(config_map));
    }
    if !tools.is_empty() {
        payload.insert(
            "tools".into(),
            json!([{
                "functionDeclarations": tools.iter().map(tool_to_google).collect::<Vec<_>>(),
            }]),
        );
    }
    Value::Object(payload)
}

/// Reasoning-effort → Gemini `thinkingConfig` (tau `_google_thinking_config`).
///
/// Not exercised by any golden (the fixtures set no `reasoning_effort`), but
/// ported for faithful parity with `google.py`. Key insertion order matches tau
/// so the payload bytes stay identical.
fn google_thinking_config(model: &str, reasoning_effort: Option<&str>) -> Option<Value> {
    let effort = reasoning_effort?;
    if effort == "none" {
        if is_gemini3_pro_model(model) {
            return Some(json!({"thinkingLevel": "LOW"}));
        }
        if is_gemini3_flash_model(model) || is_gemma4_model(model) {
            return Some(json!({"thinkingLevel": "MINIMAL"}));
        }
        return Some(json!({"thinkingBudget": 0}));
    }
    if matches!(effort, "MINIMAL" | "LOW" | "MEDIUM" | "HIGH") {
        return Some(json!({"includeThoughts": true, "thinkingLevel": effort}));
    }
    match google_budget(model, effort) {
        None => Some(json!({
            "includeThoughts": true,
            "thinkingLevel": google_level(model, effort),
        })),
        Some(budget) => Some(json!({"includeThoughts": true, "thinkingBudget": budget})),
    }
}

fn google_budget(model: &str, effort: &str) -> Option<i64> {
    let normalized = normalize_effort(effort);
    if !matches!(normalized.as_str(), "minimal" | "low" | "medium" | "high") {
        return None;
    }
    if is_gemini3_pro_model(model) || is_gemini3_flash_model(model) || is_gemma4_model(model) {
        return None;
    }
    let pick = |minimal: i64, low: i64, medium: i64, high: i64| match normalized.as_str() {
        "minimal" => minimal,
        "low" => low,
        "medium" => medium,
        _ => high,
    };
    if model.contains("2.5-pro") {
        Some(pick(128, 2048, 8192, 32768))
    } else if model.contains("2.5-flash-lite") {
        Some(pick(512, 2048, 8192, 24576))
    } else if model.contains("2.5-flash") {
        Some(pick(128, 2048, 8192, 24576))
    } else {
        Some(-1)
    }
}

fn google_level(model: &str, effort: &str) -> String {
    let normalized = normalize_effort(effort);
    if is_gemini3_pro_model(model) {
        return if matches!(normalized.as_str(), "minimal" | "low") {
            "LOW"
        } else {
            "HIGH"
        }
        .to_string();
    }
    if is_gemma4_model(model) {
        return if matches!(normalized.as_str(), "minimal" | "low") {
            "MINIMAL"
        } else {
            "HIGH"
        }
        .to_string();
    }
    // tau: `{...}.get(normalized, "HIGH")` — "high" and any unknown both map to
    // "HIGH", so the default arm covers both.
    match normalized.as_str() {
        "minimal" => "MINIMAL",
        "low" => "LOW",
        "medium" => "MEDIUM",
        _ => "HIGH",
    }
    .to_string()
}

fn normalize_effort(effort: &str) -> String {
    let normalized = effort.to_lowercase();
    if normalized == "xhigh" {
        "high".to_string()
    } else {
        normalized
    }
}

fn is_gemini3_pro_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("gemini-3") && m.contains("pro")
}

fn is_gemini3_flash_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("gemini-3") && m.contains("flash")
}

fn is_gemma4_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("gemma-4") || m.contains("gemma4")
}

fn message_to_google(message: &AgentMessage) -> Value {
    match message {
        AgentMessage::User(m) => json!({"role": "user", "parts": [{"text": m.text()}]}),
        AgentMessage::Assistant(m) => {
            let mut parts: Vec<Value> = Vec::new();
            if !m.text().is_empty() {
                parts.push(json!({"text": m.text()}));
            }
            for tool_call in m.tool_calls() {
                let mut part = Map::new();
                part.insert(
                    "functionCall".into(),
                    json!({
                        "id": tool_call.id,
                        "name": tool_call.name,
                        "args": Value::Object(tool_call.arguments.clone()),
                    }),
                );
                if let Some(sig) = &tool_call.thought_signature {
                    part.insert("thoughtSignature".into(), json!(sig));
                }
                parts.push(Value::Object(part));
            }
            if parts.is_empty() {
                parts.push(json!({"text": ""}));
            }
            json!({"role": "model", "parts": parts})
        }
        AgentMessage::ToolResult(m) => {
            let mut response = Map::new();
            response.insert("name".into(), json!(m.tool_name));
            let key = if m.is_error { "error" } else { "output" };
            response.insert("response".into(), json!({ key: m.text() }));
            if !m.tool_call_id.is_empty() {
                response.insert("id".into(), json!(m.tool_call_id));
            }
            json!({"role": "user", "parts": [{"functionResponse": Value::Object(response)}]})
        }
        other => json!({"role": "user", "parts": [{"text": message_text(other)}]}),
    }
}

fn tool_to_google(tool: &AgentTool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": sanitize_google_schema(&Value::Object(tool.input_schema().clone())),
    })
}

const UNSUPPORTED_GOOGLE_SCHEMA_KEYS: [&str; 2] = ["additionalProperties", "$schema"];

fn sanitize_google_schema(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !UNSUPPORTED_GOOGLE_SCHEMA_KEYS.contains(&key.as_str()))
                .map(|(key, sub)| (key.clone(), sanitize_google_schema(sub)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_google_schema).collect()),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parser for Gemini `streamGenerateContent` SSE chunks (tau `_GoogleStreamParser`).
pub struct GoogleParser {
    emitted_content: bool,
    content_parts: Vec<String>,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
}

impl GoogleParser {
    #[must_use]
    /// Build a fresh Google SSE parser.
    pub fn new() -> Self {
        Self {
            emitted_content: false,
            content_parts: Vec::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        }
    }
}

impl Default for GoogleParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderParser for GoogleParser {
    fn feed_line(&mut self, line: &str) -> Feed {
        let Some(payload) = parse_sse_line(line) else {
            return Feed::empty();
        };
        let Some(chunk) = loads_object(&payload) else {
            return Feed::empty();
        };
        let Some(Value::Array(candidates)) = chunk.get("candidates") else {
            return Feed::empty();
        };
        let Some(Value::Object(candidate)) = candidates.first() else {
            return Feed::empty();
        };
        if let Some(Value::String(reason)) = candidate.get("finishReason") {
            self.finish_reason = Some(reason.clone());
        }
        let Some(Value::Object(content)) = candidate.get("content") else {
            return Feed::empty();
        };
        let Some(Value::Array(parts)) = content.get("parts") else {
            return Feed::empty();
        };

        let mut deltas = Vec::new();
        for part in parts {
            let Value::Object(part) = part else {
                continue;
            };
            if let Some(Value::String(text)) = part.get("text") {
                if !text.is_empty() {
                    self.emitted_content = true;
                    if part.get("thought") == Some(&Value::Bool(true)) {
                        deltas.push(Delta::Thinking(text.clone()));
                    } else {
                        self.content_parts.push(text.clone());
                        deltas.push(Delta::Text(text.clone()));
                    }
                }
            }
            if let Some(Value::Object(function_call)) = part.get("functionCall") {
                self.emitted_content = true;
                let default_id = format!("tool-call-{}", self.tool_calls.len());
                let mut tool_call = ToolCall::new(
                    string_or_default(function_call.get("id"), &default_id),
                    string_or_default(function_call.get("name"), ""),
                    match function_call.get("args") {
                        Some(Value::Object(args)) => args.clone(),
                        _ => Map::new(),
                    },
                );
                if let Some(Value::String(sig)) = part.get("thoughtSignature") {
                    tool_call.thought_signature = Some(sig.clone());
                }
                self.tool_calls.push(tool_call.clone());
                deltas.push(Delta::ToolCall(tool_call));
            }
        }
        Feed::deltas(deltas)
    }

    fn finalize(&mut self) -> Vec<Delta> {
        let has_tool_calls = !self.tool_calls.is_empty();
        let finish_reason = normalize_finish_reason(self.finish_reason.as_deref(), has_tool_calls);
        // The accumulator rebuilds content from the streamed order; the message
        // here is authoritative only for usage (Google reports none in-stream).
        let message = assistant_message(
            assistant_content(&self.content_parts.concat(), self.tool_calls.clone()),
            Usage::default(),
            0,
        );
        vec![Delta::End {
            message,
            finish_reason: Some(finish_reason),
        }]
    }

    fn emitted_content(&self) -> bool {
        self.emitted_content
    }

    fn fatal(&self) -> bool {
        false
    }
}

fn normalize_finish_reason(reason: Option<&str>, has_tool_calls: bool) -> String {
    if has_tool_calls {
        "tool_calls".to_string()
    } else if matches!(reason, Some("MAX_TOKENS" | "MODEL_ARMOR" | "RECITATION")) {
        "length".to_string()
    } else {
        "stop".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_config_matches_tau() {
        // No effort → no thinkingConfig.
        assert_eq!(google_thinking_config("gemini-2.5-flash", None), None);
        // "none" on a 2.5 model → budget 0.
        assert_eq!(
            google_thinking_config("gemini-2.5-flash", Some("none")),
            Some(json!({"thinkingBudget": 0}))
        );
        // "none" on gemini-3-pro → thinkingLevel LOW; on flash/gemma-4 → MINIMAL.
        assert_eq!(
            google_thinking_config("gemini-3-pro", Some("none")),
            Some(json!({"thinkingLevel": "LOW"}))
        );
        assert_eq!(
            google_thinking_config("gemini-3-flash", Some("none")),
            Some(json!({"thinkingLevel": "MINIMAL"}))
        );
        // An explicit level literal passes through.
        assert_eq!(
            google_thinking_config("gemini-2.5-flash", Some("MEDIUM")),
            Some(json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"}))
        );
        // A named effort maps to a budget on 2.5 models.
        assert_eq!(
            google_thinking_config("gemini-2.5-flash", Some("high")),
            Some(json!({"includeThoughts": true, "thinkingBudget": 24576}))
        );
        assert_eq!(
            google_thinking_config("gemini-2.5-pro", Some("low")),
            Some(json!({"includeThoughts": true, "thinkingBudget": 2048}))
        );
        // xhigh normalizes to high.
        assert_eq!(
            google_thinking_config("gemini-2.5-flash-lite", Some("xhigh")),
            Some(json!({"includeThoughts": true, "thinkingBudget": 24576}))
        );
        // gemini-3-pro has no budget table → falls back to a level.
        assert_eq!(
            google_thinking_config("gemini-3-pro", Some("high")),
            Some(json!({"includeThoughts": true, "thinkingLevel": "HIGH"}))
        );
        // Unknown model with a named effort → sentinel budget -1.
        assert_eq!(
            google_thinking_config("some-model", Some("medium")),
            Some(json!({"includeThoughts": true, "thinkingBudget": -1}))
        );
    }
}
