//! Port of tau's `tests/test_provider_config.py`.
//!
//! ## Environment-dependent cases
//!
//! rho builds on edition 2024 with `unsafe_code = "forbid"`, so the tests cannot
//! call `std::env::set_var` (it is `unsafe`). tau's `monkeypatch.setenv/delenv`
//! cases are ported by injecting a fake [`CredentialReader`] (which supersedes
//! the env-var lookup) instead of mutating the process environment. Two cases
//! whose entire point is the env-var path are documented as skipped:
//!
//! - `test_openai_compatible_config_from_provider_preserves_openai_base_url_env`
//!   (requires `OPENAI_BASE_URL` to be set — no safe way to set it here).
//! - `test_openai_compatible_config_from_provider_falls_back_to_env_when_stored_missing`
//!   (requires `OPENROUTER_API_KEY` to be set to observe the env fallback).
//!
//! `test_provider_has_usable_credentials_checks_stored_key_and_env` is ported
//! with a guaranteed-unset env-var name so the "no credentials" branch is
//! deterministic; the "env var present ⇒ usable" assertion is dropped for the
//! same set_var reason.

#![allow(
    clippy::unreadable_literal,
    clippy::float_cmp,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::manual_let_else
)]

use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;
use rho_coding::paths::RhoPaths;
use rho_coding::provider_config::{
    AnthropicProviderConfig, CredentialReader, DEFAULT_MODEL, OpenAICompatibleProviderConfig,
    ProviderConfig, ProviderSettings, ScopedModelConfig, anthropic_config_from_provider,
    load_provider_settings, openai_compatible_config_from_provider,
    provider_default_thinking_level, provider_has_usable_credentials, provider_settings_from_json,
    provider_thinking_levels, provider_thinking_unavailable_reason, resolve_provider_selection,
    resolve_startup_thinking_level, save_provider_settings, set_default_provider_model,
    set_provider_thinking_level, upsert_openai_compatible_provider,
};

/// A test credential store keyed by credential name (stands in for tau's fake
/// `FakeCredentials` and, where OAuth is needed, `FileCredentialStore`).
#[derive(Default)]
struct FakeCredentials {
    keys: HashMap<String, String>,
    oauth: HashMap<String, String>,
}

impl FakeCredentials {
    fn with_key(name: &str, key: &str) -> Self {
        let mut store = Self::default();
        store.keys.insert(name.to_string(), key.to_string());
        store
    }
}

impl CredentialReader for FakeCredentials {
    fn get(&self, name: &str) -> Option<String> {
        self.keys.get(name).cloned()
    }
    fn get_oauth(&self, name: &str) -> Option<String> {
        self.oauth.get(name).cloned()
    }
}

fn paths_for(home: &Path) -> RhoPaths {
    RhoPaths::new(home.to_path_buf(), home.join(".agents"))
}

fn as_compatible(provider: &ProviderConfig) -> &OpenAICompatibleProviderConfig {
    match provider {
        ProviderConfig::OpenAICompatible(config) => config,
        _ => panic!("expected OpenAI-compatible provider"),
    }
}

fn strvec(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const EXPECTED_PROVIDER_NAMES: [&str; 28] = [
    "openai",
    "openai-codex",
    "anthropic",
    "google",
    "deepseek",
    "xai",
    "groq",
    "cerebras",
    "nvidia",
    "openrouter",
    "zai",
    "mistral",
    "minimax",
    "minimax-cn",
    "moonshotai",
    "kimi-code",
    "moonshotai-cn",
    "huggingface",
    "fireworks",
    "together",
    "vercel-ai-gateway",
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
    "opencode-go",
    "opencode",
    "github-copilot",
];

#[test]
fn load_provider_settings_missing_file_uses_openai_default() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_for(&tmp.path().join(".tau"));
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    assert_eq!(settings.default_provider, "openai");
    let names: Vec<&str> = settings
        .providers
        .iter()
        .map(ProviderConfig::name)
        .collect();
    assert_eq!(names, EXPECTED_PROVIDER_NAMES);
    assert_eq!(settings.providers[0].default_model(), DEFAULT_MODEL);
    assert_eq!(
        settings
            .get_provider(Some("anthropic"))
            .unwrap()
            .api_key_env(),
        "ANTHROPIC_API_KEY"
    );
    assert_eq!(
        settings
            .get_provider(Some("openrouter"))
            .unwrap()
            .api_key_env(),
        "OPENROUTER_API_KEY"
    );
    assert_eq!(
        settings
            .get_provider(Some("huggingface"))
            .unwrap()
            .api_key_env(),
        "HF_TOKEN"
    );
}

