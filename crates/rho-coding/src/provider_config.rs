//! Durable provider configuration for rho coding sessions (port of tau's
//! `tau_coding/provider_config.py`).
//!
//! tau's provider configs are frozen dataclasses whose `__post_init__` validates
//! on construction and whose `dataclasses.replace` re-validates. rho mirrors this
//! with plain structs plus an explicit [`validate`](OpenAICompatibleProviderConfig::validate)
//! method that construction/merge paths call, preserving tau's exact
//! `ProviderConfigError` messages.
//!
//! ## Dependency inversion vs tau
//!
//! tau's `provider_config` imports the concrete `FileCredentialStore` /
//! `oauth_registry` and reads them internally. Those modules live outside this
//! cluster in rho, so credential lookup is injected through the
//! [`CredentialReader`] trait: [`load_provider_settings`] and the save/mutate
//! helpers take an `Option<&dyn CredentialReader>` the integrator supplies. The
//! built-in OAuth provider ids are inlined in [`oauth_provider_registered`]
//! (extensions cannot register providers in this port yet). tau's missing-key
//! `RuntimeError` becomes a [`ProviderConfigError`]. See `dev-notes/phase-4b*.md`.

#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::items_after_statements
)]

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use rho_agent::types::{JsonMap, JsonValue};
use rho_ai::env::{
    AnthropicConfig, DEFAULT_ANTHROPIC_BASE_URL, DEFAULT_OPENAI_CODEX_BASE_URL,
    DEFAULT_OPENAI_COMPATIBLE_BASE_URL, DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES,
    DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS, DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
    OpenAICompatibleConfig,
};

use crate::catalog_loader::{CatalogError, effective_catalog, save_user_catalog_entries};
use crate::paths::RhoPaths;
use crate::provider_catalog::{
    BUILTIN_PROVIDER_CATALOG, ModelCatalogMetadata, ModelCostTier, ProviderApi,
    ProviderCatalogEntry, ProviderKind, ThinkingParameter,
};
use crate::thinking::{
    DEFAULT_THINKING_LEVEL, anthropic_thinking_budget_for_level, normalize_thinking_level,
    normalize_thinking_levels, reasoning_effort_for_level,
};

/// Default provider name (tau `DEFAULT_PROVIDER_NAME`).
pub const DEFAULT_PROVIDER_NAME: &str = "openai";
/// Default model (tau `DEFAULT_MODEL`).
pub const DEFAULT_MODEL: &str = "gpt-5.4";

const MAX_RETRIES_DEFAULT: i64 = DEFAULT_OPENAI_COMPATIBLE_MAX_RETRIES as i64;

/// Raised when rho provider configuration is invalid (tau `ProviderConfigError`).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ProviderConfigError(pub String);

fn cfg_err(message: impl Into<String>) -> ProviderConfigError {
    ProviderConfigError(message.into())
}

/// Python truthiness for optional CLI/config strings: an empty string is falsy
/// and falls back to `default`, matching tau's pervasive `value or default`
/// (`provider_name or default_provider`, `model or provider.default_model`).
/// So `--provider ""` / `--model ""` resolve to the defaults, not an error.
fn or_default<'a>(value: Option<&'a str>, default: &'a str) -> &'a str {
    value
        .filter(|candidate| !candidate.is_empty())
        .unwrap_or(default)
}

impl From<CatalogError> for ProviderConfigError {
    fn from(error: CatalogError) -> Self {
        Self(error.0)
    }
}

/// Credential lookup used while building runtime provider config (tau's
/// `CredentialReader` protocol).
///
/// [`get_oauth`](CredentialReader::get_oauth) narrows tau's `OAuthCredential`
/// object to its only field this module reads — the access token.
pub trait CredentialReader {
    /// Return a stored API key by credential name.
    fn get(&self, name: &str) -> Option<String>;
    /// Return a stored OAuth access token by credential name.
    fn get_oauth(&self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
}

/// Return whether an OAuth provider is registered for `name` (tau's
/// `oauth_registry.get_oauth_provider(name) is not None`, inlined to the
/// built-in provider ids).
fn oauth_provider_registered(name: &str) -> bool {
    matches!(name, "anthropic" | "github-copilot" | "openai-codex")
}

// ---------------------------------------------------------------------------
// Runtime model metadata
// ---------------------------------------------------------------------------

/// Runtime metadata for one configured model (tau `ProviderModelMetadata`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderModelMetadata {
    /// Display name.
    pub name: Option<String>,
    /// Request-family override.
    pub api: Option<ProviderApi>,
    /// Base-URL override.
    pub base_url: Option<String>,
    /// Whether the model is a reasoning model.
    pub reasoning: Option<bool>,
    /// Supported input modalities.
    pub input: Vec<String>,
    /// Flat base cost.
    pub cost: IndexMap<String, f64>,
    /// Tiered pricing.
    pub cost_tiers: Vec<ModelCostTier>,
    /// Context window size.
    pub context_window: Option<i64>,
    /// Max output tokens.
    pub max_tokens: Option<i64>,
    /// Extra request headers.
    pub headers: IndexMap<String, String>,
    /// Per-model compatibility flags.
    pub compat: JsonMap,
    /// Thinking-level mapping (`None` value marks an unsupported level).
    pub thinking_level_map: IndexMap<String, Option<String>>,
}

impl ProviderModelMetadata {
    /// Serialize this model metadata to JSON-compatible data (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut map = JsonMap::new();
        map.insert("name".into(), opt_string_json(self.name.as_ref()));
        map.insert("api".into(), opt_string_json(self.api.as_ref()));
        map.insert("base_url".into(), opt_string_json(self.base_url.as_ref()));
        map.insert(
            "reasoning".into(),
            self.reasoning.map_or(JsonValue::Null, JsonValue::Bool),
        );
        map.insert(
            "input".into(),
            JsonValue::Array(
                self.input
                    .iter()
                    .map(|s| JsonValue::String(s.clone()))
                    .collect(),
            ),
        );
        map.insert("cost".into(), float_map_json(&self.cost));
        map.insert("cost_tiers".into(), cost_tiers_json(&self.cost_tiers));
        map.insert("context_window".into(), opt_int_json(self.context_window));
        map.insert("max_tokens".into(), opt_int_json(self.max_tokens));
        map.insert("headers".into(), string_map_json(&self.headers));
        map.insert("compat".into(), JsonValue::Object(self.compat.clone()));
        map.insert(
            "thinking_level_map".into(),
            thinking_level_map_json(&self.thinking_level_map),
        );
        JsonValue::Object(map)
    }
}

// ---------------------------------------------------------------------------
// Provider config structs
// ---------------------------------------------------------------------------

/// Durable settings for one OpenAI-compatible provider (tau
/// `OpenAICompatibleProviderConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAICompatibleProviderConfig {
    /// Provider name.
    pub name: String,
    /// Base URL.
    pub base_url: String,
    /// Request-family label.
    pub api: ProviderApi,
    /// Environment variable holding the API key.
    pub api_key_env: String,
    /// Credential-store key.
    pub credential_name: Option<String>,
    /// Declared models.
    pub models: Vec<String>,
    /// Default model.
    pub default_model: String,
    /// Per-model context windows.
    pub context_windows: IndexMap<String, i64>,
    /// Extra request headers.
    pub headers: IndexMap<String, String>,
    /// Provider-level compatibility flags.
    pub compat: JsonMap,
    /// Per-model metadata.
    pub model_metadata: IndexMap<String, ProviderModelMetadata>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: i64,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// Declared thinking levels.
    pub thinking_levels: Option<Vec<String>>,
    /// Models that support thinking.
    pub thinking_models: Vec<String>,
    /// Default thinking level.
    pub thinking_default: Option<String>,
    /// Thinking-parameter wire key.
    pub thinking_parameter: Option<ThinkingParameter>,
    /// Remembered per-model thinking preferences.
    pub thinking_defaults: IndexMap<String, String>,
}

impl OpenAICompatibleProviderConfig {
    /// Build a config with tau's field defaults for `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string(),
            api: "openai-completions".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            credential_name: None,
            models: vec![DEFAULT_MODEL.to_string()],
            default_model: DEFAULT_MODEL.to_string(),
            context_windows: IndexMap::new(),
            headers: IndexMap::new(),
            compat: JsonMap::new(),
            model_metadata: IndexMap::new(),
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: MAX_RETRIES_DEFAULT,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            thinking_levels: None,
            thinking_models: Vec::new(),
            thinking_default: None,
            thinking_parameter: None,
            thinking_defaults: IndexMap::new(),
        }
    }

    /// Run tau's `__post_init__` validation.
    pub fn validate(&self) -> Result<(), ProviderConfigError> {
        validate_provider_numbers(
            self.timeout_seconds,
            self.max_retries,
            self.max_retry_delay_seconds,
        )?;
        validate_context_windows(&self.context_windows)?;
        validate_model_metadata(&self.models, &self.model_metadata)?;
        validate_json_object(&self.compat, "Provider compat")?;
        validate_thinking_config(
            self.thinking_levels.as_ref(),
            &self.thinking_models,
            self.thinking_default.as_ref(),
            self.thinking_parameter.as_ref(),
        )?;
        validate_thinking_defaults(&self.thinking_defaults)?;
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        let mut map = JsonMap::new();
        map.insert("name".into(), JsonValue::String(self.name.clone()));
        map.insert("type".into(), JsonValue::String("openai-compatible".into()));
        map.insert("base_url".into(), JsonValue::String(self.base_url.clone()));
        map.insert("api".into(), JsonValue::String(self.api.clone()));
        map.insert(
            "api_key_env".into(),
            JsonValue::String(self.api_key_env.clone()),
        );
        map.insert(
            "credential_name".into(),
            opt_string_json(self.credential_name.as_ref()),
        );
        map.insert("models".into(), string_vec_json(&self.models));
        map.insert(
            "default_model".into(),
            JsonValue::String(self.default_model.clone()),
        );
        map.insert(
            "context_windows".into(),
            int_map_json(&self.context_windows),
        );
        map.insert("headers".into(), string_map_json(&self.headers));
        map.insert("compat".into(), JsonValue::Object(self.compat.clone()));
        map.insert(
            "model_metadata".into(),
            model_metadata_json(&self.model_metadata),
        );
        map.insert("timeout_seconds".into(), float_json(self.timeout_seconds));
        map.insert("max_retries".into(), JsonValue::from(self.max_retries));
        map.insert(
            "max_retry_delay_seconds".into(),
            float_json(self.max_retry_delay_seconds),
        );
        map.insert(
            "thinking_levels".into(),
            opt_string_vec_json(self.thinking_levels.as_ref()),
        );
        map.insert(
            "thinking_models".into(),
            string_vec_json(&self.thinking_models),
        );
        map.insert(
            "thinking_default".into(),
            opt_string_json(self.thinking_default.as_ref()),
        );
        map.insert(
            "thinking_parameter".into(),
            opt_string_json(self.thinking_parameter.as_ref()),
        );
        map.insert(
            "thinking_defaults".into(),
            string_map_json(&self.thinking_defaults),
        );
        JsonValue::Object(map)
    }
}

/// Durable settings for Anthropic's Messages API (tau `AnthropicProviderConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicProviderConfig {
    /// Provider name.
    pub name: String,
    /// Base URL.
    pub base_url: String,
    /// Request-family label.
    pub api: ProviderApi,
    /// Environment variable holding the API key.
    pub api_key_env: String,
    /// Credential-store key.
    pub credential_name: Option<String>,
    /// Declared models.
    pub models: Vec<String>,
    /// Default model.
    pub default_model: String,
    /// Per-model context windows.
    pub context_windows: IndexMap<String, i64>,
    /// Extra request headers.
    pub headers: IndexMap<String, String>,
    /// Provider-level compatibility flags.
    pub compat: JsonMap,
    /// Per-model metadata.
    pub model_metadata: IndexMap<String, ProviderModelMetadata>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: i64,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// Declared thinking levels.
    pub thinking_levels: Option<Vec<String>>,
    /// Models that support thinking.
    pub thinking_models: Vec<String>,
    /// Default thinking level.
    pub thinking_default: Option<String>,
    /// Thinking-parameter wire key.
    pub thinking_parameter: Option<ThinkingParameter>,
    /// Remembered per-model thinking preferences.
    pub thinking_defaults: IndexMap<String, String>,
}

impl AnthropicProviderConfig {
    /// Build a config with tau's field defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            name: "anthropic".to_string(),
            base_url: DEFAULT_ANTHROPIC_BASE_URL.to_string(),
            api: "anthropic-messages".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            credential_name: Some("anthropic".to_string()),
            models: vec!["claude-sonnet-4-6".to_string()],
            default_model: "claude-sonnet-4-6".to_string(),
            context_windows: IndexMap::new(),
            headers: IndexMap::new(),
            compat: JsonMap::new(),
            model_metadata: IndexMap::new(),
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: MAX_RETRIES_DEFAULT,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            thinking_levels: None,
            thinking_models: Vec::new(),
            thinking_default: None,
            thinking_parameter: None,
            thinking_defaults: IndexMap::new(),
        }
    }

    /// Run tau's `__post_init__` validation.
    pub fn validate(&self) -> Result<(), ProviderConfigError> {
        validate_provider_numbers(
            self.timeout_seconds,
            self.max_retries,
            self.max_retry_delay_seconds,
        )?;
        validate_context_windows(&self.context_windows)?;
        validate_model_metadata(&self.models, &self.model_metadata)?;
        validate_json_object(&self.compat, "Provider compat")?;
        validate_thinking_config(
            self.thinking_levels.as_ref(),
            &self.thinking_models,
            self.thinking_default.as_ref(),
            self.thinking_parameter.as_ref(),
        )?;
        validate_thinking_defaults(&self.thinking_defaults)?;
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        let mut map = JsonMap::new();
        map.insert("name".into(), JsonValue::String(self.name.clone()));
        map.insert("type".into(), JsonValue::String("anthropic".into()));
        map.insert("base_url".into(), JsonValue::String(self.base_url.clone()));
        map.insert("api".into(), JsonValue::String(self.api.clone()));
        map.insert(
            "api_key_env".into(),
            JsonValue::String(self.api_key_env.clone()),
        );
        map.insert(
            "credential_name".into(),
            opt_string_json(self.credential_name.as_ref()),
        );
        map.insert("models".into(), string_vec_json(&self.models));
        map.insert(
            "default_model".into(),
            JsonValue::String(self.default_model.clone()),
        );
        map.insert(
            "context_windows".into(),
            int_map_json(&self.context_windows),
        );
        map.insert("headers".into(), string_map_json(&self.headers));
        map.insert("compat".into(), JsonValue::Object(self.compat.clone()));
        map.insert(
            "model_metadata".into(),
            model_metadata_json(&self.model_metadata),
        );
        map.insert("timeout_seconds".into(), float_json(self.timeout_seconds));
        map.insert("max_retries".into(), JsonValue::from(self.max_retries));
        map.insert(
            "max_retry_delay_seconds".into(),
            float_json(self.max_retry_delay_seconds),
        );
        map.insert(
            "thinking_levels".into(),
            opt_string_vec_json(self.thinking_levels.as_ref()),
        );
        map.insert(
            "thinking_models".into(),
            string_vec_json(&self.thinking_models),
        );
        map.insert(
            "thinking_default".into(),
            opt_string_json(self.thinking_default.as_ref()),
        );
        map.insert(
            "thinking_parameter".into(),
            opt_string_json(self.thinking_parameter.as_ref()),
        );
        map.insert(
            "thinking_defaults".into(),
            string_map_json(&self.thinking_defaults),
        );
        JsonValue::Object(map)
    }
}

