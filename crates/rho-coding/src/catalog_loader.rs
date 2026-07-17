//! Load rho's provider catalog from the packaged and user TOML files (port of
//! tau's `tau_coding/catalog_loader.py`).
//!
//! The built-in catalog is vendored byte-identically at `data/catalog.toml` and
//! embedded with [`include_str!`]. tau validates with pydantic; rho validates by
//! hand, preserving tau's field types (strict ints, non-empty strings,
//! non-negative floats), `extra="forbid"` semantics, and the exact user-facing
//! error strings (dotted `providers.<name>.<field>` locations).

#![allow(clippy::items_after_statements)]

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use rho_agent::types::{JsonMap, JsonValue};

use crate::paths::RhoPaths;
use crate::provider_catalog::{
    ModelCatalogMetadata, ModelCostTier, ProviderCatalogEntry, ThinkingLevelMap,
};
use crate::thinking::THINKING_LEVELS;

/// Catalog schema version understood by this build (tau `CATALOG_SCHEMA_VERSION`).
pub const CATALOG_SCHEMA_VERSION: i64 = 1;
/// User-level overlay filename (tau `USER_CATALOG_FILENAME`).
pub const USER_CATALOG_FILENAME: &str = "catalog.toml";

/// Vendored built-in catalog TOML (byte-identical to tau's packaged file).
const CATALOG_TOML: &str = include_str!("../data/catalog.toml");

/// Thinking fields merged as a group by the user-catalog overlay (tau
/// `_THINKING_FIELDS`).
const THINKING_FIELDS: [&str; 4] = [
    "thinking_levels",
    "thinking_models",
    "thinking_default",
    "thinking_parameter",
];

const KIND_VALUES: [&str; 5] = [
    "openai-compatible",
    "anthropic",
    "openai-codex",
    "google-generative-ai",
    "mistral-conversations",
];

const API_VALUES: [&str; 6] = [
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
    "openai-codex-responses",
    "google-generative-ai",
    "mistral-conversations",
];

const AUTH_METHOD_VALUES: [&str; 2] = ["api_key", "oauth"];
const MODEL_INPUT_VALUES: [&str; 2] = ["text", "image"];
const THINKING_PARAMETER_VALUES: [&str; 3] =
    ["reasoning_effort", "reasoning.effort", "anthropic.thinking"];

/// Raised when a rho catalog file is invalid (tau `CatalogError(ValueError)`).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct CatalogError(pub String);

fn err(message: impl Into<String>) -> CatalogError {
    CatalogError(message.into())
}

/// Return the packaged builtin catalog TOML text (tau `builtin_catalog_resource_text`).
#[must_use]
pub fn builtin_catalog_resource_text() -> &'static str {
    CATALOG_TOML
}

static BUILTIN_RAW: std::sync::LazyLock<toml::Table> = std::sync::LazyLock::new(|| {
    parse_catalog_text(CATALOG_TOML, "built-in catalog.toml")
        .expect("vendored catalog.toml must parse")
});

static BUILTIN_CATALOG: std::sync::LazyLock<Vec<ProviderCatalogEntry>> =
    std::sync::LazyLock::new(|| {
        entries_from_raw(&BUILTIN_RAW, "built-in catalog.toml")
            .expect("vendored catalog.toml must validate")
    });

/// Return rho's built-in provider catalog from the packaged data file (tau
/// `builtin_catalog`).
#[must_use]
pub fn builtin_catalog() -> &'static Vec<ProviderCatalogEntry> {
    &BUILTIN_CATALOG
}

/// Return the user-level catalog overlay path (tau `user_catalog_path`).
#[must_use]
pub fn user_catalog_path(paths: Option<&RhoPaths>) -> PathBuf {
    let default = RhoPaths::default();
    let paths = paths.unwrap_or(&default);
    paths.home.join(USER_CATALOG_FILENAME)
}

/// Return the builtin catalog with the user's `~/.rho/catalog.toml` overlaid
/// (tau `effective_catalog`).
pub fn effective_catalog(
    paths: Option<&RhoPaths>,
) -> Result<Vec<ProviderCatalogEntry>, CatalogError> {
    let path = user_catalog_path(paths);
    if !path.exists() {
        return Ok(builtin_catalog().clone());
    }
    let source = path.display().to_string();
    let text = std::fs::read_to_string(&path).map_err(|error| err(format!("{source}: {error}")))?;
    let overlay_raw = parse_catalog_text(&text, &source)?;
    validate_catalog_root(&overlay_raw, &source)?;
    let merged = merge_raw_catalogs(&BUILTIN_RAW, &overlay_raw)?;
    entries_from_raw(&merged, &source)
}

/// Upsert full provider definitions into the user-level catalog file (tau
/// `save_user_catalog_entries`).
pub fn save_user_catalog_entries(
    entries: &[ProviderCatalogEntry],
    paths: Option<&RhoPaths>,
) -> Result<PathBuf, CatalogError> {
    let path = user_catalog_path(paths);
    let source = path.display().to_string();
    let mut raw: toml::Table = if path.exists() {
        let text =
            std::fs::read_to_string(&path).map_err(|error| err(format!("{source}: {error}")))?;
        let parsed = parse_catalog_text(&text, &source)?;
        validate_catalog_root(&parsed, &source)?;
        parsed
    } else {
        let mut table = toml::Table::new();
        table.insert(
            "schema_version".to_string(),
            toml::Value::Integer(CATALOG_SCHEMA_VERSION),
        );
        table.insert("providers".to_string(), toml::Value::Array(Vec::new()));
        table
    };

    let mut providers = raw_providers(&raw)?;
    let mut provider_indexes: IndexMap<String, usize> = IndexMap::new();
    for (index, provider) in providers.iter().enumerate() {
        provider_indexes.insert(raw_provider_name(provider)?, index);
    }
    for entry in entries {
        let raw_provider = raw_provider_from_entry(entry);
        if let Some(&index) = provider_indexes.get(&entry.name) {
            providers[index] = raw_provider;
        } else {
            provider_indexes.insert(entry.name.clone(), providers.len());
            providers.push(raw_provider);
        }
    }

    let schema_version = raw
        .remove("schema_version")
        .unwrap_or(toml::Value::Integer(CATALOG_SCHEMA_VERSION));
    let mut updated = toml::Table::new();
    updated.insert("schema_version".to_string(), schema_version);
    updated.insert(
        "providers".to_string(),
        toml::Value::Array(providers.into_iter().map(toml::Value::Table).collect()),
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| err(format!("{source}: {error}")))?;
    }
    atomic_write_text(&path, &catalog_to_toml(&updated)?)
        .map_err(|error| err(format!("{source}: {error}")))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Parsing / validation