#[test]
fn builtin_openai_declares_model_scoped_thinking_capabilities() {
    let settings = ProviderSettings::default();
    let openai = settings.get_provider(Some("openai")).unwrap();
    let openrouter = settings.get_provider(Some("openrouter")).unwrap();
    let huggingface = settings.get_provider(Some("huggingface")).unwrap();
    let codex = settings.get_provider(Some("openai-codex")).unwrap();
    let anthropic = settings.get_provider(Some("anthropic")).unwrap();

    assert_eq!(openai.context_windows()["gpt-5.5"], 272_000);
    assert_eq!(openai.context_windows()["gpt-5.5-pro"], 1_050_000);
    assert_eq!(anthropic.context_windows()["claude-sonnet-4-6"], 1_000_000);
    assert_eq!(openrouter.context_windows()["openai/gpt-5.5"], 1_050_000);
    assert_eq!(
        provider_thinking_levels(openai, Some("gpt-5.5")),
        strvec(&["off", "low", "medium", "high", "xhigh"])
    );
    assert_eq!(
        provider_default_thinking_level(openai, Some("gpt-5.5")).as_deref(),
        Some("medium")
    );
    assert_eq!(
        provider_thinking_unavailable_reason(openai, Some("gpt-5.5")),
        None
    );
    assert!(provider_thinking_levels(openai, Some("gpt-4.1")).is_empty());
    assert_eq!(
        provider_thinking_unavailable_reason(openai, Some("gpt-4.1")).as_deref(),
        Some("openai:gpt-4.1 is not a reasoning model")
    );
    assert_eq!(
        provider_thinking_levels(openrouter, Some("openai/gpt-5.5")),
        strvec(&["off", "minimal", "low", "medium", "high", "xhigh"])
    );
    assert_eq!(
        provider_thinking_unavailable_reason(openrouter, Some("openai/gpt-5.5")),
        None
    );
    assert_eq!(
        provider_thinking_levels(openrouter, Some("anthropic/claude-sonnet-4.6")),
        strvec(&["off", "minimal", "low", "medium", "high"])
    );
    assert_eq!(
        provider_thinking_unavailable_reason(openrouter, Some("anthropic/claude-sonnet-4.6")),
        None
    );
    assert_eq!(
        provider_thinking_levels(huggingface, Some("MiniMaxAI/MiniMax-M2.7")),
        strvec(&["off", "minimal", "low", "medium", "high"])
    );
    assert_eq!(
        provider_thinking_unavailable_reason(huggingface, Some("MiniMaxAI/MiniMax-M2.7")),
        None
    );
    assert_eq!(
        provider_thinking_levels(codex, Some("gpt-5.5")),
        strvec(&["off", "minimal", "low", "medium", "high", "xhigh"])
    );
    assert_eq!(
        provider_thinking_unavailable_reason(codex, Some("gpt-5.5")),
        None
    );
    assert_eq!(
        provider_thinking_levels(anthropic, Some("claude-sonnet-4-6")),
        strvec(&["off", "low", "medium", "high"])
    );
    assert_eq!(
        provider_thinking_unavailable_reason(anthropic, Some("claude-sonnet-4-6")),
        None
    );
    assert_eq!(
        provider_thinking_levels(anthropic, Some("claude-haiku-4-5")),
        strvec(&["off", "minimal", "low", "medium", "high"])
    );
}

#[test]
fn load_provider_settings_accepts_provider_preferences_with_user_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("catalog.toml"),
        r#"schema_version = 1

[[providers]]
name = "local"
display_name = "local"
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
api_key_env = "LOCAL_API_KEY"
models = ["qwen", "llama"]
default_model = "qwen"
docs_url = "http://localhost:11434/v1""#,
    )
    .unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "local", "provider_preferences": {"local": {"default_model": "qwen", "headers": {"X-Test": "yes"}, "timeout_seconds": 12.0, "max_retries": 1, "max_retry_delay_seconds": 0.5, "thinking_defaults": {}}}, "scoped_models": [{"provider": "local", "model": "qwen"}]}"#,
    )
    .unwrap();

    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = settings.get_provider(Some("local")).unwrap();
    assert_eq!(settings.default_provider, "local");
    assert_eq!(provider.base_url(), "http://localhost:11434/v1");
    assert_eq!(provider.default_model(), "qwen");
    assert_eq!(provider.headers()["X-Test"], "yes");
    assert_eq!(provider.timeout_seconds(), 12.0);
    assert_eq!(
        settings.scoped_models,
        [ScopedModelConfig {
            provider: "local".into(),
            model: "qwen".into()
        }]
    );
}

#[test]
fn load_provider_settings_ignores_preference_without_catalog_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "openai", "provider_preferences": {"openai": {"default_model": "gpt-5-mini"}, "llama-cpp": {"default_model": "local"}}}"#,
    )
    .unwrap();
    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    assert_eq!(
        settings
            .get_provider(Some("openai"))
            .unwrap()
            .default_model(),
        "gpt-5-mini"
    );
    assert!(!settings.providers.iter().any(|p| p.name() == "llama-cpp"));
}

#[test]
fn save_provider_settings_writes_backup_when_replacing() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_for(&tmp.path().join(".tau"));
    let initial = ProviderSettings {
        default_provider: "openai".into(),
        providers: vec![ProviderConfig::OpenAICompatible(
            OpenAICompatibleProviderConfig {
                models: strvec(&["gpt-5"]),
                default_model: "gpt-5".into(),
                ..OpenAICompatibleProviderConfig::new("openai")
            },
        )],
        scoped_models: Vec::new(),
    };
    let updated = ProviderSettings {
        default_provider: "openai".into(),
        providers: vec![ProviderConfig::OpenAICompatible(
            OpenAICompatibleProviderConfig {
                models: strvec(&["gpt-5-mini"]),
                default_model: "gpt-5-mini".into(),
                ..OpenAICompatibleProviderConfig::new("openai")
            },
        )],
        scoped_models: Vec::new(),
    };

    let path = save_provider_settings(&initial, Some(&paths)).unwrap();
    save_provider_settings(&updated, Some(&paths)).unwrap();

    let backup = path.with_extension("json.bak");
    assert!(backup.exists());
    assert_eq!(
        load_provider_settings(Some(&paths), None)
            .unwrap()
            .get_provider(Some("openai"))
            .unwrap()
            .default_model(),
        "gpt-5-mini"
    );
    let backup_raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
    let backup_settings = provider_settings_from_json(&backup_raw, None).unwrap();
    assert_eq!(
        backup_settings
            .get_provider(Some("openai"))
            .unwrap()
            .default_model(),
        "gpt-5"
    );
}