impl Default for AnthropicProviderConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Durable settings for OpenAI Codex subscription OAuth (tau
/// `OpenAICodexProviderConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAICodexProviderConfig {
    /// Provider name.
    pub name: String,
    /// Base URL.
    pub base_url: String,
    /// Environment variable holding the access token.
    pub api_key_env: String,
    /// Credential-store key.
    pub credential_name: Option<String>,
    /// Declared models.
    pub models: Vec<String>,
    /// Default model.
    pub default_model: String,
    /// Per-model context windows.
    pub context_windows: IndexMap<String, i64>,
    /// Extra request headers.
    pub headers: IndexMap<String, String>,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Maximum retry attempts.
    pub max_retries: i64,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
    /// Declared thinking levels.
    pub thinking_levels: Option<Vec<String>>,
    /// Models that support thinking.
    pub thinking_models: Vec<String>,
    /// Default thinking level.
    pub thinking_default: Option<String>,
    /// Thinking-parameter wire key.
    pub thinking_parameter: Option<ThinkingParameter>,
    /// Remembered per-model thinking preferences.
    pub thinking_defaults: IndexMap<String, String>,
}

impl OpenAICodexProviderConfig {
    /// Build a config with tau's field defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            name: "openai-codex".to_string(),
            base_url: DEFAULT_OPENAI_CODEX_BASE_URL.to_string(),
            api_key_env: "OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            credential_name: Some("openai-codex".to_string()),
            models: vec![
                "gpt-5.5".to_string(),
                "gpt-5.4".to_string(),
                "gpt-5.4-mini".to_string(),
                "gpt-5.3-codex".to_string(),
                "gpt-5.3-codex-spark".to_string(),
                "gpt-5.2".to_string(),
            ],
            default_model: "gpt-5.5".to_string(),
            context_windows: IndexMap::new(),
            headers: IndexMap::new(),
            timeout_seconds: DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
            max_retries: MAX_RETRIES_DEFAULT,
            max_retry_delay_seconds: DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
            thinking_levels: None,
            thinking_models: Vec::new(),
            thinking_default: None,
            thinking_parameter: None,
            thinking_defaults: IndexMap::new(),
        }
    }

    /// Run tau's `__post_init__` validation.
    pub fn validate(&self) -> Result<(), ProviderConfigError> {
        validate_provider_numbers(
            self.timeout_seconds,
            self.max_retries,
            self.max_retry_delay_seconds,
        )?;
        validate_context_windows(&self.context_windows)?;
        validate_thinking_config(
            self.thinking_levels.as_ref(),
            &self.thinking_models,
            self.thinking_default.as_ref(),
            self.thinking_parameter.as_ref(),
        )?;
        validate_thinking_defaults(&self.thinking_defaults)?;
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        let mut map = JsonMap::new();
        map.insert("name".into(), JsonValue::String(self.name.clone()));
        map.insert("type".into(), JsonValue::String("openai-codex".into()));
        map.insert("base_url".into(), JsonValue::String(self.base_url.clone()));
        map.insert(
            "api_key_env".into(),
            JsonValue::String(self.api_key_env.clone()),
        );
        map.insert(
            "credential_name".into(),
            opt_string_json(self.credential_name.as_ref()),
        );
        map.insert("models".into(), string_vec_json(&self.models));
        map.insert(
            "default_model".into(),
            JsonValue::String(self.default_model.clone()),
        );
        map.insert(
            "context_windows".into(),
            int_map_json(&self.context_windows),
        );
        map.insert("headers".into(), string_map_json(&self.headers));
        map.insert("timeout_seconds".into(), float_json(self.timeout_seconds));
        map.insert("max_retries".into(), JsonValue::from(self.max_retries));
        map.insert(
            "max_retry_delay_seconds".into(),
            float_json(self.max_retry_delay_seconds),
        );
        map.insert(
            "thinking_levels".into(),
            opt_string_vec_json(self.thinking_levels.as_ref()),
        );
        map.insert(
            "thinking_models".into(),
            string_vec_json(&self.thinking_models),
        );
        map.insert(
            "thinking_default".into(),
            opt_string_json(self.thinking_default.as_ref()),
        );
        map.insert(
            "thinking_parameter".into(),
            opt_string_json(self.thinking_parameter.as_ref()),
        );
        map.insert(
            "thinking_defaults".into(),
            string_map_json(&self.thinking_defaults),
        );
        JsonValue::Object(map)
    }
}

impl Default for OpenAICodexProviderConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A durable provider config: one of the three provider kinds (tau's
/// `ProviderConfig` union).
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderConfig {
    /// OpenAI-compatible provider.
    OpenAICompatible(OpenAICompatibleProviderConfig),
    /// Anthropic provider.
    Anthropic(AnthropicProviderConfig),
    /// OpenAI Codex subscription provider.
    OpenAICodex(OpenAICodexProviderConfig),
}

impl ProviderConfig {
    /// Provider name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::OpenAICompatible(c) => &c.name,
            Self::Anthropic(c) => &c.name,
            Self::OpenAICodex(c) => &c.name,
        }
    }
    /// Base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        match self {
            Self::OpenAICompatible(c) => &c.base_url,
            Self::Anthropic(c) => &c.base_url,
            Self::OpenAICodex(c) => &c.base_url,
        }
    }
    /// API-key environment variable.
    #[must_use]
    pub fn api_key_env(&self) -> &str {
        match self {
            Self::OpenAICompatible(c) => &c.api_key_env,
            Self::Anthropic(c) => &c.api_key_env,
            Self::OpenAICodex(c) => &c.api_key_env,
        }
    }
    /// Credential-store key.
    #[must_use]
    pub fn credential_name(&self) -> Option<&str> {
        match self {
            Self::OpenAICompatible(c) => c.credential_name.as_deref(),
            Self::Anthropic(c) => c.credential_name.as_deref(),
            Self::OpenAICodex(c) => c.credential_name.as_deref(),
        }
    }
    /// Declared models.
    #[must_use]
    pub fn models(&self) -> &[String] {
        match self {
            Self::OpenAICompatible(c) => &c.models,
            Self::Anthropic(c) => &c.models,
            Self::OpenAICodex(c) => &c.models,
        }
    }
    /// Default model.
    #[must_use]
    pub fn default_model(&self) -> &str {
        match self {
            Self::OpenAICompatible(c) => &c.default_model,
            Self::Anthropic(c) => &c.default_model,
            Self::OpenAICodex(c) => &c.default_model,
        }
    }
    /// Per-model context windows.
    #[must_use]
    pub fn context_windows(&self) -> &IndexMap<String, i64> {
        match self {
            Self::OpenAICompatible(c) => &c.context_windows,
            Self::Anthropic(c) => &c.context_windows,
            Self::OpenAICodex(c) => &c.context_windows,
        }
    }
    /// Request headers.
    #[must_use]
    pub fn headers(&self) -> &IndexMap<String, String> {
        match self {
            Self::OpenAICompatible(c) => &c.headers,
            Self::Anthropic(c) => &c.headers,
            Self::OpenAICodex(c) => &c.headers,
        }
    }
    /// Request timeout in seconds.
    #[must_use]
    pub fn timeout_seconds(&self) -> f64 {
        match self {
            Self::OpenAICompatible(c) => c.timeout_seconds,
            Self::Anthropic(c) => c.timeout_seconds,
            Self::OpenAICodex(c) => c.timeout_seconds,
        }
    }
    /// Retry budget.
    #[must_use]
    pub fn max_retries(&self) -> i64 {
        match self {
            Self::OpenAICompatible(c) => c.max_retries,
            Self::Anthropic(c) => c.max_retries,
            Self::OpenAICodex(c) => c.max_retries,
        }
    }
    /// Retry delay cap in seconds.
    #[must_use]
    pub fn max_retry_delay_seconds(&self) -> f64 {
        match self {
            Self::OpenAICompatible(c) => c.max_retry_delay_seconds,
            Self::Anthropic(c) => c.max_retry_delay_seconds,
            Self::OpenAICodex(c) => c.max_retry_delay_seconds,
        }
    }
    /// Declared thinking levels.
    #[must_use]
    pub fn thinking_levels(&self) -> Option<&Vec<String>> {
        match self {
            Self::OpenAICompatible(c) => c.thinking_levels.as_ref(),
            Self::Anthropic(c) => c.thinking_levels.as_ref(),
            Self::OpenAICodex(c) => c.thinking_levels.as_ref(),
        }
    }
    /// Models that support thinking.
    #[must_use]
    pub fn thinking_models(&self) -> &[String] {
        match self {
            Self::OpenAICompatible(c) => &c.thinking_models,
            Self::Anthropic(c) => &c.thinking_models,
            Self::OpenAICodex(c) => &c.thinking_models,
        }
    }
    /// Default thinking level.
    #[must_use]
    pub fn thinking_default(&self) -> Option<&str> {
        match self {
            Self::OpenAICompatible(c) => c.thinking_default.as_deref(),
            Self::Anthropic(c) => c.thinking_default.as_deref(),
            Self::OpenAICodex(c) => c.thinking_default.as_deref(),
        }
    }
    /// Thinking-parameter wire key.
    #[must_use]
    pub fn thinking_parameter(&self) -> Option<&str> {
        match self {
            Self::OpenAICompatible(c) => c.thinking_parameter.as_deref(),
            Self::Anthropic(c) => c.thinking_parameter.as_deref(),
            Self::OpenAICodex(c) => c.thinking_parameter.as_deref(),
        }
    }
    /// Remembered per-model thinking preferences.
    #[must_use]
    pub fn thinking_defaults(&self) -> &IndexMap<String, String> {
        match self {
            Self::OpenAICompatible(c) => &c.thinking_defaults,
            Self::Anthropic(c) => &c.thinking_defaults,
            Self::OpenAICodex(c) => &c.thinking_defaults,
        }
    }
    /// Request-family label, if the provider carries one (`None` for Codex —
    /// tau's `getattr(provider, "api", None)`).
    #[must_use]
    pub fn api(&self) -> Option<&str> {
        match self {
            Self::OpenAICompatible(c) => Some(&c.api),
            Self::Anthropic(c) => Some(&c.api),
            Self::OpenAICodex(_) => None,
        }
    }
    /// Provider-level compatibility flags (empty for Codex).
    #[must_use]
    pub fn compat(&self) -> Option<&JsonMap> {
        match self {
            Self::OpenAICompatible(c) => Some(&c.compat),
            Self::Anthropic(c) => Some(&c.compat),
            Self::OpenAICodex(_) => None,
        }
    }
    /// Per-model metadata (empty for Codex).
    #[must_use]
    pub fn model_metadata(&self) -> Option<&IndexMap<String, ProviderModelMetadata>> {
        match self {
            Self::OpenAICompatible(c) => Some(&c.model_metadata),
            Self::Anthropic(c) => Some(&c.model_metadata),
            Self::OpenAICodex(_) => None,
        }
    }

    /// Serialize this provider config to JSON-compatible data (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        match self {
            Self::OpenAICompatible(c) => c.to_json(),
            Self::Anthropic(c) => c.to_json(),
            Self::OpenAICodex(c) => c.to_json(),
        }
    }

    fn set_default_model(&mut self, model: String) {
        match self {
            Self::OpenAICompatible(c) => c.default_model = model,
            Self::Anthropic(c) => c.default_model = model,
            Self::OpenAICodex(c) => c.default_model = model,
        }
    }

    fn set_thinking_defaults(&mut self, defaults: IndexMap<String, String>) {
        match self {
            Self::OpenAICompatible(c) => c.thinking_defaults = defaults,
            Self::Anthropic(c) => c.thinking_defaults = defaults,
            Self::OpenAICodex(c) => c.thinking_defaults = defaults,
        }
    }
}

/// A provider/model pair enabled for quick model cycling (tau `ScopedModelConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedModelConfig {
    /// Provider name.
    pub provider: String,
    /// Model name.
    pub model: String,
}

impl ScopedModelConfig {
    /// Serialize this scoped model reference (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut map = JsonMap::new();
        map.insert("provider".into(), JsonValue::String(self.provider.clone()));
        map.insert("model".into(), JsonValue::String(self.model.clone()));
        JsonValue::Object(map)
    }
}

/// rho provider settings loaded from rho home (tau `ProviderSettings`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSettings {
    /// Default provider name.
    pub default_provider: String,
    /// Configured providers.
    pub providers: Vec<ProviderConfig>,
    /// Scoped models for quick cycling.
    pub scoped_models: Vec<ScopedModelConfig>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            default_provider: DEFAULT_PROVIDER_NAME.to_string(),
            providers: builtin_provider_configs(),
            scoped_models: Vec::new(),
        }
    }
}

impl ProviderSettings {
    /// Return a configured provider by name (tau `get_provider`).
    pub fn get_provider(&self, name: Option<&str>) -> Result<&ProviderConfig, ProviderConfigError> {
        let target = or_default(name, &self.default_provider);
        self.providers
            .iter()
            .find(|provider| provider.name() == target)
            .ok_or_else(|| cfg_err(format!("Unknown provider: {target}")))
    }