// ---------------------------------------------------------------------------

fn parse_catalog_text(text: &str, source: &str) -> Result<toml::Table, CatalogError> {
    text.parse::<toml::Table>()
        .map_err(|error| err(format!("{source}: invalid TOML: {error}")))
}

fn validate_catalog_root(raw: &toml::Table, source: &str) -> Result<(), CatalogError> {
    let allowed = ["schema_version", "providers"];
    let mut unknown: Vec<String> = raw
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(err(format!(
            "{source}: unknown catalog keys: {}",
            unknown.join(", ")
        )));
    }
    let Some(schema_version) = raw.get("schema_version") else {
        return Err(err(format!("{source}: schema_version is required")));
    };
    if schema_version.as_integer() != Some(CATALOG_SCHEMA_VERSION) {
        return Err(err(format!(
            "{source}: unsupported schema_version: {}",
            toml_repr(schema_version)
        )));
    }
    raw_providers(raw)?;
    Ok(())
}

fn raw_providers(raw: &toml::Table) -> Result<Vec<toml::Table>, CatalogError> {
    let Some(providers) = raw.get("providers") else {
        return Ok(Vec::new());
    };
    let Some(array) = providers.as_array() else {
        return Err(err(
            "catalog providers must be an array of tables ([[providers]])",
        ));
    };
    let mut tables = Vec::with_capacity(array.len());
    for item in array {
        let Some(table) = item.as_table() else {
            return Err(err(
                "catalog providers must be an array of tables ([[providers]])",
            ));
        };
        tables.push(table.clone());
    }
    Ok(tables)
}

fn raw_provider_name(provider: &toml::Table) -> Result<String, CatalogError> {
    match provider.get("name").and_then(toml::Value::as_str) {
        Some(name) if !name.trim().is_empty() => Ok(name.trim().to_string()),
        _ => Err(err(
            "catalog provider entries must have a non-empty string name",
        )),
    }
}

fn entries_from_raw(
    raw: &toml::Table,
    source: &str,
) -> Result<Vec<ProviderCatalogEntry>, CatalogError> {
    match raw.get("schema_version").and_then(toml::Value::as_integer) {
        Some(CATALOG_SCHEMA_VERSION) => {}
        _ => return Err(err(format!("{source}: schema_version: must be 1"))),
    }
    let providers = raw_providers(raw)?;
    let mut entries = Vec::with_capacity(providers.len());
    for (index, provider) in providers.iter().enumerate() {
        entries.push(entry_from_provider(provider, index, source)?);
    }
    let mut seen: IndexMap<String, usize> = IndexMap::new();
    for entry in &entries {
        *seen.entry(entry.name.clone()).or_insert(0) += 1;
    }
    let mut duplicates: Vec<String> = seen
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect();
    if !duplicates.is_empty() {
        duplicates.sort();
        return Err(err(format!(
            "{source}: duplicate provider names: {}",
            duplicates.join(", ")
        )));
    }
    Ok(entries)
}

const PROVIDER_KEYS: [&str; 19] = [
    "name",
    "display_name",
    "kind",
    "base_url",
    "api_key_env",
    "credential_name",
    "models",
    "default_model",
    "docs_url",
    "api",
    "context_windows",
    "headers",
    "compat",
    "model_metadata",
    "thinking_levels",
    "thinking_models",
    "thinking_default",
    "thinking_parameter",
    "auth_methods",
];

