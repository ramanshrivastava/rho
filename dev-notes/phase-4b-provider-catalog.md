# Phase 4b — provider catalog + config

Ports tau's `tau_coding.provider_catalog`, `catalog_loader`, and
`provider_config` into `rho-coding`. These three modules own the durable
provider model: the built-in catalog (vendored TOML), the user overlay, durable
`providers.json` settings, and the construction of `rho-ai` runtime configs
(`OpenAICompatibleConfig` / `AnthropicConfig`) from those settings.

## What was built

- `provider_catalog.rs` — `ProviderCatalogEntry`, `ModelCatalogMetadata`,
  `ModelCostTier`, `model_cost_for_input_tokens`, `BUILTIN_PROVIDER_CATALOG`
  (a `LazyLock<Vec<…>>` built once from the vendored TOML), `builtin_provider_entry`.
  tau's `Literal[…]` label unions (`ProviderKind`/`ProviderApi`/`ModelInput`/
  `ThinkingParameter`/`AuthMethod`) are plain `String` aliases, validated at the
  parse boundaries.
- `catalog_loader.rs` — `builtin_catalog`, `effective_catalog`,
  `save_user_catalog_entries`, `user_catalog_path`, `CatalogError`. tau validates
  with pydantic; rho validates by hand, preserving the strict types
  (`StrictInt`/non-empty string/`ge=0` float), `extra="forbid"`, and the dotted
  `providers.<name>.<field>` error locations the tests assert on. Overlay merging
  and `_catalog_to_toml` re-serialization are ported faithfully.
- `provider_config.rs` — the full public surface the integrator imports (see the
  M4b task brief): `ProviderConfig` enum over the three concrete configs,
  `ProviderConfigError`, `ProviderSettings`/`ProviderSelection`/`ScopedModelConfig`,
  `load_provider_settings`, the `save_*`/`set_*`/`upsert_*`/`toggle_*` helpers,
  `resolve_provider_selection`, `validate_provider_model`,
  `provider_thinking_levels`/`_unavailable_reason`/`provider_default_thinking_level`,
  `anthropic_config_from_provider`, `openai_compatible_config_from_provider`,
  `provider_kind`, `provider_has_usable_credentials`, `provider_settings_from_json`.

Ordering is preserved with `indexmap::IndexMap` for every tau dict and `toml`
with `preserve_order`; catalog entry order and `models` tuples are `Vec`.

## Deviations from tau (all deliberate)

1. **Credential/OAuth dependency inversion.** tau's `provider_config` imports the
   concrete `FileCredentialStore` and `oauth_registry` (out of this cluster in
   rho). rho injects credential lookup through the public `CredentialReader`
   trait: `load_provider_settings` and the save/mutate helpers take an
   `Option<&dyn CredentialReader>`, and `openai_compatible_config_from_provider`/
   `anthropic_config_from_provider` take it as an argument (as tau already does).
   `CredentialReader::get_oauth` returns the access-token `String` — the only
   field tau's `provider_config` reads off the `OAuthCredential`. The
   integrator implements the trait for the real store once `credentials`/`oauth`
   land. `get_oauth_provider(name) is not None` is inlined as
   `oauth_provider_registered(name)` over the built-in ids
   (`anthropic`/`github-copilot`/`openai-codex`); extension-registered OAuth
   providers are out of scope until the extension runtime exists.

2. **Missing-key error type.** tau raises `RuntimeError` for a missing API key;
   rho returns `ProviderConfigError` with tau's exact message
   (`Missing provider API key. Set <ENV>[ or run /login <name>].`). Rust has no
   exception-type dispatch, and every provider-config failure is one error type.

3. **`model_cost_for_input_tokens` bool rejection.** tau rejects a `bool`
   `input_tokens` (Python `bool ⊂ int`); rho's `i64` parameter cannot carry a
   bool, so only the negative check survives. Same for the `input_tokens=True`
   test parametrization.

4. **`providers.json` serialization.** tau writes
   `json.dumps(..., indent=2, sort_keys=True)`. rho recursively sorts object keys
   and pretty-prints with serde_json. No test asserts the exact bytes
   (round-trips go through the reader), but sort-keys parity is kept for future
   byte-diff safety.

## Tests

`tests/provider_catalog.rs` (22) and `tests/provider_config.rs` (40) port every
non-TUI case from tau's `test_provider_catalog.py` / `test_provider_config.py`.

Two env-only cases are **skipped** and documented in the test-file module docs:
`preserves_openai_base_url_env` and `falls_back_to_env_when_stored_missing`. rho
is edition 2024 with `unsafe_code = "forbid"`, so `std::env::set_var` (now
`unsafe`) cannot be called. All other `monkeypatch.setenv` cases are ported by
injecting a fake `CredentialReader` (which supersedes the env lookup) instead of
mutating the process env; `provider_has_usable_credentials` uses a
guaranteed-unset env-var name for its deterministic "no credentials" branch, and
`restores_builtin_providers_with_stored_credentials` asserts the credentialed
builtins are appended rather than exact list equality (ambient env vars for other
builtins can't be cleared).