    /// Serialize runtime preferences to JSON-compatible data (tau `to_json`).
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut preferences = JsonMap::new();
        for provider in &self.providers {
            preferences.insert(
                provider.name().to_string(),
                provider_preference_to_json(provider),
            );
        }
        let mut map = JsonMap::new();
        map.insert(
            "default_provider".into(),
            JsonValue::String(self.default_provider.clone()),
        );
        map.insert(
            "provider_preferences".into(),
            JsonValue::Object(preferences),
        );
        map.insert(
            "scoped_models".into(),
            JsonValue::Array(
                self.scoped_models
                    .iter()
                    .map(ScopedModelConfig::to_json)
                    .collect(),
            ),
        );
        JsonValue::Object(map)
    }
}

/// Resolved provider/model selection for a rho run (tau `ProviderSelection`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSelection {
    /// Resolved provider.
    pub provider: ProviderConfig,
    /// Resolved model.
    pub model: String,
}

// ---------------------------------------------------------------------------
// Builtin / catalog-derived configs
// ---------------------------------------------------------------------------

/// Return rho's built-in provider configs (tau `builtin_provider_configs`).
#[must_use]
pub fn builtin_provider_configs() -> Vec<ProviderConfig> {
    BUILTIN_PROVIDER_CATALOG
        .iter()
        .map(|entry| provider_config_from_entry(entry).expect("built-in provider must be valid"))
        .collect()
}

/// Create a durable provider config from a built-in catalog entry (tau
/// `provider_config_from_catalog_entry`).
pub fn provider_config_from_catalog_entry(
    name: &str,
) -> Result<ProviderConfig, ProviderConfigError> {
    for entry in BUILTIN_PROVIDER_CATALOG.iter() {
        if entry.name == name {
            return provider_config_from_entry(entry);
        }
    }
    Err(cfg_err(format!("Unknown built-in provider: {name}")))
}

/// Create a durable provider config from a catalog entry (tau
/// `provider_config_from_entry`).
pub fn provider_config_from_entry(
    entry: &ProviderCatalogEntry,
) -> Result<ProviderConfig, ProviderConfigError> {
    let context_windows = entry.context_windows.clone().unwrap_or_default();
    let model_metadata = provider_model_metadata_from_catalog(&entry.model_metadata);
    let config = if entry.kind == "anthropic" {
        let cfg = AnthropicProviderConfig {
            name: entry.name.clone(),
            base_url: entry.base_url.clone(),
            api: default_api_for_kind(&entry.kind),
            api_key_env: entry.api_key_env.clone(),
            credential_name: entry.credential_name.clone(),
            models: entry.models.clone(),
            default_model: entry.default_model.clone(),
            context_windows,
            headers: entry.headers.clone(),
            compat: entry.compat.clone(),
            model_metadata,
            thinking_levels: entry.thinking_levels.clone(),
            thinking_models: entry.thinking_models.clone(),
            thinking_default: entry.thinking_default.clone(),
            thinking_parameter: entry.thinking_parameter.clone(),
            thinking_defaults: IndexMap::new(),
            ..AnthropicProviderConfig::new()
        };
        cfg.validate()?;
        ProviderConfig::Anthropic(cfg)
    } else if entry.kind == "openai-codex" {
        let cfg = OpenAICodexProviderConfig {
            name: entry.name.clone(),
            base_url: entry.base_url.clone(),
            api_key_env: entry.api_key_env.clone(),
            credential_name: entry.credential_name.clone(),
            models: entry.models.clone(),
            default_model: entry.default_model.clone(),
            context_windows,
            thinking_levels: entry.thinking_levels.clone(),
            thinking_models: entry.thinking_models.clone(),
            thinking_default: entry.thinking_default.clone(),
            thinking_parameter: entry.thinking_parameter.clone(),
            thinking_defaults: IndexMap::new(),
            ..OpenAICodexProviderConfig::new()
        };
        cfg.validate()?;
        ProviderConfig::OpenAICodex(cfg)
    } else {
        let cfg = OpenAICompatibleProviderConfig {
            name: entry.name.clone(),
            base_url: entry.base_url.clone(),
            api: entry
                .api
                .clone()
                .unwrap_or_else(|| default_api_for_kind(&entry.kind)),
            api_key_env: entry.api_key_env.clone(),
            credential_name: entry.credential_name.clone(),
            models: entry.models.clone(),
            default_model: entry.default_model.clone(),
            context_windows,
            headers: entry.headers.clone(),
            compat: entry.compat.clone(),
            model_metadata,
            thinking_levels: entry.thinking_levels.clone(),
            thinking_models: entry.thinking_models.clone(),
            thinking_default: entry.thinking_default.clone(),
            thinking_parameter: entry.thinking_parameter.clone(),
            thinking_defaults: IndexMap::new(),
            ..OpenAICompatibleProviderConfig::new(entry.name.clone())
        };
        cfg.validate()?;
        ProviderConfig::OpenAICompatible(cfg)
    };
    Ok(config)
}

fn default_api_for_kind(kind: &str) -> ProviderApi {
    match kind {
        "anthropic" => "anthropic-messages",
        "openai-codex" => "openai-codex-responses",
        "google-generative-ai" => "google-generative-ai",
        "mistral-conversations" => "mistral-conversations",
        _ => "openai-completions",
    }
    .to_string()
}

fn provider_model_metadata_from_catalog(
    model_metadata: &IndexMap<String, ModelCatalogMetadata>,
) -> IndexMap<String, ProviderModelMetadata> {
    model_metadata
        .iter()
        .map(|(model, metadata)| {
            (
                model.clone(),
                ProviderModelMetadata {
                    name: metadata.name.clone(),
                    api: metadata.api.clone(),
                    base_url: metadata.base_url.clone(),
                    reasoning: metadata.reasoning,
                    input: metadata.input.clone(),
                    cost: metadata.cost.clone().unwrap_or_default(),
                    cost_tiers: metadata.cost_tiers.clone(),
                    context_window: metadata.context_window,
                    max_tokens: metadata.max_tokens,
                    headers: metadata.headers.clone(),
                    compat: metadata.compat.clone(),
                    thinking_level_map: metadata.thinking_level_map.clone(),
                },
            )
        })
        .collect()
}