#[allow(clippy::too_many_lines)]
fn entry_from_provider(
    provider: &toml::Table,
    index: usize,
    source: &str,
) -> Result<ProviderCatalogEntry, CatalogError> {
    // Determine the location label used in error strings (name, or index when
    // the name is unusable — mirrors tau's `_dotted_location`).
    let name_label = provider
        .get("name")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| index.to_string(), |value| value.trim().to_string());
    let loc = |field: &str| format!("{source}: providers.{name_label}.{field}");
    let base_loc = format!("{source}: providers.{name_label}");

    reject_extra_keys(provider, &PROVIDER_KEYS, &base_loc)?;

    let name = req_non_empty_string(provider, "name", &loc("name"))?;
    let display_name = req_non_empty_string(provider, "display_name", &loc("display_name"))?;
    let kind = req_literal(provider, "kind", &KIND_VALUES, &loc("kind"))?;
    let base_url = req_non_empty_string(provider, "base_url", &loc("base_url"))?;
    let api_key_env = req_non_empty_string(provider, "api_key_env", &loc("api_key_env"))?;
    let credential_name =
        opt_non_empty_string(provider, "credential_name", &loc("credential_name"))?;
    let models = req_non_empty_string_tuple(provider, "models", &loc("models"))?;
    let default_model = req_non_empty_string(provider, "default_model", &loc("default_model"))?;
    let docs_url = req_non_empty_string(provider, "docs_url", &loc("docs_url"))?;
    let api = opt_literal(provider, "api", &API_VALUES, &loc("api"))?;
    let context_windows =
        opt_positive_int_map(provider, "context_windows", &loc("context_windows"))?;
    let headers = string_map(provider, "headers", &loc("headers"))?;
    let compat = json_object(provider, "compat", &loc("compat"))?;
    let model_metadata_raw =
        model_metadata_map(provider, "model_metadata", &loc("model_metadata"))?;
    let thinking_levels =
        opt_thinking_level_tuple(provider, "thinking_levels", &loc("thinking_levels"))?;
    let thinking_models =
        non_empty_string_tuple(provider, "thinking_models", &loc("thinking_models"))?;
    let thinking_default =
        opt_thinking_level(provider, "thinking_default", &loc("thinking_default"))?;
    let thinking_parameter = opt_literal(
        provider,
        "thinking_parameter",
        &THINKING_PARAMETER_VALUES,
        &loc("thinking_parameter"),
    )?;
    let auth_methods = auth_methods(provider, "auth_methods", &loc("auth_methods"))?;

    let prefix = format!("{source}: providers.{name}");

    if !models.contains(&default_model) {
        return Err(err(format!(
            "{prefix}.default_model: {} is not in models",
            python_repr_str(&default_model)
        )));
    }
    for model in &thinking_models {
        if !models.contains(model) {
            return Err(err(format!(
                "{prefix}.thinking_models: {} is not in models",
                python_repr_str(model)
            )));
        }
    }
    if let Some(windows) = &context_windows {
        for model in windows.keys() {
            if !models.contains(model) {
                return Err(err(format!(
                    "{prefix}.context_windows: {} is not in models",
                    python_repr_str(model)
                )));
            }
        }
    }
    for model in model_metadata_raw.keys() {
        if !models.contains(model) {
            return Err(err(format!(
                "{prefix}.model_metadata: {} is not in models",
                python_repr_str(model)
            )));
        }
    }
    if let Some(default) = &thinking_default {
        let ok = thinking_levels
            .as_ref()
            .is_some_and(|levels| levels.contains(default));
        if !ok {
            return Err(err(format!(
                "{prefix}.thinking_default: {} is not in thinking_levels",
                python_repr_str(default)
            )));
        }
    }

    for (model, metadata) in &model_metadata_raw {
        validate_cost_tiers(
            &metadata.cost_tiers,
            &format!("{prefix}.model_metadata.{model}"),
        )?;
    }

    let model_metadata: IndexMap<String, ModelCatalogMetadata> = model_metadata_raw
        .into_iter()
        .map(|(model, metadata)| (model, model_metadata_from_provider(metadata)))
        .collect();

    let mut effective_windows = context_windows.unwrap_or_default();
    for (model, metadata) in &model_metadata {
        if let Some(window) = metadata.context_window {
            if !effective_windows.contains_key(model) {
                effective_windows.insert(model.clone(), window);
            }
        }
    }
    let context_windows = if effective_windows.is_empty() {
        None
    } else {
        Some(effective_windows)
    };

    Ok(ProviderCatalogEntry {
        name,
        display_name,
        kind,
        base_url,
        api_key_env,
        credential_name,
        models,
        default_model,
        docs_url,
        api,
        context_windows,
        headers,
        compat,
        model_metadata,
        thinking_levels,
        thinking_models,
        thinking_default,
        thinking_parameter,
        auth_methods,
    })
}

/// A raw catalog metadata block, before thinking-map folding (tau
/// `_CatalogModelMetadata`).
struct RawCatalogMetadata {
    name: Option<String>,
    api: Option<String>,
    base_url: Option<String>,
    reasoning: Option<bool>,
    input: Vec<String>,
    cost: Option<IndexMap<String, f64>>,
    cost_tiers: Vec<ModelCostTier>,
    context_window: Option<i64>,
    max_tokens: Option<i64>,
    headers: IndexMap<String, String>,
    compat: JsonMap,
    thinking_level_map: IndexMap<String, String>,
    unsupported_thinking_levels: Vec<String>,
}

fn model_metadata_from_provider(metadata: RawCatalogMetadata) -> ModelCatalogMetadata {
    let mut thinking_level_map: ThinkingLevelMap = metadata
        .thinking_level_map
        .into_iter()
        .map(|(level, value)| (level, Some(value)))
        .collect();
    for level in metadata.unsupported_thinking_levels {
        thinking_level_map.insert(level, None);
    }
    ModelCatalogMetadata {
        name: metadata.name,
        api: metadata.api,
        base_url: metadata.base_url,
        reasoning: metadata.reasoning,
        input: metadata.input,
        cost: metadata.cost.filter(|cost| !cost.is_empty()),
        cost_tiers: metadata.cost_tiers,
        context_window: metadata.context_window,
        max_tokens: metadata.max_tokens,
        headers: metadata.headers,
        compat: metadata.compat,
        thinking_level_map,
    }
}

