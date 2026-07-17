//! Provider configuration structs (tau `tau_ai/env.py`, plus the Codex config
//! that tau keeps in `openai_codex.py`).
//!
//! tau's configs are frozen dataclasses with keyword defaults. Rust has neither
//! kwargs nor per-field defaults on a struct literal, so each config exposes
//! `new(...)` (required fields only) seeded with tau's defaults, plus `with_*`
//! builders for the optional fields. Field values and defaults match tau exactly
//! — they drive request payloads that must serialize byte-identically.
//!
//! ## Credential resolvers
//!
//! tau's `RuntimeProviderAuthResolver` / `OpenAICodexCredentialResolver` are
//! `Callable[[], Awaitable[...]]` — resolved immediately before a request, so a
//! provider can refresh a short-lived OAuth token per call. rho models them as
//! `Arc<dyn Fn() -> BoxFuture<Result<_, String>>>`: async (boxed future),
//! shareable (the provider is `Send + Sync`), and fallible (a refresh failure
//! surfaces as an `AssistantErrorEvent`, matching Codex's `except Exception`).

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::types::HeaderList;

/// Default OpenAI-compatible base URL (tau `DEFAULT_OPENAI_COMPATIBLE_BASE_URL`).
pub const DEFAULT_OPENAI_COMPATIBLE_BASE_URL: &str = "https://api.openai.com/v1";
/// Default Anthropic base URL (tau `DEFAULT_ANTHROPIC_BASE_URL`).
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
/// Default request timeout in seconds (tau `DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS`).
pub const DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS: f64 = 60.0;
/// Default retry budget (tau `DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES`).
pub const DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES: u32 = 2;
/// Default retry delay cap (tau `DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS`).
pub const DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS: f64 = 1.0;
/// Default Codex base URL (tau `DEFAULT_OPENAI_CODEX_BASE_URL`).
pub const DEFAULT_OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Request auth resolved immediately before a provider call (tau
/// `RuntimeProviderAuth`).
#[derive(Debug, Clone, Default)]
pub struct RuntimeProviderAuth {
    /// Resolved API key / bearer token.
    pub api_key: String,
    /// Optional base-URL override.
    pub base_url: Option<String>,
    /// Optional extra headers.
    pub headers: Option<HeaderList>,
}

/// An async, fallible resolver of per-request auth (tau
/// `RuntimeProviderAuthResolver`).
pub type RuntimeProviderAuthResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<RuntimeProviderAuth, String>> + Send + Sync>;

/// Bearer token + account id for `ChatGPT` Codex Responses (tau
/// `OpenAICodexCredentials`).
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexCredentials {
    /// OAuth access token.
    pub access_token: String,
    /// `ChatGPT` account id.
    pub account_id: String,
}

/// An async, fallible resolver of Codex credentials (tau
/// `OpenAICodexCredentialResolver`).
pub type OpenAICodexCredentialResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<OpenAICodexCredentials, String>> + Send + Sync>;

/// Configuration for an OpenAI-compatible chat/responses endpoint (tau
/// `OpenAICompatibleConfig`). Shared by the `OpenAI`, Google, and Mistral adapters.
#[derive(Clone)]
pub struct OpenAICompatibleConfig {
    /// API key (bearer, unless overridden by a resolver).
    pub api_key: String,
    /// Base URL (no trailing slash).
    pub base_url: String,
    /// Extra request headers.
    pub headers: Option<HeaderList>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: u32,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// `api` family label stamped onto the assistant message.
    pub api: String,
    /// Optional max-tokens cap.
    pub max_tokens: Option<i64>,
    /// Reasoning effort level (or `None`).
    pub reasoning_effort: Option<String>,
    /// Payload key for the reasoning-effort value.
    pub reasoning_effort_parameter: String,
    /// Thinking wire format variant.
    pub thinking_format: String,
    /// Per-provider compatibility flags.
    pub compat: crate::types::JsonMap,
    /// Whether to send `reasoning_effort: none` when reasoning is disabled.
    pub include_reasoning_effort_none: bool,
    /// Human-readable provider name (used in error messages / `provider`).
    pub provider_name: String,
    /// Whether to omit the `Authorization` header.
    pub omit_authorization_header: bool,
    /// Optional per-request credential resolver.
    pub credential_resolver: Option<RuntimeProviderAuthResolver>,
}

impl OpenAICompatibleConfig {
    /// Build a config with tau's defaults from an API key.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string(),
            headers: None,
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            api: "openai-completions".to_string(),
            max_tokens: None,
            reasoning_effort: None,
            reasoning_effort_parameter: "reasoning_effort".to_string(),
            thinking_format: "openai".to_string(),
            compat: crate::types::JsonMap::new(),
            include_reasoning_effort_none: false,
            provider_name: "OpenAI-compatible provider".to_string(),
            omit_authorization_header: false,
            credential_resolver: None,
        }
    }

    /// Set the base URL (a trailing slash is stripped, matching tau's request
    /// builders that all call `base_url.rstrip("/")`).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Set the `provider_name`.
    #[must_use]
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    /// Set the retry budget.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the retry delay cap.
    #[must_use]
    pub fn with_max_retry_delay_seconds(mut self, seconds: f64) -> Self {
        self.max_retry_delay_seconds = seconds;
        self
    }

    /// Set the reasoning effort level.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Set the `api` family label.
    #[must_use]
    pub fn with_api(mut self, api: impl Into<String>) -> Self {
        self.api = api.into();
        self
    }

    /// Set the max-tokens cap.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: i64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set the credential resolver.
    #[must_use]
    pub fn with_credential_resolver(mut self, resolver: RuntimeProviderAuthResolver) -> Self {
        self.credential_resolver = Some(resolver);
        self
    }

    /// Set extra request headers.
    #[must_use]
    pub fn with_headers(mut self, headers: HeaderList) -> Self {
        self.headers = Some(headers);
        self
    }
}