/// Return rho's default OpenAI-compatible provider entry (tau
/// `default_openai_provider_config`).
pub fn default_openai_provider_config()
-> Result<OpenAICompatibleProviderConfig, ProviderConfigError> {
    match provider_config_from_catalog_entry(DEFAULT_PROVIDER_NAME)? {
        ProviderConfig::OpenAICompatible(config) => Ok(config),
        _ => Err(cfg_err("default OpenAI provider must be OpenAI-compatible")),
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Return the durable provider settings path (tau `provider_settings_path`).
#[must_use]
pub fn provider_settings_path(paths: Option<&RhoPaths>) -> PathBuf {
    let default = RhoPaths::default();
    let paths = paths.unwrap_or(&default);
    paths.home.join("providers.json")
}

/// Load durable provider settings, falling back to env-compatible defaults (tau
/// `load_provider_settings`).
pub fn load_provider_settings(
    paths: Option<&RhoPaths>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let path = provider_settings_path(paths);
    if !path.exists() {
        return Ok(ProviderSettings {
            default_provider: DEFAULT_PROVIDER_NAME.to_string(),
            providers: effective_provider_configs(paths)?,
            scoped_models: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| cfg_err(format!("{}: {error}", path.display())))?;
    let raw: JsonValue = serde_json::from_str(&text)
        .map_err(|error| cfg_err(format!("{}: {error}", path.display())))?;
    if !raw.is_object() {
        return Err(cfg_err("Provider settings must be a JSON object"));
    }
    let settings = provider_settings_from_json(&raw, paths)?;
    with_builtin_catalog_models(settings, paths, credentials)
}

/// Write durable provider preferences and return the path (tau
/// `save_provider_settings`).
pub fn save_provider_settings(
    settings: &ProviderSettings,
    paths: Option<&RhoPaths>,
) -> Result<PathBuf, ProviderConfigError> {
    save_provider_definitions_to_catalog(settings, paths)?;
    let path = provider_settings_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| cfg_err(format!("{}: {error}", path.display())))?;
    }
    if path.exists() {
        let backup = path.with_extension(match path.extension() {
            Some(ext) => format!("{}.bak", ext.to_string_lossy()),
            None => "bak".to_string(),
        });
        let _ = std::fs::copy(&path, &backup);
    }
    let text = format!("{}\n", to_sorted_pretty_json(&settings.to_json()));
    atomic_write_text(&path, &text)
        .map_err(|error| cfg_err(format!("{}: {error}", path.display())))?;
    Ok(path)
}

/// Reload settings, persist one default provider/model change, and return them
/// (tau `save_default_provider_model`).
pub fn save_default_provider_model(
    provider_name: &str,
    model: &str,
    paths: Option<&RhoPaths>,
    fallback_settings: Option<&ProviderSettings>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let settings = load_provider_settings_for_write(paths, fallback_settings, credentials)?;
    let updated = set_default_provider_model(&settings, provider_name, model)?;
    save_provider_settings(&updated, paths)?;
    Ok(updated)
}

/// Reload settings, persist one provider/model thinking preference, and return
/// them (tau `save_provider_thinking_level`).
pub fn save_provider_thinking_level(
    provider_name: &str,
    model: &str,
    thinking_level: &str,
    paths: Option<&RhoPaths>,
    fallback_settings: Option<&ProviderSettings>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let settings = load_provider_settings_for_write(paths, fallback_settings, credentials)?;
    let updated = set_provider_thinking_level(&settings, provider_name, model, thinking_level)?;
    save_provider_settings(&updated, paths)?;
    Ok(updated)
}

/// Reload settings, toggle one scoped model, persist them, and return them (tau
/// `toggle_saved_scoped_model`).
pub fn toggle_saved_scoped_model(
    provider_name: &str,
    model: &str,
    paths: Option<&RhoPaths>,
    fallback_settings: Option<&ProviderSettings>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let mut settings = load_provider_settings_for_write(paths, fallback_settings, credentials)?;
    let provider = settings.get_provider(Some(provider_name))?;
    if !provider.models().iter().any(|m| m == model) {
        return Err(cfg_err(format!(
            "Model is not configured: {provider_name}:{model}"
        )));
    }
    let target = ScopedModelConfig {
        provider: provider_name.to_string(),
        model: model.to_string(),
    };
    if settings.scoped_models.contains(&target) {
        settings.scoped_models.retain(|item| item != &target);
    } else {
        settings.scoped_models.push(target);
    }
    save_provider_settings(&settings, paths)?;
    Ok(settings)
}

/// Reload settings, upsert one provider entry, persist them, and return them
/// (tau `upsert_saved_provider`).
pub fn upsert_saved_provider(
    provider: ProviderConfig,
    set_default: bool,
    paths: Option<&RhoPaths>,
    fallback_settings: Option<&ProviderSettings>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let settings = load_provider_settings_for_write(paths, fallback_settings, credentials)?;
    let updated = upsert_provider(&settings, provider, set_default)?;
    save_provider_settings(&updated, paths)?;
    Ok(updated)
}

fn load_provider_settings_for_write(
    paths: Option<&RhoPaths>,
    fallback_settings: Option<&ProviderSettings>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    if provider_settings_path(paths).exists() {
        return load_provider_settings(paths, credentials);
    }
    if let Some(fallback) = fallback_settings {
        return Ok(fallback.clone());
    }
    load_provider_settings(paths, credentials)
}

/// Return settings with the default provider/model preference updated (tau
/// `set_default_provider_model`).
pub fn set_default_provider_model(
    settings: &ProviderSettings,
    provider_name: &str,
    model: &str,
) -> Result<ProviderSettings, ProviderConfigError> {
    let provider = settings.get_provider(Some(provider_name))?;
    validate_provider_model(provider, model)?;
    let mut updated_provider = provider.clone();
    updated_provider.set_default_model(model.to_string());
    let providers = settings
        .providers
        .iter()
        .map(|item| {
            if item.name() == provider_name {
                updated_provider.clone()
            } else {
                item.clone()
            }
        })
        .collect();
    Ok(ProviderSettings {
        default_provider: provider_name.to_string(),
        providers,
        scoped_models: settings.scoped_models.clone(),
    })
}

/// Return settings with a remembered thinking level for one provider/model (tau
/// `set_provider_thinking_level`).
pub fn set_provider_thinking_level(
    settings: &ProviderSettings,
    provider_name: &str,
    model: &str,
    thinking_level: &str,
) -> Result<ProviderSettings, ProviderConfigError> {
    let provider = settings.get_provider(Some(provider_name))?;
    validate_provider_model(provider, model)?;
    let normalized = normalize_thinking_level(Some(thinking_level)).map_err(cfg_err)?;
    let available = provider_thinking_levels(provider, Some(model));
    if !available.contains(&normalized) {
        let modes = if available.is_empty() {
            "none".to_string()
        } else {
            available.join(", ")
        };
        return Err(cfg_err(format!(
            "Thinking mode {normalized} is not available for {provider_name}:{model}. Available modes: {modes}"
        )));
    }
    let mut defaults = provider.thinking_defaults().clone();
    defaults.insert(model.to_string(), normalized);
    let mut updated_provider = provider.clone();
    updated_provider.set_thinking_defaults(defaults);
    let providers = settings
        .providers
        .iter()
        .map(|item| {
            if item.name() == provider_name {
                updated_provider.clone()
            } else {
                item.clone()
            }
        })
        .collect();
    Ok(ProviderSettings {
        default_provider: settings.default_provider.clone(),
        providers,
        scoped_models: settings.scoped_models.clone(),
    })
}

/// Return settings with an OpenAI-compatible provider added or replaced (tau
/// `upsert_openai_compatible_provider`).
pub fn upsert_openai_compatible_provider(
    settings: &ProviderSettings,
    provider: OpenAICompatibleProviderConfig,
    set_default: bool,
) -> Result<ProviderSettings, ProviderConfigError> {
    upsert_provider(
        settings,
        ProviderConfig::OpenAICompatible(provider),
        set_default,
    )
}

/// Return settings with a provider added or replaced (tau `upsert_provider`).
pub fn upsert_provider(
    settings: &ProviderSettings,
    provider: ProviderConfig,
    set_default: bool,
) -> Result<ProviderSettings, ProviderConfigError> {
    let mut providers_by_name: IndexMap<String, ProviderConfig> = IndexMap::new();
    for item in &settings.providers {
        providers_by_name.insert(item.name().to_string(), item.clone());
    }
    let builtin_names: Vec<&str> = BUILTIN_PROVIDER_CATALOG
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    let name = provider.name().to_string();
    let mut provider = provider;
    if providers_by_name.contains_key(&name) && builtin_names.contains(&name.as_str()) {
        provider = merge_provider_config(&providers_by_name[&name], provider);
    }
    providers_by_name.insert(name.clone(), provider);
    let default_provider = if set_default {
        name
    } else {
        settings.default_provider.clone()
    };
    let mut sorted_names: Vec<String> = providers_by_name.keys().cloned().collect();
    sorted_names.sort();
    let providers: Vec<ProviderConfig> = sorted_names
        .iter()
        .map(|n| providers_by_name[n].clone())
        .collect();
    let updated = ProviderSettings {
        default_provider: default_provider.clone(),
        providers,
        scoped_models: settings.scoped_models.clone(),
    };
    updated.get_provider(Some(&default_provider))?;
    Ok(updated)
}

fn with_builtin_catalog_models(
    settings: ProviderSettings,
    paths: Option<&RhoPaths>,
    credentials: Option<&dyn CredentialReader>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let mut catalog_configs: IndexMap<String, ProviderConfig> = IndexMap::new();
    for config in effective_provider_configs(paths)? {
        catalog_configs.insert(config.name().to_string(), config);
    }
    let mut providers: Vec<ProviderConfig> = settings
        .providers
        .iter()
        .map(|provider| {
            catalog_configs.get(provider.name()).map_or_else(
                || provider.clone(),
                |catalog| merge_provider_config(provider, catalog.clone()),
            )
        })
        .collect();
    append_catalog_providers(&mut providers, &catalog_configs, paths, credentials);
    let names: Vec<&str> = providers.iter().map(ProviderConfig::name).collect();
    let default_provider = if names.contains(&settings.default_provider.as_str()) {
        settings.default_provider
    } else if let Some(first) = providers.first() {
        first.name().to_string()
    } else {
        DEFAULT_PROVIDER_NAME.to_string()
    };
    Ok(ProviderSettings {
        default_provider,
        providers,
        scoped_models: settings.scoped_models,
    })
}

fn effective_provider_configs(
    paths: Option<&RhoPaths>,
) -> Result<Vec<ProviderConfig>, ProviderConfigError> {
    effective_catalog(paths)?
        .iter()
        .map(provider_config_from_entry)
        .collect()
}

fn append_catalog_providers(
    providers: &mut Vec<ProviderConfig>,
    catalog_configs: &IndexMap<String, ProviderConfig>,
    _paths: Option<&RhoPaths>,
    credentials: Option<&dyn CredentialReader>,
) {
    let builtin_names: Vec<&str> = BUILTIN_PROVIDER_CATALOG
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    let mut provider_names: Vec<String> = providers.iter().map(|p| p.name().to_string()).collect();
    for provider in catalog_configs.values() {
        let name = provider.name();
        if provider_names.iter().any(|existing| existing == name) {
            continue;
        }
        if !builtin_names.contains(&name) || provider_has_usable_credentials(provider, credentials)
        {
            providers.push(provider.clone());
            provider_names.push(name.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Merge helpers
// ---------------------------------------------------------------------------

fn merge_provider_config(existing: &ProviderConfig, incoming: ProviderConfig) -> ProviderConfig {
    match (existing, &incoming) {
        (ProviderConfig::OpenAICodex(existing), ProviderConfig::OpenAICodex(incoming)) => {
            ProviderConfig::OpenAICodex(merge_codex_provider(existing, incoming))
        }
        (
            ProviderConfig::OpenAICompatible(existing),
            ProviderConfig::OpenAICompatible(incoming),
        ) => ProviderConfig::OpenAICompatible(merge_openai_compatible_provider(existing, incoming)),
        (ProviderConfig::Anthropic(existing), ProviderConfig::Anthropic(incoming)) => {
            ProviderConfig::Anthropic(merge_anthropic_provider(existing, incoming))
        }
        _ => incoming,
    }
}

fn merge_codex_provider(
    existing: &OpenAICodexProviderConfig,
    incoming: &OpenAICodexProviderConfig,
) -> OpenAICodexProviderConfig {
    let existing_has_thinking = existing.thinking_levels.is_some();
    OpenAICodexProviderConfig {
        default_model: if incoming.models.contains(&existing.default_model) {
            existing.default_model.clone()
        } else {
            incoming.default_model.clone()
        },
        headers: merge_string_maps(&incoming.headers, &existing.headers),
        timeout_seconds: existing.timeout_seconds,
        max_retries: existing.max_retries,
        max_retry_delay_seconds: existing.max_retry_delay_seconds,
        context_windows: merge_int_maps(&incoming.context_windows, &existing.context_windows),
        thinking_levels: if existing_has_thinking {
            existing.thinking_levels.clone()
        } else {
            incoming.thinking_levels.clone()
        },
        thinking_models: if existing_has_thinking {
            existing.thinking_models.clone()
        } else {
            incoming.thinking_models.clone()
        },
        thinking_default: if existing_has_thinking {
            existing.thinking_default.clone()
        } else {
            incoming.thinking_default.clone()
        },
        thinking_parameter: if existing_has_thinking {
            existing.thinking_parameter.clone()
        } else {
            incoming.thinking_parameter.clone()
        },
        thinking_defaults: existing.thinking_defaults.clone(),
        ..incoming.clone()
    }
}

fn merge_openai_compatible_provider(
    existing: &OpenAICompatibleProviderConfig,
    incoming: &OpenAICompatibleProviderConfig,
) -> OpenAICompatibleProviderConfig {
    let models = unique_strings(incoming.models.iter().chain(existing.models.iter()));
    let existing_has_thinking = existing.thinking_levels.is_some();
    OpenAICompatibleProviderConfig {
        default_model: if models.contains(&existing.default_model) {
            existing.default_model.clone()
        } else {
            incoming.default_model.clone()
        },
        models,
        headers: merge_string_maps(&incoming.headers, &existing.headers),
        compat: merge_json_maps(&incoming.compat, &existing.compat),
        model_metadata: merge_provider_model_metadata(
            &incoming.model_metadata,
            &existing.model_metadata,
        ),
        timeout_seconds: existing.timeout_seconds,
        max_retries: existing.max_retries,
        max_retry_delay_seconds: existing.max_retry_delay_seconds,
        context_windows: merge_int_maps(&incoming.context_windows, &existing.context_windows),
        thinking_levels: if existing_has_thinking {
            existing.thinking_levels.clone()
        } else {
            incoming.thinking_levels.clone()
        },
        thinking_models: if existing_has_thinking {
            existing.thinking_models.clone()
        } else {
            incoming.thinking_models.clone()
        },
        thinking_default: if existing_has_thinking {
            existing.thinking_default.clone()
        } else {
            incoming.thinking_default.clone()
        },
        thinking_parameter: if existing_has_thinking {
            existing.thinking_parameter.clone()
        } else {
            incoming.thinking_parameter.clone()
        },
        thinking_defaults: existing.thinking_defaults.clone(),
        ..incoming.clone()
    }
}

fn merge_anthropic_provider(
    existing: &AnthropicProviderConfig,
    incoming: &AnthropicProviderConfig,
) -> AnthropicProviderConfig {
    let models = unique_strings(incoming.models.iter().chain(existing.models.iter()));
    let existing_has_thinking = existing.thinking_levels.is_some();
    AnthropicProviderConfig {
        default_model: if models.contains(&existing.default_model) {
            existing.default_model.clone()
        } else {
            incoming.default_model.clone()
        },
        models,
        headers: merge_string_maps(&incoming.headers, &existing.headers),
        compat: merge_json_maps(&incoming.compat, &existing.compat),
        model_metadata: merge_provider_model_metadata(
            &incoming.model_metadata,
            &existing.model_metadata,
        ),
        timeout_seconds: existing.timeout_seconds,
        max_retries: existing.max_retries,
        max_retry_delay_seconds: existing.max_retry_delay_seconds,
        context_windows: merge_int_maps(&incoming.context_windows, &existing.context_windows),
        thinking_levels: if existing_has_thinking {
            existing.thinking_levels.clone()
        } else {
            incoming.thinking_levels.clone()
        },
        thinking_models: if existing_has_thinking {
            existing.thinking_models.clone()
        } else {
            incoming.thinking_models.clone()
        },
        thinking_default: if existing_has_thinking {
            existing.thinking_default.clone()
        } else {
            incoming.thinking_default.clone()
        },
        thinking_parameter: if existing_has_thinking {
            existing.thinking_parameter.clone()
        } else {
            incoming.thinking_parameter.clone()
        },
        thinking_defaults: existing.thinking_defaults.clone(),
        ..incoming.clone()
    }
}

fn merge_provider_model_metadata(
    incoming: &IndexMap<String, ProviderModelMetadata>,
    existing: &IndexMap<String, ProviderModelMetadata>,
) -> IndexMap<String, ProviderModelMetadata> {
    let mut merged = incoming.clone();
    for (model, metadata) in existing {
        match merged.get(model).cloned() {
            None => {
                merged.insert(model.clone(), metadata.clone());
            }
            Some(base) => {
                merged.insert(
                    model.clone(),
                    ProviderModelMetadata {
                        name: metadata.name.clone().or(base.name.clone()),
                        api: metadata.api.clone().or(base.api.clone()),
                        base_url: metadata.base_url.clone().or(base.base_url.clone()),
                        reasoning: metadata.reasoning.or(base.reasoning),
                        input: if metadata.input.is_empty() {
                            base.input.clone()
                        } else {
                            metadata.input.clone()
                        },
                        cost: merge_float_maps(&base.cost, &metadata.cost),
                        cost_tiers: if metadata.cost_tiers.is_empty() {
                            base.cost_tiers.clone()
                        } else {
                            metadata.cost_tiers.clone()
                        },
                        context_window: metadata.context_window.or(base.context_window),
                        max_tokens: metadata.max_tokens.or(base.max_tokens),
                        headers: merge_string_maps(&base.headers, &metadata.headers),
                        compat: merge_json_maps(&base.compat, &metadata.compat),
                        thinking_level_map: merge_opt_string_maps(
                            &base.thinking_level_map,
                            &metadata.thinking_level_map,
                        ),
                    },
                );
            }
        }
    }
    merged
}

fn unique_strings<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in values {
        if !seen.iter().any(|s| s == value) {
            seen.push(value.clone());
        }
    }
    seen
}

fn merge_string_maps(
    base: &IndexMap<String, String>,
    overlay: &IndexMap<String, String>,
) -> IndexMap<String, String> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn merge_int_maps(
    base: &IndexMap<String, i64>,
    overlay: &IndexMap<String, i64>,
) -> IndexMap<String, i64> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), *value);
    }
    merged
}

fn merge_float_maps(
    base: &IndexMap<String, f64>,
    overlay: &IndexMap<String, f64>,
) -> IndexMap<String, f64> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), *value);
    }
    merged
}

