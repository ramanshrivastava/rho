//! Built-in provider catalog for rho login/setup flows (port of tau's
//! `tau_coding/provider_catalog.py`).
//!
//! tau models the provider "kind"/"api"/"input"/"thinking parameter" labels as
//! `Literal[...]` string unions; rho keeps them as plain [`String`] aliases,
//! validated at the parse boundaries in [`crate::catalog_loader`] and
//! [`crate::provider_config`]. Insertion-ordered dicts become [`IndexMap`]s so
//! catalog/model ordering matches tau's order-preserving Python dicts.

use std::sync::LazyLock;

use indexmap::IndexMap;
use rho_agent::types::JsonMap;

use crate::catalog_loader::builtin_catalog;

/// Provider "kind" label (tau `ProviderKind` literal).
pub type ProviderKind = String;
/// Provider request-family label (tau `ProviderApi` literal).
pub type ProviderApi = String;
/// Supported model input modality (tau `ModelInput` literal: `text`/`image`).
pub type ModelInput = String;
/// OAuth/api-key auth method label (tau `AuthMethod` literal).
pub type AuthMethod = String;
/// Provider thinking-parameter wire key (tau `ThinkingParameter` literal).
pub type ThinkingParameter = String;
/// Map of thinking level to provider-specific value (or `None` when
/// unsupported) — tau `ThinkingLevelMap`.
pub type ThinkingLevelMap = IndexMap<String, Option<String>>;

/// Model rates that apply up to an optional input-token limit (tau
/// `ModelCostTier`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCostTier {
    /// Per-field rates (keys `input`/`output`/`cacheRead`/`cacheWrite`).
    pub cost: IndexMap<String, f64>,
    /// Upper input-token bound for this tier (`None` on the final tier).
    pub max_input_tokens: Option<i64>,
}

/// Provider-catalog metadata for a single model (tau `ModelCatalogMetadata`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelCatalogMetadata {
    /// Display name.
    pub name: Option<String>,
    /// Request-family override.
    pub api: Option<ProviderApi>,
    /// Base-URL override.
    pub base_url: Option<String>,
    /// Whether the model is a reasoning model.
    pub reasoning: Option<bool>,
    /// Supported input modalities.
    pub input: Vec<ModelInput>,
    /// Flat base cost (or `None`).
    pub cost: Option<IndexMap<String, f64>>,
    /// Tiered pricing (empty when flat).
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
    pub thinking_level_map: ThinkingLevelMap,
}

/// Return model rates for an input size, falling back to the flat base cost
/// (tau `model_cost_for_input_tokens`).
///
/// tau rejects `bool`/negative `input_tokens` with `ValueError`; rho's signed
/// integer parameter cannot carry a Python `bool`, so only the negative check
/// survives (see dev-notes).
pub fn model_cost_for_input_tokens(
    metadata: &ModelCatalogMetadata,
    input_tokens: i64,
) -> Result<Option<IndexMap<String, f64>>, String> {
    if input_tokens < 0 {
        return Err("input_tokens must be a non-negative integer".to_string());
    }
    for tier in &metadata.cost_tiers {
        if tier.max_input_tokens.is_none() || input_tokens <= tier.max_input_tokens.unwrap() {
            return Ok(Some(tier.cost.clone()));
        }
    }
    Ok(metadata.cost.clone())
}

/// A built-in provider rho can present during login (tau `ProviderCatalogEntry`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCatalogEntry {
    /// Stable provider id.
    pub name: String,
    /// Human-facing name.
    pub display_name: String,
    /// Provider kind label.
    pub kind: ProviderKind,
    /// Base URL.
    pub base_url: String,
    /// Environment variable holding the API key.
    pub api_key_env: String,
    /// Credential-store key (or `None`).
    pub credential_name: Option<String>,
    /// Declared models, in catalog order.
    pub models: Vec<String>,
    /// Default model (must be in `models`).
    pub default_model: String,
    /// Documentation URL.
    pub docs_url: String,
    /// Optional request-family override.
    pub api: Option<ProviderApi>,
    /// Per-model context windows (or `None` when empty).
    pub context_windows: Option<IndexMap<String, i64>>,
    /// Extra request headers.
    pub headers: IndexMap<String, String>,
    /// Provider-level compatibility flags.
    pub compat: JsonMap,
    /// Per-model metadata.
    pub model_metadata: IndexMap<String, ModelCatalogMetadata>,
    /// Declared thinking levels (or `None`).
    pub thinking_levels: Option<Vec<String>>,
    /// Models that support thinking.
    pub thinking_models: Vec<String>,
    /// Default thinking level.
    pub thinking_default: Option<String>,
    /// Thinking-parameter wire key.
    pub thinking_parameter: Option<ThinkingParameter>,
    /// Supported auth methods (default `("api_key",)`).
    pub auth_methods: Vec<AuthMethod>,
}

/// rho's built-in provider catalog, built once from the vendored TOML (tau
/// `BUILTIN_PROVIDER_CATALOG`).
pub static BUILTIN_PROVIDER_CATALOG: LazyLock<Vec<ProviderCatalogEntry>> =
    LazyLock::new(|| builtin_catalog().clone());

/// Return a built-in catalog entry by provider name (tau `builtin_provider_entry`).
#[must_use]
pub fn builtin_provider_entry(name: &str) -> Option<ProviderCatalogEntry> {
    BUILTIN_PROVIDER_CATALOG
        .iter()
        .find(|entry| entry.name == name)
        .cloned()
}