#[test]
fn save_and_load_provider_settings_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_for(&tmp.path().join(".tau"));
    let mut context_windows = IndexMap::new();
    context_windows.insert("qwen".to_string(), 64_000i64);
    let mut headers = IndexMap::new();
    headers.insert("X-Test".to_string(), "enabled".to_string());
    let settings = ProviderSettings {
        default_provider: "local".into(),
        providers: vec![ProviderConfig::OpenAICompatible(
            OpenAICompatibleProviderConfig {
                base_url: "http://localhost:11434/v1".into(),
                api_key_env: "LOCAL_API_KEY".into(),
                models: strvec(&["qwen", "llama"]),
                default_model: "qwen".into(),
                context_windows,
                headers,
                timeout_seconds: 120.0,
                max_retries: 2,
                max_retry_delay_seconds: 0.5,
                ..OpenAICompatibleProviderConfig::new("local")
            },
        )],
        scoped_models: vec![ScopedModelConfig {
            provider: "local".into(),
            model: "llama".into(),
        }],
    };

    let path = save_provider_settings(&settings, Some(&paths)).unwrap();
    let loaded = load_provider_settings(Some(&paths), None).unwrap();
    assert_eq!(path, tmp.path().join(".tau").join("providers.json"));
    assert_eq!(loaded, settings);
}

fn legacy_local_metadata_cost_tiers() -> serde_json::Value {
    serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible",
            "name": "local",
            "base_url": "http://localhost:11434/v1",
            "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen"],
            "default_model": "qwen",
            "model_metadata": {"qwen": {
                "cost": {"input": 0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0},
                "cost_tiers": [
                    {"max_input_tokens": 512000, "input": 0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0},
                    {"input": 0.6, "output": 2.4, "cacheRead": 0.12, "cacheWrite": 0}
                ]
            }}
        }],
        "scoped_models": []
    })
}

#[test]
fn legacy_provider_model_cost_tiers_round_trip() {
    let raw = legacy_local_metadata_cost_tiers();
    let settings = provider_settings_from_json(&raw, None).unwrap();
    let provider = as_compatible(settings.get_provider(Some("local")).unwrap());
    let serialized = provider.model_metadata["qwen"].to_json();
    let expected = &raw["providers"][0]["model_metadata"]["qwen"]["cost_tiers"];
    // Compare numerically (tau's ints vs rho's floats: 0 == 0.0).
    let got = serialized["cost_tiers"].as_array().unwrap();
    let want = expected.as_array().unwrap();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        for key in [
            "max_input_tokens",
            "input",
            "output",
            "cacheRead",
            "cacheWrite",
        ] {
            let gv = g.get(key).and_then(serde_json::Value::as_f64);
            let wv = w.get(key).and_then(serde_json::Value::as_f64);
            assert_eq!(gv, wv, "field {key}");
        }
    }
}

fn legacy_bad_cost_tiers(cost_tiers: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible",
            "name": "local",
            "base_url": "http://localhost:11434/v1",
            "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen"],
            "default_model": "qwen",
            "model_metadata": {"qwen": {"cost_tiers": cost_tiers}}
        }]
    })
}

#[test]
fn legacy_provider_rejects_invalid_cost_tiers() {
    let cases: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!([{"max_input_tokens": 512000, "input": 0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0}]),
            "final cost tier must omit max_input_tokens",
        ),
        (
            serde_json::json!([
                {"max_input_tokens": 512000, "input": 0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0},
                {"max_input_tokens": 400000, "input": 0.4, "output": 1.6, "cacheRead": 0.08, "cacheWrite": 0},
                {"input": 0.6, "output": 2.4, "cacheRead": 0.12, "cacheWrite": 0}
            ]),
            "limits must be strictly increasing",
        ),
        (
            serde_json::json!([{"unexpected": 1, "input": 0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0}]),
            "unknown fields",
        ),
        (
            serde_json::json!([{"input": -0.3, "output": 1.2, "cacheRead": 0.06, "cacheWrite": 0}]),
            "0 or greater",
        ),
    ];
    for (cost_tiers, needle) in cases {
        let raw = legacy_bad_cost_tiers(cost_tiers);
        let err = provider_settings_from_json(&raw, None).unwrap_err();
        assert!(err.0.contains(needle), "expected {needle:?} in {:?}", err.0);
    }
}

