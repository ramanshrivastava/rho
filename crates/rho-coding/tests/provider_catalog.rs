//! Port of tau's `tests/test_provider_catalog.py`.
//!
//! Every non-TUI case is ported. The only test-shape divergence is the
//! `input_tokens=True` parametrization of
//! `test_model_cost_for_input_tokens_rejects_invalid_count`: Python's `bool` is a
//! subtype of `int`, so tau rejects `True`; rho's `i64` parameter cannot carry a
//! Python `bool`, so only the negative case is meaningful.

use std::path::Path;

use indexmap::IndexMap;
use rho_coding::catalog_loader::{
    builtin_catalog, builtin_catalog_resource_text, effective_catalog, save_user_catalog_entries,
    user_catalog_path,
};
use rho_coding::paths::RhoPaths;
use rho_coding::provider_catalog::{
    BUILTIN_PROVIDER_CATALOG, builtin_provider_entry, model_cost_for_input_tokens,
};
use rho_coding::provider_config::load_provider_settings;

const VALID_PROVIDER: &str = r#"
[[providers]]
name = "nebius"
display_name = "Nebius AI Studio"
kind = "openai-compatible"
base_url = "https://api.studio.nebius.ai/v1"
api_key_env = "NEBIUS_API_KEY"
credential_name = "nebius"
models = ["deepseek-ai/DeepSeek-V4-Pro", "Qwen/Qwen3-Coder-480B-A35B-Instruct"]
default_model = "deepseek-ai/DeepSeek-V4-Pro"
docs_url = "https://studio.nebius.ai/docs"
thinking_levels = ["off", "low", "medium", "high"]
thinking_models = ["deepseek-ai/DeepSeek-V4-Pro"]
thinking_default = "medium"
thinking_parameter = "reasoning_effort"

[providers.context_windows]
"deepseek-ai/DeepSeek-V4-Pro" = 163840
"#;

fn paths_for(home: &Path) -> RhoPaths {
    RhoPaths::new(home.to_path_buf(), home.join(".agents"))
}

fn write_user_catalog(tau_home: &Path, body: &str) -> RhoPaths {
    let paths = paths_for(tau_home);
    std::fs::create_dir_all(tau_home).unwrap();
    std::fs::write(
        user_catalog_path(Some(&paths)),
        format!("schema_version = 1\n{body}"),
    )
    .unwrap();
    paths
}

fn cost(pairs: &[(&str, f64)]) -> IndexMap<String, f64> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

#[test]
fn builtin_catalog_matches_expected_providers() {
    let names: Vec<&str> = BUILTIN_PROVIDER_CATALOG
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
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
        ]
    );
}

#[test]
fn builtin_catalog_golden_anthropic_entry() {
    let entry = builtin_provider_entry("anthropic").unwrap();
    assert_eq!(entry.display_name, "Anthropic");
    assert_eq!(entry.kind, "anthropic");
    assert_eq!(entry.base_url, "https://api.anthropic.com");
    assert_eq!(entry.api_key_env, "ANTHROPIC_API_KEY");
    assert_eq!(entry.credential_name.as_deref(), Some("anthropic"));
    assert_eq!(
        entry.models,
        [
            "claude-fable-5",
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
        ]
    );
    assert_eq!(entry.default_model, "claude-sonnet-4-6");
    assert_eq!(entry.docs_url, "https://docs.anthropic.com");
    let windows = entry.context_windows.unwrap();
    assert_eq!(windows["claude-fable-5"], 1_000_000);
    assert_eq!(windows["claude-haiku-4-5"], 200_000);
    assert_eq!(windows["claude-opus-4-7"], 1_000_000);
    assert_eq!(windows["claude-sonnet-4-6"], 1_000_000);
    assert_eq!(windows["claude-sonnet-5"], 1_000_000);
    assert_eq!(windows.len(), 13);
    assert_eq!(
        entry.thinking_levels.unwrap(),
        ["off", "minimal", "low", "medium", "high", "xhigh"]
    );
    assert!(entry.thinking_models.is_empty());
    assert_eq!(entry.thinking_default.as_deref(), Some("medium"));
    assert_eq!(
        entry.thinking_parameter.as_deref(),
        Some("anthropic.thinking")
    );
    assert_eq!(entry.auth_methods, ["api_key", "oauth"]);
}