/// Configuration for Anthropic's Messages API (tau `AnthropicConfig`).
#[derive(Clone)]
pub struct AnthropicConfig {
    /// API key.
    pub api_key: String,
    /// Use `Authorization: Bearer` instead of `x-api-key`.
    pub bearer_auth: bool,
    /// Base URL (no trailing slash).
    pub base_url: String,
    /// Extra request headers.
    pub headers: Option<HeaderList>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: u32,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// Optional max-tokens cap.
    pub max_tokens: Option<i64>,
    /// Optional thinking budget in tokens.
    pub thinking_budget_tokens: Option<i64>,
    /// Optional thinking effort level.
    pub thinking_effort: Option<String>,
    /// Thinking mode (`budget` or `adaptive`).
    pub thinking_mode: String,
    /// Human-readable provider name.
    pub provider_name: String,
    /// Optional OAuth system prompt (prepended as a second system block).
    pub oauth_system_prompt: Option<String>,
    /// Optional per-request credential resolver.
    pub credential_resolver: Option<RuntimeProviderAuthResolver>,
}

impl AnthropicConfig {
    /// Build a config with tau's defaults from an API key.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            bearer_auth: false,
            base_url: DEFAULT_ANTHROPIC_BASE_URL.to_string(),
            headers: None,
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            max_tokens: None,
            thinking_budget_tokens: None,
            thinking_effort: None,
            thinking_mode: "budget".to_string(),
            provider_name: "Anthropic".to_string(),
            oauth_system_prompt: None,
            credential_resolver: None,
        }
    }

    /// Set the base URL (trailing slash stripped).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Set the retry budget.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the retry delay cap.
    #[must_use]
    pub fn with_max_retry_delay_seconds(mut self, seconds: f64) -> Self {
        self.max_retry_delay_seconds = seconds;
        self
    }

    /// Set the thinking budget in tokens.
    #[must_use]
    pub fn with_thinking_budget_tokens(mut self, tokens: i64) -> Self {
        self.thinking_budget_tokens = Some(tokens);
        self
    }

    /// Enable bearer auth.
    #[must_use]
    pub fn with_bearer_auth(mut self, bearer_auth: bool) -> Self {
        self.bearer_auth = bearer_auth;
        self
    }

    /// Set the credential resolver.
    #[must_use]
    pub fn with_credential_resolver(mut self, resolver: RuntimeProviderAuthResolver) -> Self {
        self.credential_resolver = Some(resolver);
        self
    }
}