#[test]
fn runtime_metadata_rejects_invalid_cost_tier_values() {
    use rho_coding::provider_catalog::ModelCostTier;
    use rho_coding::provider_config::ProviderModelMetadata;
    let mut model_metadata = IndexMap::new();
    let mut cost = IndexMap::new();
    for (k, v) in [
        ("input", -0.3),
        ("output", 1.2),
        ("cacheRead", 0.06),
        ("cacheWrite", 0.0),
    ] {
        cost.insert(k.to_string(), v);
    }
    model_metadata.insert(
        "qwen".to_string(),
        ProviderModelMetadata {
            cost_tiers: vec![ModelCostTier {
                cost,
                max_input_tokens: None,
            }],
            ..ProviderModelMetadata::default()
        },
    );
    let config = OpenAICompatibleProviderConfig {
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        model_metadata,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.0.contains("cost tier values must be non-negative"),
        "got: {}",
        err.0
    );
}

#[test]
fn resolve_startup_thinking_level_coerces_and_falls_back() {
    // A model whose only supported level is `xhigh` must not crash startup when
    // the global preferred level (`medium`) is unsupported — it coerces to the
    // first available level (tau `resolve_startup_thinking_level`).
    let xhigh_only = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        models: strvec(&["k3"]),
        default_model: "k3".into(),
        thinking_levels: Some(strvec(&["xhigh"])),
        thinking_parameter: Some("reasoning_effort".into()),
        ..OpenAICompatibleProviderConfig::new("kimi-code")
    });
    assert_eq!(
        resolve_startup_thinking_level(&xhigh_only, "k3", "medium").as_deref(),
        Some("xhigh")
    );

    // A remembered per-model choice wins over the global preferred level.
    let mut thinking_defaults = IndexMap::new();
    thinking_defaults.insert("k3".to_string(), "high".to_string());
    let remembered = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        models: strvec(&["k3"]),
        default_model: "k3".into(),
        thinking_levels: Some(strvec(&["low", "medium", "high"])),
        thinking_parameter: Some("reasoning_effort".into()),
        thinking_defaults,
        ..OpenAICompatibleProviderConfig::new("kimi-code")
    });
    assert_eq!(
        resolve_startup_thinking_level(&remembered, "k3", "medium").as_deref(),
        Some("high")
    );

    // The global preferred level is used when supported and not remembered.
    let no_memory = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        models: strvec(&["k3"]),
        default_model: "k3".into(),
        thinking_levels: Some(strvec(&["low", "medium", "high"])),
        thinking_parameter: Some("reasoning_effort".into()),
        ..OpenAICompatibleProviderConfig::new("kimi-code")
    });
    assert_eq!(
        resolve_startup_thinking_level(&no_memory, "k3", "low").as_deref(),
        Some("low")
    );

    // A model with no configurable thinking returns None instead of crashing.
    let plain = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        ..OpenAICompatibleProviderConfig::new("local")
    });
    assert_eq!(
        resolve_startup_thinking_level(&plain, "qwen", "medium"),
        None
    );
}

#[test]
fn provider_settings_parses_scoped_models() {
    let raw = serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible",
            "name": "local",
            "base_url": "http://localhost:11434/v1",
            "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen", "llama"],
            "default_model": "qwen",
            "context_windows": {"qwen": 64000}
        }],
        "scoped_models": [
            {"provider": "local", "model": "qwen"},
            {"provider": "local", "model": "qwen"},
            {"provider": "local", "model": "llama"}
        ]
    });
    let settings = provider_settings_from_json(&raw, None).unwrap();
    assert_eq!(
        settings
            .get_provider(Some("local"))
            .unwrap()
            .context_windows()["qwen"],
        64000
    );
    assert_eq!(
        settings.scoped_models,
        [
            ScopedModelConfig {
                provider: "local".into(),
                model: "qwen".into()
            },
            ScopedModelConfig {
                provider: "local".into(),
                model: "llama".into()
            },
        ]
    );
}

#[test]
fn upsert_openai_compatible_provider_replaces_and_sets_default() {
    let settings = ProviderSettings {
        scoped_models: vec![ScopedModelConfig {
            provider: "openai".into(),
            model: "gpt-5.5".into(),
        }],
        ..ProviderSettings::default()
    };
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let updated = upsert_openai_compatible_provider(&settings, provider, true).unwrap();
    let replaced = upsert_openai_compatible_provider(
        &updated,
        OpenAICompatibleProviderConfig {
            base_url: "http://localhost:11434/v1".into(),
            api_key_env: "LOCAL_API_KEY".into(),
            models: strvec(&["llama"]),
            default_model: "llama".into(),
            ..OpenAICompatibleProviderConfig::new("local")
        },
        true,
    )
    .unwrap();

    assert_eq!(updated.default_provider, "local");
    let mut expected_names: Vec<String> = settings
        .providers
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    expected_names.push("local".to_string());
    expected_names.sort();
    let actual_names: Vec<String> = updated
        .providers
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    assert_eq!(actual_names, expected_names);
    assert_eq!(
        replaced
            .get_provider(Some("local"))
            .unwrap()
            .default_model(),
        "llama"
    );
    assert_eq!(replaced.scoped_models, settings.scoped_models);
}

fn local_provider() -> ProviderConfig {
    ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        ..OpenAICompatibleProviderConfig::new("local")
    })
}

#[test]
fn resolve_provider_selection_uses_configured_defaults() {
    let settings = ProviderSettings {
        default_provider: "local".into(),
        providers: vec![local_provider()],
        scoped_models: Vec::new(),
    };
    let selection = resolve_provider_selection(&settings, None, None).unwrap();
    assert_eq!(selection.provider.name(), "local");
    assert_eq!(selection.model, "qwen");
}