fn validate_cost_tiers(tiers: &[ModelCostTier], field_name: &str) -> Result<(), CatalogError> {
    if tiers.is_empty() {
        return Ok(());
    }
    if tiers[tiers.len() - 1].max_input_tokens.is_some() {
        return Err(err(format!(
            "{field_name}.cost_tiers: final tier must omit max_input_tokens"
        )));
    }
    let mut previous_limit = 0i64;
    for (index, tier) in tiers[..tiers.len() - 1].iter().enumerate() {
        match tier.max_input_tokens {
            Some(limit) if limit > previous_limit => previous_limit = limit,
            _ => {
                return Err(err(format!(
                    "{field_name}.cost_tiers.{index}.max_input_tokens: limits must be strictly increasing"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Field validators (tau's pydantic strict types, by hand)
// ---------------------------------------------------------------------------

fn reject_extra_keys(
    table: &toml::Table,
    allowed: &[&str],
    base_loc: &str,
) -> Result<(), CatalogError> {
    let mut unknown: Vec<String> = table
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(err(format!(
            "{base_loc}: unknown fields: {}",
            unknown.join(", ")
        )));
    }
    Ok(())
}

fn req_non_empty_string(table: &toml::Table, key: &str, loc: &str) -> Result<String, CatalogError> {
    match table.get(key) {
        Some(toml::Value::String(value)) if !value.trim().is_empty() => {
            Ok(value.trim().to_string())
        }
        Some(_) => Err(err(format!("{loc}: must be a non-empty string"))),
        None => Err(err(format!("{loc}: field required"))),
    }
}

fn opt_non_empty_string(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Option<String>, CatalogError> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.trim().to_string()))
        }
        Some(_) => Err(err(format!("{loc}: must be a non-empty string"))),
    }
}

fn req_literal(
    table: &toml::Table,
    key: &str,
    allowed: &[&str],
    loc: &str,
) -> Result<String, CatalogError> {
    match table.get(key) {
        Some(toml::Value::String(value)) if allowed.contains(&value.as_str()) => Ok(value.clone()),
        Some(_) => Err(err(format!(
            "{loc}: must be one of: {}",
            allowed.join(", ")
        ))),
        None => Err(err(format!("{loc}: field required"))),
    }
}

fn opt_literal(
    table: &toml::Table,
    key: &str,
    allowed: &[&str],
    loc: &str,
) -> Result<Option<String>, CatalogError> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) if allowed.contains(&value.as_str()) => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(err(format!(
            "{loc}: must be one of: {}",
            allowed.join(", ")
        ))),
    }
}

fn req_non_empty_string_tuple(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Vec<String>, CatalogError> {
    let values = non_empty_string_tuple(table, key, loc)?;
    if values.is_empty() {
        return Err(err(format!("{loc}: must be a non-empty list")));
    }
    Ok(values)
}

fn non_empty_string_tuple(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Vec<String>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        return Err(err(format!("{loc}: must be a list of strings")));
    };
    let mut items = Vec::with_capacity(array.len());
    for item in array {
        match item {
            toml::Value::String(text) if !text.trim().is_empty() => {
                items.push(text.trim().to_string());
            }
            _ => return Err(err(format!("{loc}: must be a list of non-empty strings"))),
        }
    }
    Ok(items)
}

fn opt_positive_int_map(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Option<IndexMap<String, i64>>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = IndexMap::new();
    for (raw_key, raw_value) in inner {
        if raw_key.trim().is_empty() {
            return Err(err(format!("{loc}: keys must be non-empty strings")));
        }
        map.insert(raw_key.clone(), positive_int(raw_value, loc)?);
    }
    Ok(Some(map))
}

fn positive_int(value: &toml::Value, loc: &str) -> Result<i64, CatalogError> {
    match value {
        toml::Value::Integer(int) if *int > 0 => Ok(*int),
        _ => Err(err(format!("{loc}: must be a positive integer"))),
    }
}

fn opt_positive_int(value: &toml::Value, loc: &str) -> Result<Option<i64>, CatalogError> {
    match value {
        toml::Value::Integer(int) if *int > 0 => Ok(Some(*int)),
        _ => Err(err(format!("{loc}: must be a positive integer"))),
    }
}

fn non_negative_float(value: &toml::Value, loc: &str) -> Result<f64, CatalogError> {
    match value {
        toml::Value::Float(float) if *float >= 0.0 => Ok(*float),
        toml::Value::Integer(int) if *int >= 0 =>
        {
            #[allow(clippy::cast_precision_loss)]
            Ok(*int as f64)
        }
        _ => Err(err(format!("{loc}: must be a non-negative number"))),
    }
}

fn string_map(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<IndexMap<String, String>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(IndexMap::new());
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = IndexMap::new();
    for (raw_key, raw_value) in inner {
        if raw_key.trim().is_empty() {
            return Err(err(format!("{loc}: keys must be non-empty strings")));
        }
        match raw_value {
            toml::Value::String(text) if !text.trim().is_empty() => {
                map.insert(raw_key.clone(), text.clone());
            }
            _ => return Err(err(format!("{loc}: values must be non-empty strings"))),
        }
    }
    Ok(map)
}

fn float_map(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<IndexMap<String, f64>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(IndexMap::new());
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = IndexMap::new();
    for (raw_key, raw_value) in inner {
        if raw_key.trim().is_empty() {
            return Err(err(format!("{loc}: keys must be non-empty strings")));
        }
        map.insert(raw_key.clone(), non_negative_float(raw_value, loc)?);
    }
    Ok(map)
}

fn json_object(table: &toml::Table, key: &str, loc: &str) -> Result<JsonMap, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(JsonMap::new());
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = JsonMap::new();
    for (raw_key, raw_value) in inner {
        if raw_key.trim().is_empty() {
            return Err(err(format!("{loc}: keys must be non-empty strings")));
        }
        map.insert(raw_key.clone(), toml_to_json(raw_value, loc)?);
    }
    Ok(map)
}