fn merge_opt_string_maps(
    base: &IndexMap<String, Option<String>>,
    overlay: &IndexMap<String, Option<String>>,
) -> IndexMap<String, Option<String>> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn merge_json_maps(base: &JsonMap, overlay: &JsonMap) -> JsonMap {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn atomic_write_text(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().map_or_else(
        || "providers.json".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .tempfile_in(parent)?;
    use std::io::Write as _;
    temp.write_all(text.as_bytes())?;
    temp.flush()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn provider_preference_to_json(provider: &ProviderConfig) -> JsonValue {
    let mut map = JsonMap::new();
    map.insert(
        "default_model".into(),
        JsonValue::String(provider.default_model().to_string()),
    );
    map.insert("headers".into(), string_map_json(provider.headers()));
    map.insert(
        "timeout_seconds".into(),
        float_json(provider.timeout_seconds()),
    );
    map.insert(
        "max_retries".into(),
        JsonValue::from(provider.max_retries()),
    );
    map.insert(
        "max_retry_delay_seconds".into(),
        float_json(provider.max_retry_delay_seconds()),
    );
    map.insert(
        "thinking_defaults".into(),
        string_map_json(provider.thinking_defaults()),
    );
    JsonValue::Object(map)
}

fn save_provider_definitions_to_catalog(
    settings: &ProviderSettings,
    paths: Option<&RhoPaths>,
) -> Result<(), ProviderConfigError> {
    let catalog = effective_catalog(paths)?;
    let mut catalog_by_name: IndexMap<String, &ProviderCatalogEntry> = IndexMap::new();
    for entry in &catalog {
        catalog_by_name.insert(entry.name.clone(), entry);
    }
    let mut entries_to_save: Vec<ProviderCatalogEntry> = Vec::new();
    for provider in &settings.providers {
        let entry = catalog_by_name.get(provider.name()).copied();
        if entry.is_none() || provider_definition_differs_from_catalog(provider, entry.unwrap()) {
            entries_to_save.push(catalog_entry_from_provider(provider, entry));
        }
    }
    if !entries_to_save.is_empty() {
        save_user_catalog_entries(&entries_to_save, paths)?;
    }
    Ok(())
}

fn provider_definition_differs_from_catalog(
    provider: &ProviderConfig,
    entry: &ProviderCatalogEntry,
) -> bool {
    if provider_kind(provider) != entry.kind {
        return true;
    }
    if provider.base_url() != entry.base_url {
        return true;
    }
    if provider.api_key_env() != entry.api_key_env {
        return true;
    }
    if provider.credential_name() != entry.credential_name.as_deref() {
        return true;
    }
    if provider.models() != entry.models.as_slice() {
        return true;
    }
    if provider.api() != entry.api.as_deref() && entry.api.is_some() {
        return true;
    }
    if provider.context_windows() != &entry.context_windows.clone().unwrap_or_default() {
        return true;
    }
    if provider.headers() != &entry.headers {
        return true;
    }
    let compat = provider.compat().cloned().unwrap_or_default();
    if compat != entry.compat {
        return true;
    }
    if catalog_model_metadata_from_provider(provider) != entry.model_metadata {
        return true;
    }
    if provider.thinking_levels() != entry.thinking_levels.as_ref() {
        return true;
    }
    if provider.thinking_models() != entry.thinking_models.as_slice() {
        return true;
    }
    if provider.thinking_default() != entry.thinking_default.as_deref() {
        return true;
    }
    provider.thinking_parameter() != entry.thinking_parameter.as_deref()
}

fn catalog_entry_from_provider(
    provider: &ProviderConfig,
    existing: Option<&ProviderCatalogEntry>,
) -> ProviderCatalogEntry {
    let context_windows = provider.context_windows().clone();
    let default_model = match existing {
        Some(entry)
            if entry.models.contains(&entry.default_model)
                && provider.models().contains(&entry.default_model) =>
        {
            entry.default_model.clone()
        }
        _ => provider.default_model().to_string(),
    };
    ProviderCatalogEntry {
        name: provider.name().to_string(),
        display_name: existing
            .map_or_else(|| provider.name().to_string(), |e| e.display_name.clone()),
        kind: provider_kind(provider),
        base_url: provider.base_url().to_string(),
        api_key_env: provider.api_key_env().to_string(),
        api: provider.api().map(str::to_string),
        credential_name: provider.credential_name().map(str::to_string),
        models: provider.models().to_vec(),
        default_model,
        docs_url: existing.map_or_else(|| provider.base_url().to_string(), |e| e.docs_url.clone()),
        context_windows: if context_windows.is_empty() {
            None
        } else {
            Some(context_windows)
        },
        headers: provider.headers().clone(),
        compat: provider.compat().cloned().unwrap_or_default(),
        model_metadata: catalog_model_metadata_from_provider(provider),
        thinking_levels: provider.thinking_levels().cloned(),
        thinking_models: provider.thinking_models().to_vec(),
        thinking_default: provider.thinking_default().map(str::to_string),
        thinking_parameter: provider.thinking_parameter().map(str::to_string),
        auth_methods: vec!["api_key".to_string()],
    }
}

fn catalog_model_metadata_from_provider(
    provider: &ProviderConfig,
) -> IndexMap<String, ModelCatalogMetadata> {
    let Some(metadata_by_model) = provider.model_metadata() else {
        return IndexMap::new();
    };
    metadata_by_model
        .iter()
        .map(|(model, metadata)| {
            let input: Vec<String> = metadata
                .input
                .iter()
                .filter(|item| *item == "text" || *item == "image")
                .cloned()
                .collect();
            (
                model.clone(),
                ModelCatalogMetadata {
                    name: metadata.name.clone(),
                    api: metadata.api.clone(),
                    base_url: metadata.base_url.clone(),
                    reasoning: metadata.reasoning,
                    input,
                    cost: if metadata.cost.is_empty() {
                        None
                    } else {
                        Some(metadata.cost.clone())
                    },
                    cost_tiers: metadata.cost_tiers.clone(),
                    context_window: metadata.context_window,
                    max_tokens: metadata.max_tokens,
                    headers: metadata.headers.clone(),
                    compat: metadata.compat.clone(),
                    thinking_level_map: metadata.thinking_level_map.clone(),
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// JSON parsing (providers.json)
// ---------------------------------------------------------------------------

/// Parse provider preferences from JSON-compatible data (tau
/// `provider_settings_from_json`).
pub fn provider_settings_from_json(
    data: &JsonValue,
    paths: Option<&RhoPaths>,
) -> Result<ProviderSettings, ProviderConfigError> {
    let object = data
        .as_object()
        .ok_or_else(|| cfg_err("Provider settings must be a JSON object"))?;
    let default_provider = parse_string(object.get("default_provider"), "default_provider")?;
    let scoped_models = scoped_models_from_json(object.get("scoped_models"))?;
    if object.contains_key("provider_preferences") {
        let providers = providers_with_preferences(object.get("provider_preferences"), paths)?;
        return Ok(ProviderSettings {
            default_provider,
            providers,
            scoped_models,
        });
    }
    let providers_data = object.get("providers");
    let list = providers_data
        .and_then(JsonValue::as_array)
        .filter(|list| !list.is_empty())
        .ok_or_else(|| {
            cfg_err("Provider settings must include provider_preferences or legacy providers")
        })?;
    let mut providers = Vec::with_capacity(list.len());
    for item in list {
        providers.push(provider_from_json(item)?);
    }
    let mut names: Vec<&str> = providers.iter().map(ProviderConfig::name).collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    if names.len() != count {
        return Err(cfg_err("Provider names must be unique"));
    }
    Ok(ProviderSettings {
        default_provider,
        providers,
        scoped_models,
    })
}

fn providers_with_preferences(
    value: Option<&JsonValue>,
    paths: Option<&RhoPaths>,
) -> Result<Vec<ProviderConfig>, ProviderConfigError> {
    let object = value.and_then(JsonValue::as_object).ok_or_else(|| {
        cfg_err("Provider settings field must be an object: provider_preferences")
    })?;
    let mut catalog_configs: IndexMap<String, ProviderConfig> = IndexMap::new();
    for provider in effective_provider_configs(paths)? {
        catalog_configs.insert(provider.name().to_string(), provider);
    }
    let mut providers = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (name, preference_data) in object {
        if name.trim().is_empty() {
            return Err(cfg_err(
                "Provider preference names must be non-empty strings",
            ));
        }
        let provider_name = name.trim().to_string();
        if seen.contains(&provider_name) {
            return Err(cfg_err("Provider preference names must be unique"));
        }
        let Some(catalog) = catalog_configs.get(&provider_name) else {
            continue;
        };
        providers.push(apply_provider_preference(catalog, preference_data)?);
        seen.push(provider_name);
    }
    Ok(providers)
}

fn apply_provider_preference(
    provider: &ProviderConfig,
    value: &JsonValue,
) -> Result<ProviderConfig, ProviderConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| cfg_err("Provider preference entries must be objects"))?;
    let allowed = [
        "default_model",
        "headers",
        "timeout_seconds",
        "max_retries",
        "max_retry_delay_seconds",
        "thinking_defaults",
    ];
    let mut unknown: Vec<String> = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(cfg_err(format!(
            "Unknown provider preference fields for {}: {}",
            provider.name(),
            unknown.join(", ")
        )));
    }
    let name = provider.name();
    let default_model = if object.contains_key("default_model") {
        parse_string(
            object.get("default_model"),
            &format!("provider_preferences.{name}.default_model"),
        )?
    } else {
        provider.default_model().to_string()
    };
    let models = if provider.models().contains(&default_model) {
        provider.models().to_vec()
    } else {
        let mut models = provider.models().to_vec();
        models.push(default_model.clone());
        models
    };
    let headers = if object.contains_key("headers") {
        parse_string_dict(
            object.get("headers"),
            &format!("provider_preferences.{name}.headers"),
        )?
    } else {
        provider.headers().clone()
    };
    let timeout_seconds = if object.contains_key("timeout_seconds") {
        parse_positive_float(
            object.get("timeout_seconds"),
            &format!("provider_preferences.{name}.timeout_seconds"),
        )?
    } else {
        provider.timeout_seconds()
    };
    let max_retries = if object.contains_key("max_retries") {
        parse_non_negative_int(
            object.get("max_retries"),
            &format!("provider_preferences.{name}.max_retries"),
        )?
    } else {
        provider.max_retries()
    };
    let max_retry_delay_seconds = if object.contains_key("max_retry_delay_seconds") {
        parse_non_negative_float(
            object.get("max_retry_delay_seconds"),
            &format!("provider_preferences.{name}.max_retry_delay_seconds"),
        )?
    } else {
        provider.max_retry_delay_seconds()
    };
    let thinking_defaults = if object.contains_key("thinking_defaults") {
        thinking_defaults_dict(
            object.get("thinking_defaults"),
            provider,
            &format!("provider_preferences.{name}.thinking_defaults"),
        )?
    } else {
        provider.thinking_defaults().clone()
    };

    let mut updated = provider.clone();
    match &mut updated {
        ProviderConfig::OpenAICompatible(c) => {
            c.models = models;
            c.default_model = default_model;
            c.headers = headers;
            c.timeout_seconds = timeout_seconds;
            c.max_retries = max_retries;
            c.max_retry_delay_seconds = max_retry_delay_seconds;
            c.thinking_defaults = thinking_defaults;
            c.validate()?;
        }
        ProviderConfig::Anthropic(c) => {
            c.models = models;
            c.default_model = default_model;
            c.headers = headers;
            c.timeout_seconds = timeout_seconds;
            c.max_retries = max_retries;
            c.max_retry_delay_seconds = max_retry_delay_seconds;
            c.thinking_defaults = thinking_defaults;
            c.validate()?;
        }
        ProviderConfig::OpenAICodex(c) => {
            c.models = models;
            c.default_model = default_model;
            c.headers = headers;
            c.timeout_seconds = timeout_seconds;
            c.max_retries = max_retries;
            c.max_retry_delay_seconds = max_retry_delay_seconds;
            c.thinking_defaults = thinking_defaults;
            c.validate()?;
        }
    }
    Ok(updated)
}

fn thinking_defaults_dict(
    value: Option<&JsonValue>,
    provider: &ProviderConfig,
    field_name: &str,
) -> Result<IndexMap<String, String>, ProviderConfigError> {
    let raw = raw_thinking_defaults_dict(value, field_name)?;
    for (model, thinking_level) in &raw {
        validate_provider_model(provider, model)?;
        let available = provider_thinking_levels(provider, Some(model));
        if !available.contains(thinking_level) {
            let modes = if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            };
            return Err(cfg_err(format!(
                "Provider thinking default {thinking_level} is not available for {}:{model}. Available modes: {modes}",
                provider.name()
            )));
        }
    }
    Ok(raw)
}

fn raw_thinking_defaults_dict(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<IndexMap<String, String>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be a thinking mode object: {field_name}"
        ))
    })?;
    let mut defaults = IndexMap::new();
    for (key, item) in object {
        let model = parse_string_value(key, field_name)?;
        let thinking_level = optional_thinking_level(item, &format!("{field_name}.{model}"))?;
        let Some(thinking_level) = thinking_level else {
            return Err(cfg_err(format!(
                "Provider field must be a thinking mode: {field_name}"
            )));
        };
        defaults.insert(model, thinking_level);
    }
    Ok(defaults)
}

fn scoped_models_from_json(
    value: Option<&JsonValue>,
) -> Result<Vec<ScopedModelConfig>, ProviderConfigError> {
    let value = match value {
        None | Some(JsonValue::Null) => return Ok(Vec::new()),
        Some(value) => value,
    };
    let list = value
        .as_array()
        .ok_or_else(|| cfg_err("Provider settings field must be a list: scoped_models"))?;
    let mut scoped = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for item in list {
        let object = item
            .as_object()
            .ok_or_else(|| cfg_err("Provider scoped_models entries must be objects"))?;
        let provider = parse_string(object.get("provider"), "scoped_models.provider")?;
        let model = parse_string(object.get("model"), "scoped_models.model")?;
        let key = (provider.clone(), model.clone());
        if !seen.contains(&key) {
            scoped.push(ScopedModelConfig { provider, model });
            seen.push(key);
        }
    }
    Ok(scoped)
}

fn provider_from_json(data: &JsonValue) -> Result<ProviderConfig, ProviderConfigError> {
    let object = data
        .as_object()
        .ok_or_else(|| cfg_err("Provider entries must be JSON objects"))?;
    let provider_type = parse_string(object.get("type"), "providers[].type")?;
    if !matches!(
        provider_type.as_str(),
        "openai-compatible"
            | "anthropic"
            | "openai-codex"
            | "google-generative-ai"
            | "mistral-conversations"
    ) {
        return Err(cfg_err(format!(
            "Unsupported provider type: {provider_type}"
        )));
    }
    let name = parse_string(object.get("name"), "providers[].name")?;
    let base_url = parse_string(
        object.get("base_url"),
        &format!("providers[{name}].base_url"),
    )?
    .trim_end_matches('/')
    .to_string();
    let api = optional_provider_api(object.get("api"), &format!("providers[{name}].api"))?;
    let api_key_env = parse_string(
        object.get("api_key_env"),
        &format!("providers[{name}].api_key_env"),
    )?;
    let credential_name = optional_string(
        object.get("credential_name"),
        &format!("providers[{name}].credential_name"),
    )?;
    let models = parse_string_tuple(object.get("models"), &format!("providers[{name}].models"))?;
    let default_model = parse_string(
        object.get("default_model"),
        &format!("providers[{name}].default_model"),
    )?;
    let context_windows = parse_context_window_dict(
        object.get("context_windows"),
        &format!("providers[{name}].context_windows"),
    )?;
    let headers = parse_string_dict(object.get("headers"), &format!("providers[{name}].headers"))?;
    let compat = parse_json_dict(object.get("compat"), &format!("providers[{name}].compat"))?;
    let model_metadata = model_metadata_dict(
        object.get("model_metadata"),
        &models,
        &format!("providers[{name}].model_metadata"),
    )?;
    let timeout_seconds = match object.get("timeout_seconds") {
        None => DEFAULT_OPENAI_COMPATIBLE_TIMEOUT_SECONDS,
        some => parse_positive_float(some, &format!("providers[{name}].timeout_seconds"))?,
    };
    let max_retries = match object.get("max_retries") {
        None => MAX_RETRIES_DEFAULT,
        some => parse_non_negative_int(some, &format!("providers[{name}].max_retries"))?,
    };
    let max_retry_delay_seconds = match object.get("max_retry_delay_seconds") {
        None => DEFAULT_OPENAI_COMPATIBLE_MAX_RETRY_DELAY_SECONDS,
        some => {
            parse_non_negative_float(some, &format!("providers[{name}].max_retry_delay_seconds"))?
        }
    };
    let thinking_levels = optional_thinking_levels(
        object.get("thinking_levels"),
        &format!("providers[{name}].thinking_levels"),
    )?;
    let thinking_models = optional_string_tuple(
        object.get("thinking_models"),
        &format!("providers[{name}].thinking_models"),
    )?;
    let thinking_default = optional_thinking_level_opt(
        object.get("thinking_default"),
        &format!("providers[{name}].thinking_default"),
    )?;
    let thinking_parameter = optional_thinking_parameter(
        object.get("thinking_parameter"),
        &format!("providers[{name}].thinking_parameter"),
    )?;
    let thinking_defaults = raw_thinking_defaults_dict(
        object.get("thinking_defaults"),
        &format!("providers[{name}].thinking_defaults"),
    )?;

    let mut models = models;
    if !models.contains(&default_model) {
        models.push(default_model.clone());
    }

    let config = match provider_type.as_str() {
        "anthropic" => {
            let cfg = AnthropicProviderConfig {
                name,
                base_url,
                api: api.unwrap_or_else(|| "anthropic-messages".to_string()),
                api_key_env,
                credential_name,
                models,
                default_model,
                context_windows,
                headers,
                compat,
                model_metadata,
                timeout_seconds,
                max_retries,
                max_retry_delay_seconds,
                thinking_levels,
                thinking_models,
                thinking_default,
                thinking_parameter,
                thinking_defaults,
            };
            cfg.validate()?;
            ProviderConfig::Anthropic(cfg)
        }
        "openai-codex" => {
            reject_catalog_only_legacy_metadata(&compat, &model_metadata)?;
            let cfg = OpenAICodexProviderConfig {
                name,
                base_url,
                api_key_env,
                credential_name,
                models,
                default_model,
                context_windows,
                headers,
                timeout_seconds,
                max_retries,
                max_retry_delay_seconds,
                thinking_levels,
                thinking_models,
                thinking_default,
                thinking_parameter,
                thinking_defaults,
            };
            cfg.validate()?;
            ProviderConfig::OpenAICodex(cfg)
        }
        other => {
            let cfg = OpenAICompatibleProviderConfig {
                name,
                base_url,
                api: api.unwrap_or_else(|| default_api_for_kind(other)),
                api_key_env,
                credential_name,
                models,
                default_model,
                context_windows,
                headers,
                compat,
                model_metadata,
                timeout_seconds,
                max_retries,
                max_retry_delay_seconds,
                thinking_levels,
                thinking_models,
                thinking_default,
                thinking_parameter,
                thinking_defaults,
            };
            cfg.validate()?;
            ProviderConfig::OpenAICompatible(cfg)
        }
    };
    Ok(config)
}