#[test]
fn builtin_catalog_oauth_and_opencode_auth_methods() {
    let codex = builtin_provider_entry("openai-codex").unwrap();
    let copilot = builtin_provider_entry("github-copilot").unwrap();
    let opencode_go = builtin_provider_entry("opencode-go").unwrap();
    let opencode = builtin_provider_entry("opencode").unwrap();
    assert_eq!(codex.auth_methods, ["oauth"]);
    assert_eq!(copilot.auth_methods, ["oauth"]);
    assert_eq!(opencode_go.auth_methods, ["api_key"]);
    assert_eq!(opencode.auth_methods, ["api_key"]);
    assert_eq!(opencode_go.api_key_env, "OPENCODE_API_KEY");
    assert_eq!(opencode.api_key_env, "OPENCODE_API_KEY");
}

#[test]
fn builtin_catalog_golden_nvidia_entry() {
    let entry = builtin_provider_entry("nvidia").unwrap();
    assert_eq!(entry.display_name, "NVIDIA NIM");
    assert_eq!(entry.kind, "openai-compatible");
    assert_eq!(entry.base_url, "https://integrate.api.nvidia.com/v1");
    assert_eq!(entry.api_key_env, "NVIDIA_API_KEY");
    assert_eq!(entry.credential_name.as_deref(), Some("nvidia"));
    assert_eq!(
        entry.models,
        [
            "nvidia/llama-3.3-nemotron-super-49b-v1.5",
            "nvidia/nvidia-nemotron-nano-9b-v2",
            "meta/llama-3.3-70b-instruct",
            "meta/llama-3.1-8b-instruct",
            "deepseek-ai/deepseek-v4-pro",
            "qwen/qwen3.5-122b-a10b",
            "mistralai/mistral-large-2-instruct",
            "openai/gpt-oss-120b",
        ]
    );
    assert_eq!(
        entry.default_model,
        "nvidia/llama-3.3-nemotron-super-49b-v1.5"
    );
    assert_eq!(entry.docs_url, "https://docs.api.nvidia.com/nim");
    assert_eq!(entry.api.as_deref(), Some("openai-completions"));
    let windows = entry.context_windows.clone().unwrap();
    assert_eq!(windows["nvidia/llama-3.3-nemotron-super-49b-v1.5"], 131_072);
    assert_eq!(windows["nvidia/nvidia-nemotron-nano-9b-v2"], 128_000);
    assert_eq!(windows["deepseek-ai/deepseek-v4-pro"], 1_000_000);
    assert_eq!(windows["qwen/qwen3.5-122b-a10b"], 262_144);
    assert_eq!(windows["openai/gpt-oss-120b"], 131_072);
    assert_eq!(
        entry.thinking_levels.clone().unwrap(),
        ["off", "minimal", "low", "medium", "high"]
    );
    assert!(entry.thinking_models.is_empty());
    assert_eq!(entry.thinking_default.as_deref(), Some("medium"));
    assert_eq!(
        entry.thinking_parameter.as_deref(),
        Some("reasoning_effort")
    );

    let default_metadata = &entry.model_metadata[&entry.default_model];
    assert_eq!(
        default_metadata.name.as_deref(),
        Some("NVIDIA: Llama 3.3 Nemotron Super 49B V1.5")
    );
    assert_eq!(default_metadata.reasoning, Some(true));
    assert_eq!(default_metadata.input, ["text"]);
    assert_eq!(default_metadata.context_window, Some(131_072));
    assert_eq!(default_metadata.max_tokens, Some(16_384));
    assert_eq!(
        default_metadata.cost.clone().unwrap(),
        cost(&[
            ("input", 0.0),
            ("output", 0.0),
            ("cacheRead", 0.0),
            ("cacheWrite", 0.0)
        ])
    );

    let gpt_oss = &entry.model_metadata["openai/gpt-oss-120b"];
    assert_eq!(gpt_oss.reasoning, Some(true));
    assert_eq!(gpt_oss.context_window, Some(131_072));
    assert_eq!(gpt_oss.max_tokens, Some(65_536));
}