fn toml_to_json(value: &toml::Value, loc: &str) -> Result<JsonValue, CatalogError> {
    Ok(match value {
        toml::Value::String(text) => JsonValue::String(text.clone()),
        toml::Value::Integer(int) => JsonValue::from(*int),
        toml::Value::Float(float) => {
            serde_json::Number::from_f64(*float).map_or(JsonValue::Null, JsonValue::Number)
        }
        toml::Value::Boolean(boolean) => JsonValue::Bool(*boolean),
        toml::Value::Array(array) => {
            let mut items = Vec::with_capacity(array.len());
            for item in array {
                items.push(toml_to_json(item, loc)?);
            }
            JsonValue::Array(items)
        }
        toml::Value::Table(inner) => {
            let mut map = JsonMap::new();
            for (raw_key, raw_value) in inner {
                map.insert(raw_key.clone(), toml_to_json(raw_value, loc)?);
            }
            JsonValue::Object(map)
        }
        toml::Value::Datetime(_) => {
            return Err(err(format!("{loc}: unsupported value")));
        }
    })
}

fn opt_bool(value: &toml::Value, loc: &str) -> Result<Option<bool>, CatalogError> {
    match value {
        toml::Value::Boolean(boolean) => Ok(Some(*boolean)),
        _ => Err(err(format!("{loc}: must be a boolean"))),
    }
}

fn model_input_tuple(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Vec<String>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        return Err(err(format!("{loc}: must be a list")));
    };
    let mut items = Vec::with_capacity(array.len());
    for item in array {
        match item {
            toml::Value::String(text) if MODEL_INPUT_VALUES.contains(&text.as_str()) => {
                items.push(text.clone());
            }
            _ => return Err(err(format!("{loc}: must contain only text or image"))),
        }
    }
    Ok(items)
}

fn opt_thinking_level(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Option<String>, CatalogError> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) if THINKING_LEVELS.contains(&value.as_str()) => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(err(format!("{loc}: must be a thinking level"))),
    }
}

fn opt_thinking_level_tuple(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Option<Vec<String>>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(err(format!("{loc}: must be a list of thinking levels")));
    };
    let mut items = Vec::with_capacity(array.len());
    for item in array {
        match item {
            toml::Value::String(text) if THINKING_LEVELS.contains(&text.as_str()) => {
                items.push(text.clone());
            }
            _ => return Err(err(format!("{loc}: must be a list of thinking levels"))),
        }
    }
    Ok(Some(items))
}

fn thinking_level_map(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<IndexMap<String, String>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(IndexMap::new());
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = IndexMap::new();
    for (raw_key, raw_value) in inner {
        if !THINKING_LEVELS.contains(&raw_key.as_str()) {
            return Err(err(format!("{loc}: keys must be thinking levels")));
        }
        match raw_value {
            toml::Value::String(text) if !text.trim().is_empty() => {
                map.insert(raw_key.clone(), text.clone());
            }
            _ => return Err(err(format!("{loc}: values must be non-empty strings"))),
        }
    }
    Ok(map)
}

fn cost_tiers(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<Vec<ModelCostTier>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        return Err(err(format!("{loc}: must be a list")));
    };
    const TIER_KEYS: [&str; 5] = [
        "max_input_tokens",
        "input",
        "output",
        "cacheRead",
        "cacheWrite",
    ];
    let mut tiers = Vec::with_capacity(array.len());
    for item in array {
        let Some(tier_table) = item.as_table() else {
            return Err(err(format!("{loc}: cost tiers must be objects")));
        };
        reject_extra_keys(tier_table, &TIER_KEYS, loc)?;
        let max_input_tokens = match tier_table.get("max_input_tokens") {
            None => None,
            Some(value) => opt_positive_int(value, loc)?,
        };
        let mut cost = IndexMap::new();
        for field in ["input", "output", "cacheRead", "cacheWrite"] {
            let Some(value) = tier_table.get(field) else {
                return Err(err(format!("{loc}.{field}: field required")));
            };
            cost.insert(field.to_string(), non_negative_float(value, loc)?);
        }
        tiers.push(ModelCostTier {
            cost,
            max_input_tokens,
        });
    }
    Ok(tiers)
}

const METADATA_KEYS: [&str; 12] = [
    "name",
    "api",
    "base_url",
    "reasoning",
    "input",
    "cost",
    "cost_tiers",
    "context_window",
    "max_tokens",
    "headers",
    "compat",
    "thinking_level_map",
];
// `unsupported_thinking_levels` is also accepted (13 total).

fn model_metadata_map(
    table: &toml::Table,
    key: &str,
    loc: &str,
) -> Result<IndexMap<String, RawCatalogMetadata>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(IndexMap::new());
    };
    let Some(inner) = value.as_table() else {
        return Err(err(format!("{loc}: must be an object")));
    };
    let mut map = IndexMap::new();
    for (model, raw_metadata) in inner {
        if model.trim().is_empty() {
            return Err(err(format!("{loc}: keys must be non-empty strings")));
        }
        let Some(metadata) = raw_metadata.as_table() else {
            return Err(err(format!("{loc}.{model}: must be an object")));
        };
        let mloc = |field: &str| format!("{loc}.{model}.{field}");
        let mut allowed: Vec<&str> = METADATA_KEYS.to_vec();
        allowed.push("unsupported_thinking_levels");
        reject_extra_keys(metadata, &allowed, &format!("{loc}.{model}"))?;

        let raw = RawCatalogMetadata {
            name: opt_non_empty_string(metadata, "name", &mloc("name"))?,
            api: opt_literal(metadata, "api", &API_VALUES, &mloc("api"))?,
            base_url: opt_non_empty_string(metadata, "base_url", &mloc("base_url"))?,
            reasoning: match metadata.get("reasoning") {
                None => None,
                Some(value) => opt_bool(value, &mloc("reasoning"))?,
            },
            input: model_input_tuple(metadata, "input", &mloc("input"))?,
            cost: match metadata.get("cost") {
                None => None,
                Some(_) => Some(float_map(metadata, "cost", &mloc("cost"))?),
            },
            cost_tiers: cost_tiers(metadata, "cost_tiers", &mloc("cost_tiers"))?,
            context_window: match metadata.get("context_window") {
                None => None,
                Some(value) => opt_positive_int(value, &mloc("context_window"))?,
            },
            max_tokens: match metadata.get("max_tokens") {
                None => None,
                Some(value) => opt_positive_int(value, &mloc("max_tokens"))?,
            },
            headers: string_map(metadata, "headers", &mloc("headers"))?,
            compat: json_object(metadata, "compat", &mloc("compat"))?,
            thinking_level_map: thinking_level_map(
                metadata,
                "thinking_level_map",
                &mloc("thinking_level_map"),
            )?,
            unsupported_thinking_levels: opt_thinking_level_tuple(
                metadata,
                "unsupported_thinking_levels",
                &mloc("unsupported_thinking_levels"),
            )?
            .unwrap_or_default(),
        };
        map.insert(model.clone(), raw);
    }
    Ok(map)
}