fn reject_catalog_only_legacy_metadata(
    compat: &JsonMap,
    model_metadata: &IndexMap<String, ProviderModelMetadata>,
) -> Result<(), ProviderConfigError> {
    if !compat.is_empty() || !model_metadata.is_empty() {
        return Err(cfg_err(
            "OpenAI Codex legacy provider metadata is not supported",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime config building
// ---------------------------------------------------------------------------

/// Build OpenAI-compatible runtime config from durable settings (tau
/// `openai_compatible_config_from_provider`).
pub fn openai_compatible_config_from_provider(
    provider: &OpenAICompatibleProviderConfig,
    credential_reader: Option<&dyn CredentialReader>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<OpenAICompatibleConfig, ProviderConfigError> {
    let wrapped = ProviderConfig::OpenAICompatible(provider.clone());
    let api_key = api_key_from_provider(&wrapped, credential_reader)?;
    let selected_model = or_default(model, &provider.default_model).to_string();
    let mut base_url = model_base_url(&wrapped, &selected_model);
    if provider.name == DEFAULT_PROVIDER_NAME && provider.api_key_env == "OPENAI_API_KEY" {
        if let Ok(env_url) = std::env::var("OPENAI_BASE_URL") {
            base_url = env_url;
        }
    }
    let reasoning_effort =
        reasoning_effort_from_provider(provider, Some(&selected_model), thinking_level)?;
    let compat = model_compat(&wrapped, &selected_model);
    let headers = header_list(&model_headers(&wrapped, &selected_model));

    let mut config = OpenAICompatibleConfig::new(api_key);
    config.provider_name.clone_from(&provider.name);
    config.api = provider_api(&wrapped, &selected_model);
    config.base_url = base_url.trim_end_matches('/').to_string();
    config.headers = Some(headers);
    config.timeout_seconds = provider.timeout_seconds;
    config.max_retries = u32::try_from(provider.max_retries).unwrap_or(u32::MAX);
    config.max_retry_delay_seconds = provider.max_retry_delay_seconds;
    config.reasoning_effort = reasoning_effort;
    config.reasoning_effort_parameter = provider
        .thinking_parameter
        .clone()
        .unwrap_or_else(|| "reasoning_effort".to_string());
    config.thinking_format = thinking_format(&wrapped, &selected_model);
    config.compat = compat;
    config.include_reasoning_effort_none =
        include_reasoning_effort_none(&wrapped, &selected_model, thinking_level);
    Ok(config)
}

/// Build Anthropic runtime config from durable settings (tau
/// `anthropic_config_from_provider`).
pub fn anthropic_config_from_provider(
    provider: &AnthropicProviderConfig,
    credential_reader: Option<&dyn CredentialReader>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<AnthropicConfig, ProviderConfigError> {
    let wrapped = ProviderConfig::Anthropic(provider.clone());
    let api_key = api_key_from_provider(&wrapped, credential_reader)?;
    let selected_model = or_default(model, &provider.default_model).to_string();
    let thinking_budget_tokens =
        anthropic_thinking_budget_from_provider(provider, Some(&selected_model), thinking_level)?;
    let headers = header_list(&model_headers(&wrapped, &selected_model));

    let mut config = AnthropicConfig::new(api_key);
    config.provider_name.clone_from(&provider.name);
    config.base_url = normalize_anthropic_base_url(&model_base_url(&wrapped, &selected_model));
    config.headers = Some(headers);
    config.timeout_seconds = provider.timeout_seconds;
    config.max_retries = u32::try_from(provider.max_retries).unwrap_or(u32::MAX);
    config.max_retry_delay_seconds = provider.max_retry_delay_seconds;
    config.thinking_budget_tokens = thinking_budget_tokens;
    config.thinking_effort =
        reasoning_effort_from_anthropic_provider(&wrapped, &selected_model, thinking_level)?;
    config.thinking_mode = anthropic_thinking_mode(&wrapped, &selected_model);
    Ok(config)
}

/// Return the durable provider kind (tau `provider_kind`).
#[must_use]
pub fn provider_kind(provider: &ProviderConfig) -> ProviderKind {
    match provider {
        ProviderConfig::Anthropic(_) => "anthropic".to_string(),
        ProviderConfig::OpenAICodex(_) => "openai-codex".to_string(),
        ProviderConfig::OpenAICompatible(c) => {
            if c.api == "google-generative-ai" {
                "google-generative-ai".to_string()
            } else if c.api == "mistral-conversations" {
                "mistral-conversations".to_string()
            } else {
                "openai-compatible".to_string()
            }
        }
    }
}

/// Return whether rho can attempt calls for this provider without prompting
/// setup (tau `provider_has_usable_credentials`).
#[must_use]
pub fn provider_has_usable_credentials(
    provider: &ProviderConfig,
    credential_reader: Option<&dyn CredentialReader>,
) -> bool {
    if let (Some(credential_name), Some(reader)) = (provider.credential_name(), credential_reader) {
        if !credential_name.is_empty() {
            if oauth_provider_registered(provider.name())
                && reader.get_oauth(credential_name).is_some()
            {
                return true;
            }
            if reader
                .get(credential_name)
                .is_some_and(|key| !key.is_empty())
            {
                return true;
            }
        }
    }
    std::env::var(provider.api_key_env()).is_ok_and(|value| !value.is_empty())
}

fn api_key_from_provider(
    provider: &ProviderConfig,
    credential_reader: Option<&dyn CredentialReader>,
) -> Result<String, ProviderConfigError> {
    if let (Some(credential_name), Some(reader)) = (provider.credential_name(), credential_reader) {
        if !credential_name.is_empty() {
            if let Some(credential) = reader.get(credential_name) {
                if !credential.is_empty() {
                    return Ok(credential);
                }
            }
            if oauth_provider_registered(provider.name()) {
                if let Some(access) = reader.get_oauth(credential_name) {
                    if !access.is_empty() {
                        return Ok(access);
                    }
                }
            }
        }
    }
    if let Ok(api_key) = std::env::var(provider.api_key_env()) {
        if !api_key.is_empty() {
            return Ok(api_key);
        }
    }
    let credential_hint = if provider.credential_name().is_some() {
        format!(" or run /login {}", provider.name())
    } else {
        String::new()
    };
    Err(cfg_err(format!(
        "Missing provider API key. Set {}{credential_hint}.",
        provider.api_key_env()
    )))
}

// ---------------------------------------------------------------------------
// Selection / thinking resolution
// ---------------------------------------------------------------------------

/// Resolve the provider and model for a run (tau `resolve_provider_selection`).
pub fn resolve_provider_selection(
    settings: &ProviderSettings,
    provider_name: Option<&str>,
    model: Option<&str>,
) -> Result<ProviderSelection, ProviderConfigError> {
    let provider = settings.get_provider(provider_name)?;
    let selected_model = or_default(model, provider.default_model());
    if selected_model.is_empty() {
        return Err(cfg_err(format!(
            "Provider {} does not define a default model",
            provider.name()
        )));
    }
    validate_provider_model(provider, selected_model)?;
    Ok(ProviderSelection {
        provider: provider.clone(),
        model: selected_model.to_string(),
    })
}

/// Raise when `model` is not declared by `provider` (tau `validate_provider_model`).
pub fn validate_provider_model(
    provider: &ProviderConfig,
    model: &str,
) -> Result<(), ProviderConfigError> {
    if provider.models().iter().any(|m| m == model) {
        return Ok(());
    }
    let mut available: Vec<String> = provider.models().to_vec();
    available.sort();
    let available = if available.is_empty() {
        "none".to_string()
    } else {
        available.join(", ")
    };
    Err(cfg_err(format!(
        "Model is not configured for provider {}: {model}. Available models: {available}",
        provider.name()
    )))
}

/// Return thinking levels supported by a provider/model pair (tau
/// `provider_thinking_levels`).
#[must_use]
pub fn provider_thinking_levels(provider: &ProviderConfig, model: Option<&str>) -> Vec<String> {
    let selected_model = or_default(model, provider.default_model());
    let metadata = metadata_for_model(provider, selected_model);
    if let Some(metadata) = metadata {
        if metadata.reasoning == Some(false) {
            return Vec::new();
        }
    }
    let Some(thinking_levels) = provider.thinking_levels() else {
        match metadata {
            Some(metadata) if metadata.reasoning == Some(true) => {
                return levels_from_thinking_map(&metadata.thinking_level_map);
            }
            _ => return Vec::new(),
        }
    };
    if !provider.thinking_models().is_empty()
        && !provider
            .thinking_models()
            .iter()
            .any(|m| m == selected_model)
    {
        return Vec::new();
    }
    thinking_levels
        .iter()
        .filter(|level| {
            metadata.is_none_or(|metadata| metadata_supports_thinking_level(metadata, level))
        })
        .cloned()
        .collect()
}

/// Explain why a provider/model pair has no configurable thinking modes (tau
/// `provider_thinking_unavailable_reason`).
#[must_use]
pub fn provider_thinking_unavailable_reason(
    provider: &ProviderConfig,
    model: Option<&str>,
) -> Option<String> {
    let selected_model = or_default(model, provider.default_model());
    let metadata = metadata_for_model(provider, selected_model);
    if let Some(metadata) = metadata {
        if metadata.reasoning == Some(false) {
            return Some(format!(
                "{}:{selected_model} is not a reasoning model",
                provider.name()
            ));
        }
    }
    if provider.thinking_levels().is_none() {
        if let Some(metadata) = metadata {
            if metadata.reasoning == Some(true) {
                return None;
            }
        }
        if matches!(provider, ProviderConfig::OpenAICodex(_)) {
            return Some(
                "OpenAI Codex subscription can stream reasoning output, but rho does \
                 not have a validated Codex transport mapping for changing reasoning \
                 effort yet"
                    .to_string(),
            );
        }
        return Some(format!(
            "Provider {} does not declare thinking_levels",
            provider.name()
        ));
    }
    if !provider.thinking_models().is_empty()
        && !provider
            .thinking_models()
            .iter()
            .any(|m| m == selected_model)
    {
        return Some(format!(
            "{}:{selected_model} is not declared in thinking_models",
            provider.name()
        ));
    }
    None
}

fn levels_from_thinking_map(thinking_level_map: &IndexMap<String, Option<String>>) -> Vec<String> {
    ["off", "minimal", "low", "medium", "high", "xhigh"]
        .into_iter()
        .filter(|level| thinking_level_map_supports(thinking_level_map, level))
        .map(str::to_string)
        .collect()
}

fn metadata_supports_thinking_level(metadata: &ProviderModelMetadata, level: &str) -> bool {
    thinking_level_map_supports(&metadata.thinking_level_map, level)
}

fn thinking_level_map_supports(
    thinking_level_map: &IndexMap<String, Option<String>>,
    level: &str,
) -> bool {
    match thinking_level_map.get(level) {
        Some(value) => value.is_some(),
        None => level != "xhigh",
    }
}

fn metadata_for_model<'a>(
    provider: &'a ProviderConfig,
    model: &str,
) -> Option<&'a ProviderModelMetadata> {
    provider.model_metadata().and_then(|map| map.get(model))
}

fn provider_api(provider: &ProviderConfig, model: &str) -> String {
    if let Some(metadata) = metadata_for_model(provider, model) {
        if let Some(api) = &metadata.api {
            return api.clone();
        }
    }
    if matches!(provider, ProviderConfig::OpenAICodex(_)) {
        return "openai-codex-responses".to_string();
    }
    provider.api().unwrap_or("openai-completions").to_string()
}

fn model_base_url(provider: &ProviderConfig, model: &str) -> String {
    if let Some(metadata) = metadata_for_model(provider, model) {
        if let Some(base_url) = &metadata.base_url {
            if !base_url.is_empty() {
                return base_url.clone();
            }
        }
    }
    provider.base_url().to_string()
}

fn model_headers(provider: &ProviderConfig, model: &str) -> IndexMap<String, String> {
    let mut headers = provider.headers().clone();
    if let Some(metadata) = metadata_for_model(provider, model) {
        for (key, value) in &metadata.headers {
            headers.insert(key.clone(), value.clone());
        }
    }
    headers
}

fn model_compat(provider: &ProviderConfig, model: &str) -> JsonMap {
    let mut compat = detected_compat(provider, model);
    if let Some(provider_compat) = provider.compat() {
        for (key, value) in provider_compat {
            compat.insert(key.clone(), value.clone());
        }
    }
    if let Some(metadata) = metadata_for_model(provider, model) {
        for (key, value) in &metadata.compat {
            compat.insert(key.clone(), value.clone());
        }
    }
    compat
}

fn detected_compat(provider: &ProviderConfig, model: &str) -> JsonMap {
    let base_url = model_base_url(provider, model);
    let name = provider.name();
    let is_together = name == "together" || base_url.contains("api.together.ai");
    let is_zai = name == "zai" || base_url.contains("api.z.ai");
    let is_moonshot =
        name == "moonshotai" || name == "moonshotai-cn" || base_url.contains("moonshot.");
    let is_grok = name == "xai" || base_url.contains("api.x.ai");
    let is_deepseek = name == "deepseek" || base_url.contains("deepseek.com");
    let is_cerebras = name == "cerebras" || base_url.contains("cerebras.ai");
    let is_openrouter = name == "openrouter" || base_url.contains("openrouter.ai");
    let is_nonstandard =
        is_cerebras || is_grok || is_together || is_deepseek || is_zai || is_moonshot;
    let use_max_tokens = is_moonshot || is_together;

    let thinking_format = if is_deepseek {
        "deepseek"
    } else if is_zai {
        "zai"
    } else if is_together {
        "together"
    } else if is_openrouter {
        "openrouter"
    } else {
        "openai"
    };

    let mut map = JsonMap::new();
    map.insert("supportsStore".into(), JsonValue::Bool(!is_nonstandard));
    map.insert(
        "supportsReasoningEffort".into(),
        JsonValue::Bool(!(is_grok || is_zai || is_moonshot || is_together)),
    );
    map.insert("supportsUsageInStreaming".into(), JsonValue::Bool(true));
    map.insert(
        "maxTokensField".into(),
        JsonValue::String(
            if use_max_tokens {
                "max_tokens"
            } else {
                "max_completion_tokens"
            }
            .to_string(),
        ),
    );
    map.insert(
        "thinkingFormat".into(),
        JsonValue::String(thinking_format.to_string()),
    );
    map.insert(
        "supportsStrictMode".into(),
        JsonValue::Bool(!(is_moonshot || is_together)),
    );
    map.insert(
        "supportsLongCacheRetention".into(),
        JsonValue::Bool(!is_together),
    );
    map
}

/// Return the preferred thinking level for a provider/model pair (tau
/// `provider_default_thinking_level`).
#[must_use]
pub fn provider_default_thinking_level(
    provider: &ProviderConfig,
    model: Option<&str>,
) -> Option<String> {
    let levels = provider_thinking_levels(provider, model);
    if levels.is_empty() {
        return None;
    }
    if let Some(default) = provider.thinking_default() {
        if levels.iter().any(|level| level == default) {
            return Some(default.to_string());
        }
    }
    if levels.iter().any(|level| level == DEFAULT_THINKING_LEVEL) {
        return Some(DEFAULT_THINKING_LEVEL.to_string());
    }
    levels.first().cloned()
}

fn reasoning_effort_from_provider(
    provider: &OpenAICompatibleProviderConfig,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Option<String>, ProviderConfigError> {
    let Some(thinking_level) = thinking_level else {
        return Ok(None);
    };
    if !matches!(
        provider.thinking_parameter.as_deref(),
        Some("reasoning_effort" | "reasoning.effort")
    ) {
        return Ok(None);
    }
    let wrapped = ProviderConfig::OpenAICompatible(provider.clone());
    let levels = provider_thinking_levels(&wrapped, model);
    if levels.is_empty() {
        return Ok(None);
    }
    let selected_model = or_default(model, &provider.default_model).to_string();
    let normalized = normalize_thinking_level(Some(thinking_level)).map_err(cfg_err)?;
    if !levels.contains(&normalized) {
        return Err(cfg_err(format!(
            "Thinking mode {normalized} is not available for {}:{selected_model}. Available modes: {}",
            provider.name,
            levels.join(", ")
        )));
    }
    if let Some(mapped) = metadata_thinking_value(&wrapped, &selected_model, &normalized) {
        return Ok(Some(mapped));
    }
    if provider.name == "huggingface" && normalized == "minimal" {
        return Ok(Some("low".to_string()));
    }
    Ok(Some(
        reasoning_effort_for_level(Some(&normalized)).map_err(cfg_err)?,
    ))
}

fn anthropic_thinking_budget_from_provider(
    provider: &AnthropicProviderConfig,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Option<i64>, ProviderConfigError> {
    let Some(thinking_level) = thinking_level else {
        return Ok(None);
    };
    if provider.thinking_parameter.as_deref() != Some("anthropic.thinking") {
        return Ok(None);
    }
    let wrapped = ProviderConfig::Anthropic(provider.clone());
    let selected_model = or_default(model, &provider.default_model).to_string();
    if anthropic_thinking_mode(&wrapped, &selected_model) == "adaptive" {
        return Ok(None);
    }
    let levels = provider_thinking_levels(&wrapped, Some(&selected_model));
    if levels.is_empty() {
        return Ok(None);
    }
    let normalized = normalize_thinking_level(Some(thinking_level)).map_err(cfg_err)?;
    if !levels.contains(&normalized) {
        return Err(cfg_err(format!(
            "Thinking mode {normalized} is not available for {}:{selected_model}. Available modes: {}",
            provider.name,
            levels.join(", ")
        )));
    }
    anthropic_thinking_budget_for_level(Some(&normalized)).map_err(cfg_err)
}

fn metadata_thinking_value(provider: &ProviderConfig, model: &str, level: &str) -> Option<String> {
    let metadata = metadata_for_model(provider, model)?;
    metadata
        .thinking_level_map
        .get(level)
        .and_then(Clone::clone)
}

fn thinking_format(provider: &ProviderConfig, model: &str) -> String {
    let compat = model_compat(provider, model);
    if let Some(JsonValue::String(value)) = compat.get("thinkingFormat") {
        if !value.is_empty() {
            return value.clone();
        }
    }
    let base_url = model_base_url(provider, model);
    let name = provider.name();
    if name == "deepseek" || base_url.contains("deepseek.com") {
        "deepseek".to_string()
    } else if name == "zai" || base_url.contains("api.z.ai") {
        "zai".to_string()
    } else if name == "together" || base_url.contains("api.together.ai") {
        "together".to_string()
    } else if name == "openrouter" || base_url.contains("openrouter.ai") {
        "openrouter".to_string()
    } else {
        "openai".to_string()
    }
}

fn include_reasoning_effort_none(
    provider: &ProviderConfig,
    model: &str,
    thinking_level: Option<&str>,
) -> bool {
    let Some(thinking_level) = thinking_level else {
        return false;
    };
    let Ok(normalized) = normalize_thinking_level(Some(thinking_level)) else {
        return false;
    };
    if normalized != "off" {
        return false;
    }
    metadata_thinking_value(provider, model, "off").as_deref() == Some("none")
}

fn reasoning_effort_from_anthropic_provider(
    provider: &ProviderConfig,
    model: &str,
    thinking_level: Option<&str>,
) -> Result<Option<String>, ProviderConfigError> {
    let Some(thinking_level) = thinking_level else {
        return Ok(None);
    };
    let normalized = normalize_thinking_level(Some(thinking_level)).map_err(cfg_err)?;
    if normalized == "off" {
        return Ok(None);
    }
    let mapped = metadata_thinking_value(provider, model, &normalized);
    Ok(Some(mapped.unwrap_or(normalized)))
}

fn anthropic_thinking_mode(provider: &ProviderConfig, model: &str) -> String {
    let compat = model_compat(provider, model);
    if compat.get("forceAdaptiveThinking") == Some(&JsonValue::Bool(true)) {
        "adaptive".to_string()
    } else {
        "budget".to_string()
    }
}

fn normalize_anthropic_base_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/v1") {
        normalized.to_string()
    } else {
        format!("{normalized}/v1")
    }
}

// ---------------------------------------------------------------------------
// __post_init__ validators
// ---------------------------------------------------------------------------

fn validate_provider_numbers(
    timeout_seconds: f64,
    max_retries: i64,
    max_retry_delay_seconds: f64,
) -> Result<(), ProviderConfigError> {
    if timeout_seconds <= 0.0 {
        return Err(cfg_err("Provider timeout_seconds must be greater than 0"));
    }
    if max_retries < 0 {
        return Err(cfg_err("Provider max_retries must be 0 or greater"));
    }
    if max_retry_delay_seconds < 0.0 {
        return Err(cfg_err(
            "Provider max_retry_delay_seconds must be 0 or greater",
        ));
    }
    Ok(())
}

fn validate_context_windows(
    context_windows: &IndexMap<String, i64>,
) -> Result<(), ProviderConfigError> {
    for (model, context_window) in context_windows {
        if model.trim().is_empty() {
            return Err(cfg_err(
                "Provider context_windows keys must be non-empty strings",
            ));
        }
        if *context_window <= 0 {
            return Err(cfg_err(
                "Provider context_windows values must be positive integers",
            ));
        }
    }
    Ok(())
}

fn validate_model_metadata(
    models: &[String],
    model_metadata: &IndexMap<String, ProviderModelMetadata>,
) -> Result<(), ProviderConfigError> {
    for (model, metadata) in model_metadata {
        if !models.iter().any(|m| m == model) {
            return Err(cfg_err(format!(
                "Provider model_metadata key is not in models: {model}"
            )));
        }
        if metadata.context_window.is_some_and(|value| value <= 0) {
            return Err(cfg_err(
                "Provider model_metadata context_window must be positive",
            ));
        }
        if metadata.max_tokens.is_some_and(|value| value <= 0) {
            return Err(cfg_err(
                "Provider model_metadata max_tokens must be positive",
            ));
        }
        if metadata
            .input
            .iter()
            .any(|item| item != "text" && item != "image")
        {
            return Err(cfg_err(
                "Provider model_metadata input must contain text or image",
            ));
        }
        if metadata.cost.values().any(|value| *value < 0.0) {
            return Err(cfg_err(
                "Provider model_metadata cost values must be non-negative",
            ));
        }
        validate_runtime_cost_tiers(&metadata.cost_tiers)?;
        validate_json_object(&metadata.compat, "Provider model_metadata compat")?;
        validate_string_dict(&metadata.headers, "Provider model_metadata headers")?;
        for (level, value) in &metadata.thinking_level_map {
            normalize_thinking_level(Some(level)).map_err(cfg_err)?;
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(cfg_err(
                        "Provider model_metadata thinking_level_map values must be strings or null",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_runtime_cost_tiers(tiers: &[ModelCostTier]) -> Result<(), ProviderConfigError> {
    if let Some(last) = tiers.last() {
        if last.max_input_tokens.is_some() {
            return Err(cfg_err(
                "Provider model_metadata final cost tier must omit max_input_tokens",
            ));
        }
    }
    let mut previous_limit = 0i64;
    for tier in tiers {
        if tier.cost.values().any(|value| *value < 0.0) {
            return Err(cfg_err(
                "Provider model_metadata cost tier values must be non-negative",
            ));
        }
        let Some(limit) = tier.max_input_tokens else {
            continue;
        };
        if limit <= previous_limit {
            return Err(cfg_err(
                "Provider model_metadata cost tier limits must be strictly increasing",
            ));
        }
        previous_limit = limit;
    }
    Ok(())
}

fn validate_string_dict(
    value: &IndexMap<String, String>,
    field_name: &str,
) -> Result<(), ProviderConfigError> {
    for (key, item) in value {
        if key.trim().is_empty() {
            return Err(cfg_err(format!(
                "{field_name} keys must be non-empty strings"
            )));
        }
        if item.trim().is_empty() {
            return Err(cfg_err(format!(
                "{field_name} values must be non-empty strings"
            )));
        }
    }
    Ok(())
}

fn validate_json_object(value: &JsonMap, field_name: &str) -> Result<(), ProviderConfigError> {
    for (key, _item) in value {
        if key.trim().is_empty() {
            return Err(cfg_err(format!(
                "{field_name} keys must be non-empty strings"
            )));
        }
    }
    Ok(())
}

fn validate_thinking_defaults(
    thinking_defaults: &IndexMap<String, String>,
) -> Result<(), ProviderConfigError> {
    for (model, thinking_level) in thinking_defaults {
        if model.trim().is_empty() {
            return Err(cfg_err(
                "Provider thinking_defaults keys must be non-empty strings",
            ));
        }
        normalize_thinking_level(Some(thinking_level)).map_err(cfg_err)?;
    }
    Ok(())
}

fn validate_thinking_config(
    thinking_levels: Option<&Vec<String>>,
    thinking_models: &[String],
    thinking_default: Option<&String>,
    thinking_parameter: Option<&String>,
) -> Result<(), ProviderConfigError> {
    let Some(thinking_levels) = thinking_levels else {
        if !thinking_models.is_empty() || thinking_default.is_some() || thinking_parameter.is_some()
        {
            return Err(cfg_err(
                "Provider thinking_levels must be set before thinking metadata",
            ));
        }
        return Ok(());
    };
    let normalized = normalize_thinking_levels(thinking_levels).map_err(cfg_err)?;
    if &normalized != thinking_levels {
        return Err(cfg_err("Provider thinking_levels must be normalized"));
    }
    if thinking_models.iter().any(|model| model.trim().is_empty()) {
        return Err(cfg_err(
            "Provider thinking_models must contain non-empty strings",
        ));
    }
    if let Some(default) = thinking_default {
        if !thinking_levels.contains(default) {
            return Err(cfg_err(
                "Provider thinking_default must be in thinking_levels",
            ));
        }
    }
    if let Some(parameter) = thinking_parameter {
        if !matches!(
            parameter.as_str(),
            "reasoning_effort" | "reasoning.effort" | "anthropic.thinking"
        ) {
            return Err(cfg_err(
                "Provider thinking_parameter must be reasoning_effort, reasoning.effort, or anthropic.thinking",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON field parsers
// ---------------------------------------------------------------------------

fn optional_provider_api(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<String>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(text))
            if matches!(
                text.as_str(),
                "openai-completions"
                    | "openai-responses"
                    | "anthropic-messages"
                    | "openai-codex-responses"
                    | "google-generative-ai"
                    | "mistral-conversations"
            ) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(cfg_err(format!(
            "Provider field has unsupported API: {field_name}"
        ))),
    }
}

fn optional_string(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<String>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(text)) if !text.trim().is_empty() => {
            Ok(Some(text.trim().to_string()))
        }
        Some(_) => Err(cfg_err(format!(
            "Provider field must be a non-empty string: {field_name}"
        ))),
    }
}

fn parse_string(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<String, ProviderConfigError> {
    match value {
        Some(JsonValue::String(text)) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        _ => Err(cfg_err(format!(
            "Provider field must be a non-empty string: {field_name}"
        ))),
    }
}

fn parse_string_value(value: &str, field_name: &str) -> Result<String, ProviderConfigError> {
    if value.trim().is_empty() {
        return Err(cfg_err(format!(
            "Provider field must be a non-empty string: {field_name}"
        )));
    }
    Ok(value.trim().to_string())
}

fn parse_string_tuple(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Vec<String>, ProviderConfigError> {
    let list = value
        .and_then(JsonValue::as_array)
        .filter(|list| !list.is_empty())
        .ok_or_else(|| {
            cfg_err(format!(
                "Provider field must be a non-empty string list: {field_name}"
            ))
        })?;
    let items: Vec<String> = list
        .iter()
        .filter_map(|item| match item {
            JsonValue::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
        .collect();
    if items.len() != list.len() {
        return Err(cfg_err(format!(
            "Provider field must be a string list: {field_name}"
        )));
    }
    Ok(items)
}

fn optional_string_tuple(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Vec<String>, ProviderConfigError> {
    let value = match value {
        None | Some(JsonValue::Null) => return Ok(Vec::new()),
        Some(value) => value,
    };
    let list = value.as_array().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be a string list: {field_name}"
        ))
    })?;
    let items: Vec<String> = list
        .iter()
        .filter_map(|item| match item {
            JsonValue::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
        .collect();
    if items.len() != list.len() {
        return Err(cfg_err(format!(
            "Provider field must be a string list: {field_name}"
        )));
    }
    Ok(items)
}

fn optional_thinking_levels(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<Vec<String>>, ProviderConfigError> {
    let value = match value {
        None | Some(JsonValue::Null) => return Ok(None),
        Some(value) => value,
    };
    let list = value.as_array().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be a thinking mode list: {field_name}"
        ))
    })?;
    let strings: Vec<String> = list
        .iter()
        .map(|item| match item {
            JsonValue::String(text) => Ok(text.clone()),
            _ => Err(cfg_err(format!(
                "Provider field must be a thinking mode list: {field_name}"
            ))),
        })
        .collect::<Result<_, _>>()?;
    Ok(Some(normalize_thinking_levels(&strings).map_err(cfg_err)?))
}

fn optional_thinking_level_opt(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<String>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(text)) => {
            Ok(Some(normalize_thinking_level(Some(text)).map_err(cfg_err)?))
        }
        Some(_) => Err(cfg_err(format!(
            "Provider field must be a thinking mode: {field_name}"
        ))),
    }
}

fn optional_thinking_level(
    value: &JsonValue,
    field_name: &str,
) -> Result<Option<String>, ProviderConfigError> {
    match value {
        JsonValue::Null => Ok(None),
        JsonValue::String(text) => Ok(Some(normalize_thinking_level(Some(text)).map_err(cfg_err)?)),
        _ => Err(cfg_err(format!(
            "Provider field must be a thinking mode: {field_name}"
        ))),
    }
}

fn optional_thinking_parameter(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<String>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(text))
            if matches!(
                text.as_str(),
                "reasoning_effort" | "reasoning.effort" | "anthropic.thinking"
            ) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(cfg_err(format!(
            "Provider field must be reasoning_effort, reasoning.effort, or anthropic.thinking: {field_name}"
        ))),
    }
}

fn parse_string_dict(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<IndexMap<String, String>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be a string object: {field_name}"
        ))
    })?;
    let mut items = IndexMap::new();
    for (key, item) in object {
        match item {
            JsonValue::String(text) if !key.trim().is_empty() && !text.trim().is_empty() => {
                items.insert(key.trim().to_string(), text.trim().to_string());
            }
            _ => {
                return Err(cfg_err(format!(
                    "Provider field must be a string object: {field_name}"
                )));
            }
        }
    }
    Ok(items)
}

fn parse_json_dict(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<JsonMap, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(JsonMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| cfg_err(format!("Provider field must be an object: {field_name}")))?;
    let mut items = JsonMap::new();
    for (key, item) in object {
        if key.trim().is_empty() {
            return Err(cfg_err(format!(
                "Provider field must have string keys: {field_name}"
            )));
        }
        items.insert(key.trim().to_string(), item.clone());
    }
    Ok(items)
}

fn model_metadata_dict(
    value: Option<&JsonValue>,
    models: &[String],
    field_name: &str,
) -> Result<IndexMap<String, ProviderModelMetadata>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| cfg_err(format!("Provider field must be an object: {field_name}")))?;
    let mut items = IndexMap::new();
    for (key, item) in object {
        let model = parse_string_value(key, field_name)?;
        if !models.iter().any(|m| m == &model) {
            return Err(cfg_err(format!(
                "Provider model_metadata key is not in models: {model}"
            )));
        }
        let object = item.as_object().ok_or_else(|| {
            cfg_err(format!(
                "Provider model_metadata entries must be objects: {field_name}"
            ))
        })?;
        let mloc = |sub: &str| format!("{field_name}.{model}.{sub}");
        let metadata = ProviderModelMetadata {
            name: optional_string(object.get("name"), &mloc("name"))?,
            api: optional_provider_api(object.get("api"), &mloc("api"))?,
            base_url: optional_string(object.get("base_url"), &mloc("base_url"))?,
            reasoning: optional_bool(object.get("reasoning"), &mloc("reasoning"))?,
            input: optional_string_tuple(object.get("input"), &mloc("input"))?,
            cost: parse_float_dict(object.get("cost"), &mloc("cost"))?,
            cost_tiers: parse_cost_tiers(object.get("cost_tiers"), &mloc("cost_tiers"))?,
            context_window: optional_positive_int(
                object.get("context_window"),
                &mloc("context_window"),
            )?,
            max_tokens: optional_positive_int(object.get("max_tokens"), &mloc("max_tokens"))?,
            headers: parse_string_dict(object.get("headers"), &mloc("headers"))?,
            compat: parse_json_dict(object.get("compat"), &mloc("compat"))?,
            thinking_level_map: parse_thinking_level_map(
                object.get("thinking_level_map"),
                &mloc("thinking_level_map"),
            )?,
        };
        items.insert(model, metadata);
    }
    Ok(items)
}