#[test]
fn builtin_catalog_golden_kimi_entries() {
    let moonshot = builtin_provider_entry("moonshotai").unwrap();
    assert_eq!(moonshot.display_name, "Moonshot AI (Kimi)");
    assert_eq!(moonshot.default_model, "kimi-k2.7-code");
    assert!(moonshot.models.iter().any(|m| m == "kimi-k2.7-code"));
    assert_eq!(
        moonshot.context_windows.as_ref().unwrap()["kimi-k2.7-code"],
        262_144
    );

    let moonshot_cn = builtin_provider_entry("moonshotai-cn").unwrap();
    assert_eq!(moonshot_cn.default_model, "kimi-k2.7-code");
    assert!(moonshot_cn.models.iter().any(|m| m == "kimi-k2.7-code"));
    assert_eq!(
        moonshot_cn.context_windows.as_ref().unwrap()["kimi-k2.7-code"],
        262_144
    );

    let k2_7 = &moonshot.model_metadata["kimi-k2.7-code"];
    assert_eq!(k2_7.name.as_deref(), Some("Kimi K2.7 Code"));
    assert_eq!(k2_7.reasoning, Some(true));
    assert_eq!(k2_7.input, ["text", "image"]);
    assert_eq!(k2_7.context_window, Some(262_144));
    assert_eq!(k2_7.max_tokens, Some(32_768));
    let expected: IndexMap<String, Option<String>> = [
        ("off", None),
        ("minimal", None),
        ("low", None),
        ("high", None),
    ]
    .into_iter()
    .map(|(k, v): (&str, Option<&str>)| (k.to_string(), v.map(str::to_string)))
    .collect();
    assert_eq!(k2_7.thinking_level_map, expected);

    let coding = builtin_provider_entry("kimi-code").unwrap();
    assert_eq!(coding.display_name, "Kimi Code subscription");
    assert_eq!(coding.base_url, "https://api.kimi.com/coding/v1");
    assert_eq!(coding.api_key_env, "KIMI_CODE_API_KEY");
    assert_eq!(coding.credential_name.as_deref(), Some("kimi-code"));
    assert_eq!(coding.models, ["k3", "kimi-for-coding"]);
    assert_eq!(coding.default_model, "kimi-for-coding");
    let windows = coding.context_windows.clone().unwrap();
    assert_eq!(windows["k3"], 1_048_576);
    assert_eq!(windows["kimi-for-coding"], 262_144);
    assert_eq!(windows.len(), 2);

    let k3 = &coding.model_metadata["k3"];
    assert_eq!(k3.name.as_deref(), Some("Kimi K3"));
    assert_eq!(k3.reasoning, Some(true));
    assert_eq!(k3.input, ["text"]);
    assert_eq!(k3.context_window, Some(1_048_576));
    let expected_k3: IndexMap<String, Option<String>> = [
        ("off", None),
        ("minimal", None),
        ("low", None),
        ("medium", None),
        ("high", None),
        ("xhigh", Some("max")),
    ]
    .into_iter()
    .map(|(k, v): (&str, Option<&str>)| (k.to_string(), v.map(str::to_string)))
    .collect();
    assert_eq!(k3.thinking_level_map, expected_k3);

    let latest = &coding.model_metadata["kimi-for-coding"];
    assert_eq!(latest.name.as_deref(), Some("Kimi for Coding (latest)"));
    assert_eq!(latest.reasoning, Some(true));
    assert_eq!(latest.context_window, Some(262_144));
}

#[test]
fn builtin_minimax_m3_has_tiered_pricing() {
    let base_cost = cost(&[
        ("input", 0.3),
        ("output", 1.2),
        ("cacheRead", 0.06),
        ("cacheWrite", 0.0),
    ]);
    let long_context_cost = cost(&[
        ("input", 0.6),
        ("output", 2.4),
        ("cacheRead", 0.12),
        ("cacheWrite", 0.0),
    ]);
    for provider_name in ["minimax", "minimax-cn"] {
        let entry = builtin_provider_entry(provider_name).unwrap();
        let metadata = &entry.model_metadata["MiniMax-M3"];
        assert_eq!(metadata.input, ["text", "image"]);
        assert_eq!(metadata.cost.clone().unwrap(), base_cost);
        assert_eq!(
            model_cost_for_input_tokens(metadata, 512_000)
                .unwrap()
                .unwrap(),
            base_cost
        );
        assert_eq!(
            model_cost_for_input_tokens(metadata, 512_001)
                .unwrap()
                .unwrap(),
            long_context_cost
        );
    }
}

#[test]
fn model_cost_for_input_tokens_rejects_invalid_count() {
    let entry = builtin_provider_entry("minimax").unwrap();
    let metadata = &entry.model_metadata["MiniMax-M3"];
    let err = model_cost_for_input_tokens(metadata, -1).unwrap_err();
    assert!(err.contains("non-negative integer"), "got: {err}");
    // tau also rejects `input_tokens=True` (Python bool ⊂ int); rho's i64
    // parameter cannot carry a bool, so that parametrization is inapplicable.
}