#[test]
fn resolve_provider_selection_rejects_unknown_provider() {
    let err = resolve_provider_selection(&ProviderSettings::default(), Some("missing"), None)
        .unwrap_err();
    assert!(err.0.contains("Unknown provider"), "got: {}", err.0);
}

#[test]
fn resolve_provider_selection_rejects_model_not_declared_for_provider() {
    let settings = ProviderSettings {
        default_provider: "local".into(),
        providers: vec![local_provider()],
        scoped_models: Vec::new(),
    };
    let err = resolve_provider_selection(&settings, None, Some("llama")).unwrap_err();
    assert!(
        err.0
            .contains("Model is not configured for provider local: llama"),
        "got: {}",
        err.0
    );
}

#[test]
fn set_default_provider_model_rejects_model_not_declared_for_provider() {
    let settings = ProviderSettings {
        default_provider: "local".into(),
        providers: vec![local_provider()],
        scoped_models: Vec::new(),
    };
    let err = set_default_provider_model(&settings, "local", "llama").unwrap_err();
    assert!(
        err.0
            .contains("Model is not configured for provider local: llama"),
        "got: {}",
        err.0
    );
}

#[test]
fn openai_compatible_config_from_provider_uses_configured_credential() {
    // tau sets LOCAL_API_KEY in the env; rho injects it via a credential reader.
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1/".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        credential_name: Some("local".into()),
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let creds = FakeCredentials::with_key("local", "test-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(config.api_key, "test-key");
    assert_eq!(config.provider_name, "local");
    assert_eq!(config.base_url, "http://localhost:11434/v1");
    assert_eq!(config.headers, Some(vec![]));
    assert_eq!(config.timeout_seconds, 60.0);
    assert_eq!(config.max_retries, 2);
    assert_eq!(config.max_retry_delay_seconds, 1.0);
}

#[test]
fn openai_compatible_config_from_provider_uses_configured_timeout() {
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1/".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        credential_name: Some("local".into()),
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        timeout_seconds: 180.0,
        max_retries: 3,
        max_retry_delay_seconds: 0.25,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let creds = FakeCredentials::with_key("local", "test-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(config.timeout_seconds, 180.0);
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.max_retry_delay_seconds, 0.25);
}

#[test]
fn openai_compatible_config_from_provider_uses_configured_headers() {
    let mut headers = IndexMap::new();
    headers.insert("X-HF-Bill-To".to_string(), "my-org".to_string());
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1/".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        credential_name: Some("local".into()),
        models: strvec(&["qwen"]),
        default_model: "qwen".into(),
        headers,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let creds = FakeCredentials::with_key("local", "test-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(
        config.headers,
        Some(vec![("X-HF-Bill-To".to_string(), "my-org".to_string())])
    );
}

#[test]
fn openai_compatible_config_from_provider_sets_reasoning_effort() {
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1/".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        credential_name: Some("local".into()),
        models: strvec(&["reasoner", "plain"]),
        default_model: "reasoner".into(),
        thinking_levels: Some(strvec(&["off", "low", "high"])),
        thinking_models: strvec(&["reasoner"]),
        thinking_default: Some("low".into()),
        thinking_parameter: Some("reasoning_effort".into()),
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let creds = FakeCredentials::with_key("local", "test-key");
    let reasoner = openai_compatible_config_from_provider(
        &provider,
        Some(&creds),
        Some("reasoner"),
        Some("off"),
    )
    .unwrap();
    let plain = openai_compatible_config_from_provider(
        &provider,
        Some(&creds),
        Some("plain"),
        Some("high"),
    )
    .unwrap();
    assert_eq!(reasoner.reasoning_effort.as_deref(), Some("none"));
    assert_eq!(plain.reasoning_effort, None);
}

#[test]
fn kimi_k3_maps_xhigh_thinking_to_max() {
    let paths = paths_for(Path::new("/rho-nonexistent-home"));
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = as_compatible(settings.get_provider(Some("kimi-code")).unwrap()).clone();
    let creds = FakeCredentials::with_key("kimi-code", "test-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), Some("k3"), Some("xhigh"))
            .unwrap();
    assert_eq!(
        provider_thinking_levels(&ProviderConfig::OpenAICompatible(provider), Some("k3")),
        strvec(&["xhigh"])
    );
    assert_eq!(config.reasoning_effort.as_deref(), Some("max"));
}

#[test]
fn openai_compatible_config_from_provider_rejects_unsupported_thinking_level() {
    let provider = OpenAICompatibleProviderConfig {
        base_url: "http://localhost:11434/v1/".into(),
        api_key_env: "LOCAL_API_KEY".into(),
        credential_name: Some("local".into()),
        models: strvec(&["reasoner"]),
        default_model: "reasoner".into(),
        thinking_levels: Some(strvec(&["low", "high"])),
        thinking_parameter: Some("reasoning_effort".into()),
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let creds = FakeCredentials::with_key("local", "test-key");
    let err = match openai_compatible_config_from_provider(
        &provider,
        Some(&creds),
        Some("reasoner"),
        Some("medium"),
    ) {
        Err(err) => err,
        Ok(_) => panic!("expected thinking-level error"),
    };
    assert!(err.0.contains("not available"), "got: {}", err.0);
}

#[test]
fn openai_compatible_config_from_provider_uses_stored_credential() {
    let provider = OpenAICompatibleProviderConfig {
        base_url: "https://openrouter.ai/api/v1".into(),
        api_key_env: "OPENROUTER_API_KEY".into(),
        credential_name: Some("openrouter".into()),
        models: strvec(&["openai/gpt-4.1-mini"]),
        default_model: "openai/gpt-4.1-mini".into(),
        ..OpenAICompatibleProviderConfig::new("openrouter")
    };
    let creds = FakeCredentials::with_key("openrouter", "stored-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(config.api_key, "stored-key");
}

#[test]
fn provider_has_usable_credentials_checks_stored_key() {
    // Uses a guaranteed-unset env-var name so the "no credentials" branch is
    // deterministic (see module docs — env vars can't be set under unsafe-forbid).
    let provider = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        api_key_env: "RHO_TEST_DEFINITELY_UNSET_ENV_VAR".into(),
        credential_name: Some("openrouter".into()),
        ..OpenAICompatibleProviderConfig::new("openrouter")
    });
    let empty = FakeCredentials::default();
    let stored = FakeCredentials::with_key("openrouter", "stored-key");
    assert!(!provider_has_usable_credentials(&provider, Some(&empty)));
    assert!(provider_has_usable_credentials(&provider, Some(&stored)));
}

#[test]
fn anthropic_config_from_provider_uses_stored_credential() {
    let provider = AnthropicProviderConfig {
        credential_name: Some("anthropic".into()),
        ..AnthropicProviderConfig::new()
    };
    let creds = FakeCredentials::with_key("anthropic", "stored-anthropic-key");
    let config = anthropic_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(config.api_key, "stored-anthropic-key");
    assert_eq!(config.base_url, "https://api.anthropic.com/v1");
}

#[test]
fn anthropic_config_from_provider_sets_thinking_budget() {
    let provider = AnthropicProviderConfig {
        thinking_levels: Some(strvec(&["off", "low", "high"])),
        thinking_default: Some("low".into()),
        thinking_parameter: Some("anthropic.thinking".into()),
        ..AnthropicProviderConfig::new()
    };
    let creds = FakeCredentials::with_key("anthropic", "test-key");
    let off = anthropic_config_from_provider(&provider, Some(&creds), None, Some("off")).unwrap();
    let high = anthropic_config_from_provider(&provider, Some(&creds), None, Some("high")).unwrap();
    assert_eq!(off.thinking_budget_tokens, None);
    assert_eq!(high.thinking_budget_tokens, Some(8192));
}

#[test]
fn openai_compatible_config_from_provider_sets_reasoning_parameter() {
    for (parameter, expected) in [
        ("reasoning_effort", "reasoning_effort"),
        ("reasoning.effort", "reasoning.effort"),
    ] {
        let provider = OpenAICompatibleProviderConfig {
            base_url: "http://localhost:11434/v1/".into(),
            api_key_env: "LOCAL_API_KEY".into(),
            credential_name: Some("local".into()),
            models: strvec(&["reasoner"]),
            default_model: "reasoner".into(),
            thinking_levels: Some(strvec(&["low", "high"])),
            thinking_parameter: Some(parameter.into()),
            ..OpenAICompatibleProviderConfig::new("local")
        };
        let creds = FakeCredentials::with_key("local", "test-key");
        let config = openai_compatible_config_from_provider(
            &provider,
            Some(&creds),
            Some("reasoner"),
            Some("high"),
        )
        .unwrap();
        assert_eq!(config.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(config.reasoning_effort_parameter, expected);
    }
}

#[test]
fn provider_settings_from_json_loads_headers() {
    let raw = serde_json::json!({
        "default_provider": "huggingface",
        "providers": [{
            "type": "openai-compatible",
            "name": "huggingface",
            "base_url": "https://router.huggingface.co/v1",
            "api_key_env": "HF_TOKEN",
            "credential_name": "huggingface",
            "models": ["Qwen/Qwen3-Coder"],
            "default_model": "Qwen/Qwen3-Coder",
            "headers": {"X-HF-Bill-To": "my-org"}
        }]
    });
    let settings = provider_settings_from_json(&raw, None).unwrap();
    let provider = as_compatible(settings.get_provider(Some("huggingface")).unwrap());
    assert_eq!(provider.headers["X-HF-Bill-To"], "my-org");
}

#[test]
fn provider_settings_from_json_loads_custom_thinking_capabilities() {
    let raw = serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible",
            "name": "local",
            "base_url": "http://localhost:11434/v1",
            "api_key_env": "LOCAL_API_KEY",
            "models": ["reasoner", "plain"],
            "default_model": "reasoner",
            "thinking_levels": ["off", "low", "high"],
            "thinking_models": ["reasoner"],
            "thinking_default": "low",
            "thinking_parameter": "reasoning_effort",
            "thinking_defaults": {"reasoner": "high"}
        }]
    });
    let settings = provider_settings_from_json(&raw, None).unwrap();
    let provider = settings.get_provider(Some("local")).unwrap();
    assert_eq!(
        provider_thinking_levels(provider, Some("reasoner")),
        strvec(&["off", "low", "high"])
    );
    assert!(provider_thinking_levels(provider, Some("plain")).is_empty());
    assert_eq!(
        provider_default_thinking_level(provider, Some("reasoner")).as_deref(),
        Some("low")
    );
    assert_eq!(provider.thinking_defaults()["reasoner"], "high");
    assert_eq!(
        provider.to_json()["thinking_parameter"],
        serde_json::json!("reasoning_effort")
    );
}