fn parse_cost_tiers(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Vec<ModelCostTier>, ProviderConfigError> {
    let list = match value {
        None | Some(JsonValue::Null) => return Ok(Vec::new()),
        Some(JsonValue::Array(list)) => list,
        Some(_) => {
            return Err(cfg_err(format!(
                "Provider field must be an array: {field_name}"
            )));
        }
    };
    let mut tiers = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| cfg_err(format!("Provider cost tiers must be objects: {field_name}")))?;
        let tier_field = format!("{field_name}.{index}");
        let allowed = [
            "max_input_tokens",
            "input",
            "output",
            "cacheRead",
            "cacheWrite",
        ];
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(cfg_err(format!(
                "Provider cost tier has unknown fields: {tier_field}"
            )));
        }
        let mut cost = IndexMap::new();
        for key in ["input", "output", "cacheRead", "cacheWrite"] {
            cost.insert(
                key.to_string(),
                parse_non_negative_float(object.get(key), &format!("{tier_field}.{key}"))?,
            );
        }
        tiers.push(ModelCostTier {
            max_input_tokens: optional_positive_int(
                object.get("max_input_tokens"),
                &format!("{tier_field}.max_input_tokens"),
            )?,
            cost,
        });
    }
    validate_runtime_cost_tiers(&tiers)?;
    Ok(tiers)
}