#[test]
fn builtin_catalog_entries_are_internally_consistent() {
    for entry in builtin_catalog() {
        assert!(entry.models.contains(&entry.default_model));
        for model in &entry.thinking_models {
            assert!(entry.models.contains(model));
        }
        if let Some(windows) = &entry.context_windows {
            for model in windows.keys() {
                assert!(entry.models.contains(model));
            }
        }
        if let Some(default) = &entry.thinking_default {
            let levels = entry.thinking_levels.as_ref().expect("thinking_levels set");
            assert!(levels.contains(default));
        }
    }
}

#[test]
fn builtin_catalog_resource_is_packaged() {
    assert!(builtin_catalog_resource_text().contains("[[providers]]"));
}

#[test]
fn effective_catalog_without_user_file_is_builtin() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_for(&tmp.path().join(".tau"));
    assert_eq!(&effective_catalog(Some(&paths)).unwrap(), builtin_catalog());
}

#[test]
fn user_catalog_adds_new_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(&tmp.path().join(".tau"), VALID_PROVIDER);
    let catalog = effective_catalog(Some(&paths)).unwrap();
    let head: Vec<&str> = catalog[..catalog.len() - 1]
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    let builtin_names: Vec<&str> = builtin_catalog().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(head, builtin_names);
    let entry = catalog.last().unwrap();
    assert_eq!(entry.name, "nebius");
    assert_eq!(entry.default_model, "deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(
        entry.context_windows.as_ref().unwrap()["deepseek-ai/DeepSeek-V4-Pro"],
        163_840
    );
    assert_eq!(
        entry.thinking_levels.clone().unwrap(),
        ["off", "low", "medium", "high"]
    );
}

#[test]
fn user_catalog_overlays_builtin_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        r#"
[[providers]]
name = "anthropic"
models = ["claude-next-1"]
default_model = "claude-next-1"

[providers.context_windows]
"claude-next-1" = 500000
"#,
    );
    let catalog = effective_catalog(Some(&paths)).unwrap();
    let entry = catalog.iter().find(|e| e.name == "anthropic").unwrap();
    assert_eq!(entry.models[0], "claude-next-1");
    assert!(entry.models.iter().any(|m| m == "claude-sonnet-4-6"));
    assert_eq!(entry.default_model, "claude-next-1");
    let windows = entry.context_windows.as_ref().unwrap();
    assert_eq!(windows["claude-next-1"], 500_000);
    assert_eq!(windows["claude-opus-4-7"], 1_000_000);
    assert_eq!(entry.base_url, "https://api.anthropic.com");
    assert_eq!(
        entry.thinking_parameter.as_deref(),
        Some("anthropic.thinking")
    );
}

#[test]
fn user_catalog_thinking_fields_replace_as_group() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        r#"
[[providers]]
name = "anthropic"
thinking_levels = ["off", "high"]
thinking_default = "high"
"#,
    );
    let catalog = effective_catalog(Some(&paths)).unwrap();
    let entry = catalog.iter().find(|e| e.name == "anthropic").unwrap();
    assert_eq!(entry.thinking_levels.clone().unwrap(), ["off", "high"]);
    assert_eq!(entry.thinking_default.as_deref(), Some("high"));
    assert!(entry.thinking_models.is_empty());
    assert_eq!(entry.thinking_parameter, None);
}

#[test]
fn user_catalog_overlays_and_serializes_cost_tiers() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        r#"
[[providers]]
name = "minimax"

[providers.model_metadata."MiniMax-M3"]
cost_tiers = [
  { max_input_tokens = 400000, input = 0.2, output = 1.0, cacheRead = 0.04, cacheWrite = 0 },
  { input = 0.5, output = 2.0, cacheRead = 0.1, cacheWrite = 0 },
]
"#,
    );
    let catalog = effective_catalog(Some(&paths)).unwrap();
    let entry = catalog.iter().find(|e| e.name == "minimax").unwrap();
    let metadata = &entry.model_metadata["MiniMax-M3"];
    assert_eq!(
        model_cost_for_input_tokens(metadata, 400_000)
            .unwrap()
            .unwrap(),
        cost(&[
            ("input", 0.2),
            ("output", 1.0),
            ("cacheRead", 0.04),
            ("cacheWrite", 0.0)
        ])
    );
    let long_context_cost = cost(&[
        ("input", 0.5),
        ("output", 2.0),
        ("cacheRead", 0.1),
        ("cacheWrite", 0.0),
    ]);
    assert_eq!(
        model_cost_for_input_tokens(metadata, 400_001)
            .unwrap()
            .unwrap(),
        long_context_cost
    );

    save_user_catalog_entries(std::slice::from_ref(entry), Some(&paths)).unwrap();
    let reloaded = effective_catalog(Some(&paths)).unwrap();
    let reloaded_entry = reloaded.iter().find(|e| e.name == "minimax").unwrap();
    assert_eq!(
        model_cost_for_input_tokens(&reloaded_entry.model_metadata["MiniMax-M3"], 400_001)
            .unwrap()
            .unwrap(),
        long_context_cost
    );
}