#[test]
fn set_provider_thinking_level_updates_preference() {
    let provider = ProviderConfig::OpenAICompatible(OpenAICompatibleProviderConfig {
        models: strvec(&["reasoner"]),
        default_model: "reasoner".into(),
        thinking_levels: Some(strvec(&["low", "high"])),
        thinking_models: strvec(&["reasoner"]),
        thinking_default: Some("low".into()),
        thinking_parameter: Some("reasoning_effort".into()),
        ..OpenAICompatibleProviderConfig::new("local")
    });
    let settings = ProviderSettings {
        default_provider: "local".into(),
        providers: vec![provider],
        scoped_models: Vec::new(),
    };
    let updated = set_provider_thinking_level(&settings, "local", "reasoner", "high").unwrap();
    assert_eq!(
        updated
            .get_provider(Some("local"))
            .unwrap()
            .thinking_defaults()["reasoner"],
        "high"
    );
    assert_eq!(
        updated.to_json()["provider_preferences"]["local"]["thinking_defaults"]["reasoner"],
        serde_json::json!("high")
    );
}

#[test]
fn provider_settings_from_json_loads_openai_codex_provider() {
    let raw = serde_json::json!({
        "default_provider": "openai-codex",
        "providers": [{
            "type": "openai-codex",
            "name": "openai-codex",
            "base_url": "https://chatgpt.com/backend-api",
            "api_key_env": "OPENAI_CODEX_ACCESS_TOKEN",
            "credential_name": "openai-codex",
            "models": ["gpt-5.5", "gpt-5.4"],
            "default_model": "gpt-5.5",
            "headers": {"X-Test": "enabled"}
        }]
    });
    let settings = provider_settings_from_json(&raw, None).unwrap();
    let provider = settings.get_provider(Some("openai-codex")).unwrap();
    assert!(matches!(provider, ProviderConfig::OpenAICodex(_)));
    assert_eq!(provider.default_model(), "gpt-5.5");
    assert_eq!(provider.headers()["X-Test"], "enabled");
}