fn auth_methods(table: &toml::Table, key: &str, loc: &str) -> Result<Vec<String>, CatalogError> {
    let Some(value) = table.get(key) else {
        return Ok(vec!["api_key".to_string()]);
    };
    let Some(array) = value.as_array() else {
        return Err(err(format!("{loc}: must be a list")));
    };
    let mut items = Vec::with_capacity(array.len());
    for item in array {
        match item {
            toml::Value::String(text) if AUTH_METHOD_VALUES.contains(&text.as_str()) => {
                items.push(text.clone());
            }
            _ => return Err(err(format!("{loc}: must contain only api_key or oauth"))),
        }
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Raw catalog merging (overlay over builtin)
// ---------------------------------------------------------------------------

fn merge_raw_catalogs(
    base: &toml::Table,
    overlay: &toml::Table,
) -> Result<toml::Table, CatalogError> {
    let base_providers = raw_providers(base)?;
    let overlay_providers = raw_providers(overlay)?;
    let mut by_name: IndexMap<String, toml::Table> = IndexMap::new();
    let mut order: Vec<String> = Vec::new();
    for provider in base_providers {
        let name = raw_provider_name(&provider)?;
        order.push(name.clone());
        by_name.insert(name, provider);
    }
    for provider in overlay_providers {
        let name = raw_provider_name(&provider)?;
        if let Some(existing) = by_name.get(&name) {
            let merged = merge_raw_provider(existing, &provider);
            by_name.insert(name, merged);
        } else {
            order.push(name.clone());
            by_name.insert(name, provider);
        }
    }
    let schema_version = overlay
        .get("schema_version")
        .or_else(|| base.get("schema_version"))
        .cloned()
        .unwrap_or(toml::Value::Integer(CATALOG_SCHEMA_VERSION));
    let providers: Vec<toml::Value> = order
        .iter()
        .map(|name| toml::Value::Table(by_name[name].clone()))
        .collect();
    let mut merged = toml::Table::new();
    merged.insert("schema_version".to_string(), schema_version);
    merged.insert("providers".to_string(), toml::Value::Array(providers));
    Ok(merged)
}

fn merge_tables(base: &toml::Table, overlay: &toml::Table) -> toml::Table {
    let mut merged = base.clone();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn merge_raw_provider(base: &toml::Table, overlay: &toml::Table) -> toml::Table {
    let mut merged = merge_tables(base, overlay);

    if let (Some(base_models), Some(overlay_models)) = (
        base.get("models").and_then(toml::Value::as_array),
        overlay.get("models").and_then(toml::Value::as_array),
    ) {
        let mut ordered: Vec<toml::Value> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for item in overlay_models.iter().chain(base_models.iter()) {
            if let Some(text) = item.as_str() {
                if !seen.iter().any(|s| s == text) {
                    seen.push(text.to_string());
                    ordered.push(item.clone());
                }
            }
        }
        merged.insert("models".to_string(), toml::Value::Array(ordered));
    }

    for key in ["context_windows", "headers", "compat"] {
        if let (Some(base_map), Some(overlay_map)) = (
            base.get(key).and_then(toml::Value::as_table),
            overlay.get(key).and_then(toml::Value::as_table),
        ) {
            merged.insert(
                key.to_string(),
                toml::Value::Table(merge_tables(base_map, overlay_map)),
            );
        }
    }

    if let (Some(base_metadata), Some(overlay_metadata)) = (
        base.get("model_metadata").and_then(toml::Value::as_table),
        overlay
            .get("model_metadata")
            .and_then(toml::Value::as_table),
    ) {
        merged.insert(
            "model_metadata".to_string(),
            toml::Value::Table(merge_model_metadata(base_metadata, overlay_metadata)),
        );
    }

    if overlay.contains_key("thinking_levels") {
        for field in THINKING_FIELDS {
            if let Some(value) = overlay.get(field) {
                merged.insert(field.to_string(), value.clone());
            } else {
                merged.remove(field);
            }
        }
    }
    merged
}

fn merge_model_metadata(base: &toml::Table, overlay: &toml::Table) -> toml::Table {
    let mut merged = base.clone();
    for (model, overlay_metadata) in overlay {
        match (
            merged.get(model).and_then(toml::Value::as_table).cloned(),
            overlay_metadata.as_table(),
        ) {
            (Some(base_metadata), Some(overlay_table)) => {
                let mut next = merge_tables(&base_metadata, overlay_table);
                for key in ["headers", "compat", "thinking_level_map"] {
                    if let (Some(base_map), Some(overlay_map)) = (
                        base_metadata.get(key).and_then(toml::Value::as_table),
                        overlay_table.get(key).and_then(toml::Value::as_table),
                    ) {
                        next.insert(
                            key.to_string(),
                            toml::Value::Table(merge_tables(base_map, overlay_map)),
                        );
                    }
                }
                merged.insert(model.clone(), toml::Value::Table(next));
            }
            _ => {
                merged.insert(model.clone(), overlay_metadata.clone());
            }
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// Raw serialization (entry -> catalog.toml)
// ---------------------------------------------------------------------------

fn raw_provider_from_entry(entry: &ProviderCatalogEntry) -> toml::Table {
    let mut raw = toml::Table::new();
    raw.insert("name".to_string(), s(&entry.name));
    raw.insert("display_name".to_string(), s(&entry.display_name));
    raw.insert("kind".to_string(), s(&entry.kind));
    raw.insert("base_url".to_string(), s(&entry.base_url));
    raw.insert("api_key_env".to_string(), s(&entry.api_key_env));
    raw.insert(
        "models".to_string(),
        toml::Value::Array(entry.models.iter().map(|m| s(m)).collect()),
    );
    raw.insert("default_model".to_string(), s(&entry.default_model));
    raw.insert("docs_url".to_string(), s(&entry.docs_url));
    if let Some(api) = &entry.api {
        raw.insert("api".to_string(), s(api));
    }
    if let Some(credential_name) = &entry.credential_name {
        raw.insert("credential_name".to_string(), s(credential_name));
    }
    if let Some(windows) = &entry.context_windows {
        if !windows.is_empty() {
            let mut table = toml::Table::new();
            for (model, window) in windows {
                table.insert(model.clone(), toml::Value::Integer(*window));
            }
            raw.insert("context_windows".to_string(), toml::Value::Table(table));
        }
    }
    if !entry.headers.is_empty() {
        raw.insert("headers".to_string(), string_map_to_toml(&entry.headers));
    }
    if !entry.compat.is_empty() {
        raw.insert("compat".to_string(), json_to_toml(&entry.compat));
    }
    if !entry.model_metadata.is_empty() {
        let mut table = toml::Table::new();
        for (model, metadata) in &entry.model_metadata {
            table.insert(
                model.clone(),
                toml::Value::Table(raw_model_metadata_from_entry(metadata)),
            );
        }
        raw.insert("model_metadata".to_string(), toml::Value::Table(table));
    }
    if let Some(levels) = &entry.thinking_levels {
        raw.insert(
            "thinking_levels".to_string(),
            toml::Value::Array(levels.iter().map(|l| s(l)).collect()),
        );
    }
    if !entry.thinking_models.is_empty() {
        raw.insert(
            "thinking_models".to_string(),
            toml::Value::Array(entry.thinking_models.iter().map(|m| s(m)).collect()),
        );
    }
    if let Some(default) = &entry.thinking_default {
        raw.insert("thinking_default".to_string(), s(default));
    }
    if let Some(parameter) = &entry.thinking_parameter {
        raw.insert("thinking_parameter".to_string(), s(parameter));
    }
    if entry.auth_methods != vec!["api_key".to_string()] {
        raw.insert(
            "auth_methods".to_string(),
            toml::Value::Array(entry.auth_methods.iter().map(|m| s(m)).collect()),
        );
    }
    raw
}

fn raw_model_metadata_from_entry(metadata: &ModelCatalogMetadata) -> toml::Table {
    let mut raw = toml::Table::new();
    if let Some(name) = &metadata.name {
        raw.insert("name".to_string(), s(name));
    }
    if let Some(api) = &metadata.api {
        raw.insert("api".to_string(), s(api));
    }
    if let Some(base_url) = &metadata.base_url {
        raw.insert("base_url".to_string(), s(base_url));
    }
    if let Some(reasoning) = metadata.reasoning {
        raw.insert("reasoning".to_string(), toml::Value::Boolean(reasoning));
    }
    if !metadata.input.is_empty() {
        raw.insert(
            "input".to_string(),
            toml::Value::Array(metadata.input.iter().map(|i| s(i)).collect()),
        );
    }
    if let Some(cost) = &metadata.cost {
        if !cost.is_empty() {
            raw.insert("cost".to_string(), float_map_to_toml(cost));
        }
    }
    if !metadata.cost_tiers.is_empty() {
        let tiers: Vec<toml::Value> = metadata
            .cost_tiers
            .iter()
            .map(|tier| {
                let mut table = toml::Table::new();
                if let Some(limit) = tier.max_input_tokens {
                    table.insert("max_input_tokens".to_string(), toml::Value::Integer(limit));
                }
                for (field, value) in &tier.cost {
                    table.insert(field.clone(), toml::Value::Float(*value));
                }
                toml::Value::Table(table)
            })
            .collect();
        raw.insert("cost_tiers".to_string(), toml::Value::Array(tiers));
    }
    if let Some(context_window) = metadata.context_window {
        raw.insert(
            "context_window".to_string(),
            toml::Value::Integer(context_window),
        );
    }
    if let Some(max_tokens) = metadata.max_tokens {
        raw.insert("max_tokens".to_string(), toml::Value::Integer(max_tokens));
    }
    if !metadata.headers.is_empty() {
        raw.insert("headers".to_string(), string_map_to_toml(&metadata.headers));
    }
    if !metadata.compat.is_empty() {
        raw.insert("compat".to_string(), json_to_toml(&metadata.compat));
    }
    let mut level_map = toml::Table::new();
    let mut unsupported = Vec::new();
    for (level, value) in &metadata.thinking_level_map {
        match value {
            Some(text) => {
                level_map.insert(level.clone(), s(text));
            }
            None => unsupported.push(s(level)),
        }
    }
    if !level_map.is_empty() {
        raw.insert(
            "thinking_level_map".to_string(),
            toml::Value::Table(level_map),
        );
    }
    if !unsupported.is_empty() {
        raw.insert(
            "unsupported_thinking_levels".to_string(),
            toml::Value::Array(unsupported),
        );
    }
    raw
}

fn s(value: &str) -> toml::Value {
    toml::Value::String(value.to_string())
}

fn string_map_to_toml(map: &IndexMap<String, String>) -> toml::Value {
    let mut table = toml::Table::new();
    for (key, value) in map {
        table.insert(key.clone(), s(value));
    }
    toml::Value::Table(table)
}

fn float_map_to_toml(map: &IndexMap<String, f64>) -> toml::Value {
    let mut table = toml::Table::new();
    for (key, value) in map {
        table.insert(key.clone(), toml::Value::Float(*value));
    }
    toml::Value::Table(table)
}

fn json_to_toml(map: &JsonMap) -> toml::Value {
    let mut table = toml::Table::new();
    for (key, value) in map {
        table.insert(key.clone(), json_value_to_toml(value));
    }
    toml::Value::Table(table)
}

fn json_value_to_toml(value: &JsonValue) -> toml::Value {
    match value {
        JsonValue::Null => toml::Value::String(String::new()),
        JsonValue::Bool(boolean) => toml::Value::Boolean(*boolean),
        JsonValue::Number(number) => number.as_i64().map_or_else(
            || toml::Value::Float(number.as_f64().unwrap_or(0.0)),
            toml::Value::Integer,
        ),
        JsonValue::String(text) => toml::Value::String(text.clone()),
        JsonValue::Array(items) => {
            toml::Value::Array(items.iter().map(json_value_to_toml).collect())
        }
        JsonValue::Object(map) => json_to_toml(map),
    }
}

// ---------------------------------------------------------------------------
// TOML text emission (tau `_catalog_to_toml`)
// ---------------------------------------------------------------------------

fn catalog_to_toml(raw: &toml::Table) -> Result<String, CatalogError> {
    let schema_version = raw
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(CATALOG_SCHEMA_VERSION);
    let mut lines: Vec<String> = vec![format!("schema_version = {schema_version}"), String::new()];
    for provider in raw_providers(raw)? {
        lines.push("[[providers]]".to_string());
        for key in [
            "name",
            "display_name",
            "kind",
            "base_url",
            "api_key_env",
            "credential_name",
            "models",
            "default_model",
            "docs_url",
            "api",
            "headers",
            "compat",
            "thinking_levels",
            "thinking_models",
            "thinking_default",
            "thinking_parameter",
            "auth_methods",
        ] {
            if let Some(value) = provider.get(key) {
                lines.push(format!("{key} = {}", toml_value_text(value)?));
            }
        }
        if let Some(context_windows) = provider
            .get("context_windows")
            .and_then(toml::Value::as_table)
        {
            if !context_windows.is_empty() {
                lines.push(String::new());
                lines.push("[providers.context_windows]".to_string());
                for (model, window) in context_windows {
                    lines.push(format!(
                        "{} = {}",
                        toml_key(model),
                        toml_value_text(window)?
                    ));
                }
            }
        }
        if let Some(model_metadata) = provider
            .get("model_metadata")
            .and_then(toml::Value::as_table)
        {
            if !model_metadata.is_empty() {
                for (model, metadata) in model_metadata {
                    let Some(metadata) = metadata.as_table() else {
                        continue;
                    };
                    lines.push(String::new());
                    lines.push(format!("[providers.model_metadata.{}]", toml_key(model)));
                    for (key, value) in metadata {
                        lines.push(format!("{key} = {}", toml_value_text(value)?));
                    }
                }
            }
        }
        lines.push(String::new());
    }
    Ok(format!("{}\n", crate::pystr::py_rstrip(&lines.join("\n"))))
}

fn toml_key(value: &str) -> String {
    let stripped: String = value.chars().filter(|&c| c != '_' && c != '-').collect();
    let first_is_digit = value.chars().next().is_some_and(|c| c.is_ascii_digit());
    if !stripped.is_empty() && stripped.chars().all(char::is_alphanumeric) && !first_is_digit {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
    }
}

fn toml_value_text(value: &toml::Value) -> Result<String, CatalogError> {
    Ok(match value {
        toml::Value::String(text) => {
            serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
        }
        toml::Value::Boolean(boolean) => {
            if *boolean {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        toml::Value::Integer(int) => int.to_string(),
        toml::Value::Float(float) => crate::pystr::python_float_repr(*float),
        toml::Value::Array(array) => {
            let mut parts = Vec::with_capacity(array.len());
            for item in array {
                parts.push(toml_value_text(item)?);
            }
            format!("[{}]", parts.join(", "))
        }
        toml::Value::Table(table) => {
            let mut parts = Vec::with_capacity(table.len());
            for (key, item) in table {
                parts.push(format!("{} = {}", toml_key(key), toml_value_text(item)?));
            }
            format!("{{ {} }}", parts.join(", "))
        }
        toml::Value::Datetime(_) => {
            return Err(err("Unsupported TOML value: datetime"));
        }
    })
}

fn atomic_write_text(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().map_or_else(
        || "catalog".to_string(),
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

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn python_repr_str(value: &str) -> String {
    crate::pystr::python_repr(&JsonValue::String(value.to_string()))
}

fn toml_repr(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => python_repr_str(text),
        toml::Value::Integer(int) => int.to_string(),
        toml::Value::Float(float) => crate::pystr::python_float_repr(*float),
        toml::Value::Boolean(boolean) => {
            if *boolean {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        other => format!("{other}"),
    }
}