#[test]
fn user_catalog_rejects_bounded_final_cost_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        r#"
[[providers]]
name = "minimax"

[providers.model_metadata."MiniMax-M3"]
cost_tiers = [
  { max_input_tokens = 512000, input = 0.3, output = 1.2, cacheRead = 0.06, cacheWrite = 0 },
]
"#,
    );
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(
        err.0.contains("final tier must omit max_input_tokens"),
        "got: {}",
        err.0
    );
}

#[test]
fn user_catalog_rejects_unknown_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        &VALID_PROVIDER.replace("docs_url", "docs_ur1"),
    );
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(err.0.contains("providers.nebius"), "got: {}", err.0);
}

#[test]
fn user_catalog_rejects_default_model_not_in_models() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        &VALID_PROVIDER.replace(
            "default_model = \"deepseek-ai/DeepSeek-V4-Pro\"",
            "default_model = \"missing\"",
        ),
    );
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(
        err.0.contains("providers.nebius.default_model"),
        "got: {}",
        err.0
    );
}

fn assert_catalog_error(body: &str, needle: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(&tmp.path().join(".tau"), body);
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(err.0.contains(needle), "expected {needle:?} in {:?}", err.0);
}

#[test]
fn user_catalog_rejects_empty_and_coerced_values() {
    let cw = "\"deepseek-ai/DeepSeek-V4-Pro\" = 163840";
    assert_catalog_error(
        &VALID_PROVIDER.replace("display_name = \"Nebius AI Studio\"", "display_name = \"\""),
        "providers.nebius.display_name",
    );
    assert_catalog_error(
        &VALID_PROVIDER.replace(
            "models = [\"deepseek-ai/DeepSeek-V4-Pro\", \"Qwen/Qwen3-Coder-480B-A35B-Instruct\"]",
            "models = [\"\"]",
        ),
        "providers.nebius.models",
    );
    for replacement in [
        "\"\" = 163840",
        "\"deepseek-ai/DeepSeek-V4-Pro\" = 0",
        "\"deepseek-ai/DeepSeek-V4-Pro\" = -1",
        "\"deepseek-ai/DeepSeek-V4-Pro\" = true",
        "\"deepseek-ai/DeepSeek-V4-Pro\" = \"163840\"",
    ] {
        assert_catalog_error(
            &VALID_PROVIDER.replace(cw, replacement),
            "providers.nebius.context_windows",
        );
    }
}

#[test]
fn user_catalog_rejects_bad_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(
        &tmp.path().join(".tau"),
        &VALID_PROVIDER.replace("openai-compatible", "grpc"),
    );
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(err.0.contains("kind"), "got: {}", err.0);
}

#[test]
fn user_catalog_rejects_malformed_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(&tmp.path().join(".tau"), "[[providers]\nname =");
    let err = effective_catalog(Some(&paths)).unwrap_err();
    assert!(err.0.contains("invalid TOML"), "got: {}", err.0);
}

#[test]
fn user_catalog_provider_appears_in_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = write_user_catalog(&tmp.path().join(".tau"), VALID_PROVIDER);
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    let provider = settings.get_provider(Some("nebius")).unwrap();
    assert_eq!(provider.base_url(), "https://api.studio.nebius.ai/v1");
    assert_eq!(provider.default_model(), "deepseek-ai/DeepSeek-V4-Pro");
}

#[test]
fn user_catalog_provider_appears_with_existing_settings_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tau_home = tmp.path().join(".tau");
    let paths = write_user_catalog(&tau_home, VALID_PROVIDER);
    std::fs::write(
        tau_home.join("providers.json"),
        r#"{"default_provider": "openai", "providers": [{"type": "openai-compatible", "name": "openai", "base_url": "https://api.openai.com/v1", "api_key_env": "OPENAI_API_KEY", "models": ["gpt-5.5"], "default_model": "gpt-5.5"}], "scoped_models": []}"#,
    )
    .unwrap();
    let settings = load_provider_settings(Some(&paths), None).unwrap();
    assert_eq!(
        settings.get_provider(Some("nebius")).unwrap().models()[0],
        "deepseek-ai/DeepSeek-V4-Pro"
    );
}