#[test]
fn provider_settings_from_json_loads_anthropic_thinking_provider() {
    let raw = serde_json::json!({
        "default_provider": "anthropic",
        "providers": [{
            "type": "anthropic",
            "name": "anthropic",
            "base_url": "https://api.anthropic.com/v1",
            "api_key_env": "ANTHROPIC_API_KEY",
            "models": ["claude-sonnet-4-6"],
            "default_model": "claude-sonnet-4-6",
            "thinking_levels": ["off", "low", "high"],
            "thinking_models": ["claude-sonnet-4-6"],
            "thinking_parameter": "anthropic.thinking"
        }]
    });
    let settings = provider_settings_from_json(&raw, None).unwrap();
    let provider = settings.get_provider(Some("anthropic")).unwrap();
    assert!(matches!(provider, ProviderConfig::Anthropic(_)));
    assert_eq!(
        provider_thinking_levels(provider, Some("claude-sonnet-4-6")),
        strvec(&["off", "low", "high"])
    );
    assert_eq!(provider.thinking_parameter(), Some("anthropic.thinking"));
}

#[test]
fn load_provider_settings_does_not_restore_stale_codex_builtin_models() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "openai-codex", "providers": [{"type": "openai-codex", "name": "openai-codex", "base_url": "https://chatgpt.com/backend-api", "api_key_env": "OPENAI_CODEX_ACCESS_TOKEN", "credential_name": "openai-codex", "models": ["gpt-5", "gpt-5.5"], "default_model": "gpt-5"}]}"#,
    )
    .unwrap();
    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = settings.get_provider(Some("openai-codex")).unwrap();
    assert_eq!(
        provider.models(),
        strvec(&[
            "gpt-5.6",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
        ])
        .as_slice()
    );
    assert_eq!(provider.default_model(), "gpt-5.5");
}

#[test]
fn load_provider_settings_merges_builtin_model_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "huggingface", "providers": [{"type": "openai-compatible", "name": "huggingface", "base_url": "https://router.huggingface.co/v1", "api_key_env": "HF_TOKEN", "credential_name": "huggingface", "models": ["MiniMaxAI/MiniMax-M2.7", "custom/coder"], "default_model": "MiniMaxAI/MiniMax-M2.7", "headers": {"X-HF-Bill-To": "my-org"}}]}"#,
    )
    .unwrap();
    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = settings.get_provider(Some("huggingface")).unwrap();
    assert_eq!(provider.default_model(), "MiniMaxAI/MiniMax-M2.7");
    assert_eq!(provider.headers()["X-HF-Bill-To"], "my-org");
    assert_eq!(
        provider.context_windows()["MiniMaxAI/MiniMax-M2.7"],
        204_800
    );
    assert!(
        provider
            .models()
            .iter()
            .any(|m| m == "Qwen/Qwen3-Coder-480B-A35B-Instruct")
    );
    assert!(
        provider
            .models()
            .iter()
            .any(|m| m == "moonshotai/Kimi-K2.6")
    );
    assert!(provider.models().iter().any(|m| m == "custom/coder"));
}