fn parse_thinking_level_map(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<IndexMap<String, Option<String>>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| cfg_err(format!("Provider field must be an object: {field_name}")))?;
    let mut items = IndexMap::new();
    for (key, item) in object {
        let Ok(level) = normalize_thinking_level(Some(key)) else {
            return Err(cfg_err(format!(
                "Provider field must be a thinking mode: {field_name}"
            )));
        };
        match item {
            JsonValue::Null => {
                items.insert(level, None);
            }
            JsonValue::String(text) if !text.trim().is_empty() => {
                items.insert(level, Some(text.trim().to_string()));
            }
            _ => {
                return Err(cfg_err(format!(
                    "Provider field values must be strings or null: {field_name}"
                )));
            }
        }
    }
    Ok(items)
}

fn parse_float_dict(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<IndexMap<String, f64>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be a number object: {field_name}"
        ))
    })?;
    let mut items = IndexMap::new();
    for (key, item) in object {
        if key.trim().is_empty() {
            return Err(cfg_err(format!(
                "Provider field must be a number object: {field_name}"
            )));
        }
        let number = match item {
            JsonValue::Number(number) => number.as_f64().filter(|value| *value >= 0.0),
            _ => None,
        };
        let Some(number) = number else {
            return Err(cfg_err(format!(
                "Provider field values must be non-negative: {field_name}"
            )));
        };
        items.insert(key.trim().to_string(), number);
    }
    Ok(items)
}

fn optional_bool(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<bool>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Bool(boolean)) => Ok(Some(*boolean)),
        Some(_) => Err(cfg_err(format!(
            "Provider field must be a boolean: {field_name}"
        ))),
    }
}

fn optional_positive_int(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<Option<i64>, ProviderConfigError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Number(number)) => match number.as_i64() {
            Some(int) if int > 0 => Ok(Some(int)),
            _ => Err(cfg_err(format!(
                "Provider field must be a positive integer: {field_name}"
            ))),
        },
        Some(_) => Err(cfg_err(format!(
            "Provider field must be a positive integer: {field_name}"
        ))),
    }
}

fn parse_context_window_dict(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<IndexMap<String, i64>, ProviderConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        cfg_err(format!(
            "Provider field must be an integer object: {field_name}"
        ))
    })?;
    let mut items = IndexMap::new();
    for (key, item) in object {
        if key.trim().is_empty() {
            return Err(cfg_err(format!(
                "Provider field must be an integer object: {field_name}"
            )));
        }
        let int = match item {
            JsonValue::Number(number) => number.as_i64().filter(|value| *value > 0),
            _ => None,
        };
        let Some(int) = int else {
            return Err(cfg_err(format!(
                "Provider field values must be positive integers: {field_name}"
            )));
        };
        items.insert(key.trim().to_string(), int);
    }
    Ok(items)
}

fn parse_positive_float(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<f64, ProviderConfigError> {
    let number = match value {
        Some(JsonValue::Number(number)) => number.as_f64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(cfg_err(format!(
            "Provider field must be a positive number: {field_name}"
        )));
    };
    if number <= 0.0 {
        return Err(cfg_err(format!(
            "Provider field must be greater than 0: {field_name}"
        )));
    }
    Ok(number)
}

fn parse_non_negative_int(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<i64, ProviderConfigError> {
    let int = match value {
        Some(JsonValue::Number(number)) if number.is_i64() || number.is_u64() => number.as_i64(),
        _ => {
            return Err(cfg_err(format!(
                "Provider field must be a non-negative integer: {field_name}"
            )));
        }
    };
    let Some(int) = int else {
        return Err(cfg_err(format!(
            "Provider field must be a non-negative integer: {field_name}"
        )));
    };
    if int < 0 {
        return Err(cfg_err(format!(
            "Provider field must be 0 or greater: {field_name}"
        )));
    }
    Ok(int)
}

fn parse_non_negative_float(
    value: Option<&JsonValue>,
    field_name: &str,
) -> Result<f64, ProviderConfigError> {
    let number = match value {
        Some(JsonValue::Number(number)) => number.as_f64(),
        _ => None,
    };
    let Some(number) = number else {
        return Err(cfg_err(format!(
            "Provider field must be a non-negative number: {field_name}"
        )));
    };
    if number < 0.0 {
        return Err(cfg_err(format!(
            "Provider field must be 0 or greater: {field_name}"
        )));
    }
    Ok(number)
}

// ---------------------------------------------------------------------------
// JSON emit helpers
// ---------------------------------------------------------------------------

fn header_list(map: &IndexMap<String, String>) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn opt_string_json(value: Option<&String>) -> JsonValue {
    value.map_or(JsonValue::Null, |s| JsonValue::String(s.clone()))
}

fn opt_int_json(value: Option<i64>) -> JsonValue {
    value.map_or(JsonValue::Null, JsonValue::from)
}

fn float_json(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value).map_or(JsonValue::Null, JsonValue::Number)
}

fn string_vec_json(values: &[String]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .map(|s| JsonValue::String(s.clone()))
            .collect(),
    )
}

fn opt_string_vec_json(values: Option<&Vec<String>>) -> JsonValue {
    values.map_or(JsonValue::Null, |v| string_vec_json(v))
}

fn string_map_json(map: &IndexMap<String, String>) -> JsonValue {
    let mut object = JsonMap::new();
    for (key, value) in map {
        object.insert(key.clone(), JsonValue::String(value.clone()));
    }
    JsonValue::Object(object)
}

fn int_map_json(map: &IndexMap<String, i64>) -> JsonValue {
    let mut object = JsonMap::new();
    for (key, value) in map {
        object.insert(key.clone(), JsonValue::from(*value));
    }
    JsonValue::Object(object)
}

fn float_map_json(map: &IndexMap<String, f64>) -> JsonValue {
    let mut object = JsonMap::new();
    for (key, value) in map {
        object.insert(key.clone(), float_json(*value));
    }
    JsonValue::Object(object)
}

fn thinking_level_map_json(map: &IndexMap<String, Option<String>>) -> JsonValue {
    let mut object = JsonMap::new();
    for (key, value) in map {
        object.insert(key.clone(), opt_string_json(value.as_ref()));
    }
    JsonValue::Object(object)
}

fn cost_tiers_json(tiers: &[ModelCostTier]) -> JsonValue {
    JsonValue::Array(
        tiers
            .iter()
            .map(|tier| {
                let mut object = JsonMap::new();
                if let Some(limit) = tier.max_input_tokens {
                    object.insert("max_input_tokens".into(), JsonValue::from(limit));
                }
                for (key, value) in &tier.cost {
                    object.insert(key.clone(), float_json(*value));
                }
                JsonValue::Object(object)
            })
            .collect(),
    )
}

fn model_metadata_json(map: &IndexMap<String, ProviderModelMetadata>) -> JsonValue {
    let mut object = JsonMap::new();
    for (key, value) in map {
        object.insert(key.clone(), value.to_json());
    }
    JsonValue::Object(object)
}

fn to_sorted_pretty_json(value: &JsonValue) -> String {
    serde_json::to_string_pretty(&sort_json(value)).unwrap_or_default()
}

fn sort_json(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = JsonMap::new();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&map[key]));
            }
            JsonValue::Object(sorted)
        }
        JsonValue::Array(items) => JsonValue::Array(items.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}