/// Configuration for the `ChatGPT` Codex subscription Responses endpoint (tau
/// `OpenAICodexConfig`).
#[derive(Clone)]
pub struct OpenAICodexConfig {
    /// Required credential resolver.
    pub credential_resolver: OpenAICodexCredentialResolver,
    /// Base URL.
    pub base_url: String,
    /// Extra request headers.
    pub headers: Option<HeaderList>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: u32,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// Originator header value.
    pub originator: String,
    /// Optional reasoning effort level.
    pub reasoning_effort: Option<String>,
    /// Reasoning summary mode.
    pub reasoning_summary: String,
    /// Human-readable provider name.
    pub provider_name: String,
}

impl OpenAICodexConfig {
    /// Build a config with tau's defaults from a credential resolver.
    #[must_use]
    pub fn new(credential_resolver: OpenAICodexCredentialResolver) -> Self {
        Self {
            credential_resolver,
            base_url: DEFAULT_OPENAI_CODEX_BASE_URL.to_string(),
            headers: None,
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            originator: "tau".to_string(),
            reasoning_effort: None,
            reasoning_summary: "auto".to_string(),
            provider_name: "OpenAI Codex".to_string(),
        }
    }

    /// Set the base URL.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Set the retry budget.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the retry delay cap.
    #[must_use]
    pub fn with_max_retry_delay_seconds(mut self, seconds: f64) -> Self {
        self.max_retry_delay_seconds = seconds;
        self
    }

    /// Set the reasoning effort level.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }
}

/// Load OpenAI-compatible configuration from environment variables (tau
/// `openai_compatible_config_from_env`).
///
/// Reads `OPENAI_API_KEY` (required), `OPENAI_BASE_URL`, `OPENAI_TIMEOUT_SECONDS`,
/// `OPENAI_MAX_RETRIES`, `OPENAI_MAX_RETRY_DELAY_SECONDS`, validating each exactly
/// as tau does.
pub fn openai_compatible_config_from_env() -> Result<OpenAICompatibleConfig, String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or("Missing required environment variable: OPENAI_API_KEY")?;

    let timeout_seconds = timeout_seconds_from_env(
        "OPENAI_TIMEOUT_SECONDS",
        DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
    )?;
    let max_retries =
        non_negative_int_from_env("OPENAI_MAX_RETRIES", DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES)?;
    let max_retry_delay_seconds = non_negative_float_from_env(
        "OPENAI_MAX_RETRY_DELAY_SECONDS",
        DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
    )?;

    let base_url = std::env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string();

    let mut config = OpenAICompatibleConfig::new(api_key);
    config.base_url = base_url;
    config.timeout_seconds = timeout_seconds;
    config.max_retries = max_retries;
    config.max_retry_delay_seconds = max_retry_delay_seconds;
    Ok(config)
}

fn timeout_seconds_from_env(name: &str, default: f64) -> Result<f64, String> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("Environment variable must be a number: {name}"))?;
    // Reject NaN/±inf: unlike Python (where `nan <= 0` is False and httpx tolerates
    // it), Rust's `Duration::from_secs_f64` panics on a non-finite timeout.
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "Environment variable must be greater than 0: {name}"
        ));
    }
    Ok(value)
}

fn non_negative_int_from_env(name: &str, default: u32) -> Result<u32, String> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    // tau parses `int(raw)` then rejects negatives; a negative parses in Python
    // but fails the `< 0` check. Mirror that with a signed parse.
    let value: i64 = raw
        .parse()
        .map_err(|_| format!("Environment variable must be an integer: {name}"))?;
    if value < 0 {
        return Err(format!("Environment variable must be 0 or greater: {name}"));
    }
    Ok(u32::try_from(value).unwrap_or(u32::MAX))
}

fn non_negative_float_from_env(name: &str, default: f64) -> Result<f64, String> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("Environment variable must be a number: {name}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("Environment variable must be 0 or greater: {name}"));
    }
    Ok(value)
}