#[test]
fn load_provider_settings_restores_builtin_providers_with_stored_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "local", "providers": [{"type": "openai-compatible", "name": "local", "base_url": "http://localhost:11434/v1", "api_key_env": "LOCAL_API_KEY", "credential_name": null, "models": ["qwen"], "default_model": "qwen"}]}"#,
    )
    .unwrap();
    let mut creds = FakeCredentials::default();
    creds
        .keys
        .insert("openrouter".into(), "stored-openrouter-key".into());
    creds
        .oauth
        .insert("openai-codex".into(), "access-token".into());

    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), Some(&creds)).unwrap();
    let names: Vec<&str> = settings
        .providers
        .iter()
        .map(ProviderConfig::name)
        .collect();
    // Credential-gated builtins are appended. (Exact list equality is avoided:
    // ambient env vars for other builtins can't be cleared under unsafe-forbid.)
    assert!(names.contains(&"local"));
    assert!(names.contains(&"openrouter"), "got: {names:?}");
    assert!(names.contains(&"openai-codex"), "got: {names:?}");
    assert_eq!(settings.default_provider, "local");
    assert_eq!(
        settings
            .get_provider(Some("openrouter"))
            .unwrap()
            .credential_name(),
        Some("openrouter")
    );
    assert_eq!(
        settings
            .get_provider(Some("openai-codex"))
            .unwrap()
            .credential_name(),
        Some("openai-codex")
    );
}

#[test]
fn load_provider_settings_restores_builtin_credential_name() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    std::fs::create_dir_all(&tau_home).unwrap();
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "openrouter", "providers": [{"type": "openai-compatible", "name": "openrouter", "base_url": "https://openrouter.ai/api/v1", "api_key_env": "OPENROUTER_API_KEY", "credential_name": null, "models": ["openai/gpt-5.5"], "default_model": "openai/gpt-5.5"}]}"#,
    )
    .unwrap();
    let paths = paths_for(&tau_home);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = as_compatible(settings.get_provider(Some("openrouter")).unwrap()).clone();
    assert_eq!(provider.credential_name.as_deref(), Some("openrouter"));
    assert_eq!(provider.context_windows["openai/gpt-5.5"], 1_050_000);
    let creds = FakeCredentials::with_key("openrouter", "stored-key");
    let config =
        openai_compatible_config_from_provider(&provider, Some(&creds), None, None).unwrap();
    assert_eq!(config.api_key, "stored-key");
}

#[test]
fn provider_settings_from_json_rejects_invalid_headers() {
    let raw = serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible", "name": "local",
            "base_url": "http://localhost:11434/v1", "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen"], "default_model": "qwen", "headers": {"X-Test": 123}
        }]
    });
    let err = provider_settings_from_json(&raw, None).unwrap_err();
    assert!(err.0.contains("string object"), "got: {}", err.0);
}

#[test]
fn provider_settings_from_json_rejects_invalid_timeout() {
    let raw = serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible", "name": "local",
            "base_url": "http://localhost:11434/v1", "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen"], "default_model": "qwen", "timeout_seconds": 0
        }]
    });
    let err = provider_settings_from_json(&raw, None).unwrap_err();
    assert!(err.0.contains("greater than 0"), "got: {}", err.0);
}

#[test]
fn openai_compatible_provider_config_rejects_invalid_timeout() {
    let config = OpenAICompatibleProviderConfig {
        timeout_seconds: 0.0,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("greater than 0"), "got: {}", err.0);
}

#[test]
fn provider_settings_from_json_rejects_invalid_retries() {
    let raw = serde_json::json!({
        "default_provider": "local",
        "providers": [{
            "type": "openai-compatible", "name": "local",
            "base_url": "http://localhost:11434/v1", "api_key_env": "LOCAL_API_KEY",
            "models": ["qwen"], "default_model": "qwen", "max_retries": -1
        }]
    });
    let err = provider_settings_from_json(&raw, None).unwrap_err();
    assert!(err.0.contains("0 or greater"), "got: {}", err.0);
}

#[test]
fn openai_compatible_provider_config_rejects_invalid_retries() {
    let bad_retries = OpenAICompatibleProviderConfig {
        max_retries: -1,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    assert!(
        bad_retries
            .validate()
            .unwrap_err()
            .0
            .contains("0 or greater")
    );
    let bad_delay = OpenAICompatibleProviderConfig {
        max_retry_delay_seconds: -1.0,
        ..OpenAICompatibleProviderConfig::new("local")
    };
    assert!(bad_delay.validate().unwrap_err().0.contains("0 or greater"));
}

#[test]
fn empty_provider_and_model_fall_back_to_defaults() {
    // Python truthiness (M1): `--provider ""` / `--model ""` are falsy and
    // resolve to the defaults (tau `provider_name or default_provider`,
    // `model or provider.default_model`) rather than erroring on an empty name.
    let settings = ProviderSettings::default();
    let default_provider = settings.get_provider(None).unwrap();

    assert_eq!(
        settings.get_provider(Some("")).unwrap().name(),
        default_provider.name(),
        "empty provider name resolves to the default provider"
    );

    let selection = resolve_provider_selection(&settings, Some(""), Some("")).unwrap();
    assert_eq!(selection.provider.name(), default_provider.name());
    assert_eq!(selection.model, default_provider.default_model());
}
