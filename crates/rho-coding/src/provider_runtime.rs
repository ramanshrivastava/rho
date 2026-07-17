//! Runtime provider construction for rho coding sessions.
//!
//! Port of tau's `tau_coding/provider_runtime.py`. [`create_model_provider`]
//! turns a durable [`ProviderConfig`] into a live `rho-ai` provider, wiring the
//! per-request OAuth credential resolvers where a provider is OAuth-backed.
//!
//! ## Deviations from tau (parity notes)
//!
//! - tau's `create_model_provider` imports the concrete `FileCredentialStore`
//!   and `oauth_registry` directly. rho keeps the provider-config layer free of
//!   the credential/oauth layer (they were ported as separate clusters), so the
//!   glue lives here: this module provides [`CredentialReader`] for
//!   [`FileCredentialStore`] and resolves [`get_oauth_provider`] itself.
//! - tau's resolvers await an ambient `httpx` client and read `time.time()`.
//!   rho's [`OAuthProvider::refresh`] and [`refresh_openai_codex_token`] take an
//!   injected [`OAuthHttpClient`] and an explicit `now_ms`; the resolver
//!   closures construct a [`ReqwestOAuthClient`] and read the wall clock at call
//!   time (runtime-only values, never persisted).
//! - A `CredentialStoreError` while reading a credential surfaces as `None`
//!   (treated as "no stored credential"), matching how a missing store reads.

// Same rationale as `session.rs`/`tools/difflib.rs`: the port mirrors tau's
// terse field-by-field config assembly, so a few pedantic style lints are
// allowed module-wide rather than idiomatized away from the source.
#![allow(clippy::assigning_clones, clippy::map_unwrap_or)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;

use rho_agent::provider::ModelProvider;
use rho_ai::env::{
    AnthropicConfig, OpenAICodexConfig, OpenAICodexCredentials, RuntimeProviderAuth,
    RuntimeProviderAuthResolver,
};
use rho_ai::{
    AnthropicProvider, GoogleGenerativeAIProvider, MistralConversationsProvider,
    OpenAICodexProvider, OpenAICompatibleProvider,
};

use crate::credentials::{FileCredentialStore, OAuthCredential};
use crate::oauth::{account_id_from_access_token, oauth_credential_is_expired};
use crate::oauth_http::ReqwestOAuthClient;
use crate::oauth_registry::get_oauth_provider;
use crate::oauth_types::OAuthProvider;
use crate::provider_config::{
    AnthropicProviderConfig, CredentialReader, OpenAICodexProviderConfig,
    OpenAICompatibleProviderConfig, ProviderConfig, ProviderConfigError,
    anthropic_config_from_provider, openai_compatible_config_from_provider,
    provider_thinking_levels, validate_provider_model,
};
use crate::thinking::{normalize_thinking_level, reasoning_effort_for_level};

/// tau's Anthropic OAuth system prompt (a second system block for the CLI
/// identity when authenticating with a subscription credential).
const ANTHROPIC_OAUTH_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Adapt the concrete credential store to the provider-config reader trait.
///
/// Inherent methods win over trait methods, so `self.get(..)` / `self.get_oauth(..)`
/// below resolve to [`FileCredentialStore`]'s inherent `Result`-returning
/// accessors; a read error collapses to `None`.
impl CredentialReader for FileCredentialStore {
    fn get(&self, name: &str) -> Option<String> {
        FileCredentialStore::get(self, name).ok().flatten()
    }

    fn get_oauth(&self, name: &str) -> Option<String> {
        FileCredentialStore::get_oauth(self, name)
            .ok()
            .flatten()
            .map(|credential| credential.access)
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX)
}

/// Create a runtime model provider from durable provider settings (tau
/// `create_model_provider`).
pub fn create_model_provider(
    provider: &ProviderConfig,
    credential_store: Option<Arc<FileCredentialStore>>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Arc<dyn ModelProvider>, ProviderConfigError> {
    if let Some(model) = model {
        validate_provider_model(provider, model)?;
    }
    let credentials =
        credential_store.unwrap_or_else(|| Arc::new(FileCredentialStore::at_default()));

    match provider {
        ProviderConfig::Anthropic(anthropic) => {
            create_anthropic_provider(provider, anthropic, &credentials, model, thinking_level)
        }
        ProviderConfig::OpenAICodex(codex) => {
            create_codex_provider(codex, &credentials, model, thinking_level)
        }
        ProviderConfig::OpenAICompatible(compatible) => create_openai_compatible_provider(
            provider,
            compatible,
            &credentials,
            model,
            thinking_level,
        ),
    }
}

fn create_anthropic_provider(
    provider: &ProviderConfig,
    anthropic: &AnthropicProviderConfig,
    credentials: &Arc<FileCredentialStore>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Arc<dyn ModelProvider>, ProviderConfigError> {
    let credential = oauth_credential(provider, credentials);
    let mut config = anthropic_config_from_provider(
        anthropic,
        Some(credentials.as_ref()),
        model,
        thinking_level,
    )?;
    if let Some(credential) = credential {
        let oauth_provider = required_oauth_provider(provider.name())?;
        let runtime_auth = oauth_provider.runtime_auth(&credential);
        config.api_key = runtime_auth.api_key;
        config.bearer_auth = true;
        config.headers = Some(merge_headers(config.headers, runtime_auth.headers));
        config.oauth_system_prompt = Some(ANTHROPIC_OAUTH_SYSTEM_PROMPT.to_string());
        config.credential_resolver = Some(runtime_auth_resolver(
            provider.name().to_string(),
            provider.credential_name().map(str::to_string),
            credentials.clone(),
            oauth_provider,
        ));
    }
    Ok(Arc::new(AnthropicProvider::new(config)))
}

fn create_openai_compatible_provider(
    provider: &ProviderConfig,
    compatible: &OpenAICompatibleProviderConfig,
    credentials: &Arc<FileCredentialStore>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Arc<dyn ModelProvider>, ProviderConfigError> {
    let credential = oauth_credential(provider, credentials);
    let mut config = openai_compatible_config_from_provider(
        compatible,
        Some(credentials.as_ref()),
        model,
        thinking_level,
    )?;
    if let Some(credential) = credential.as_ref() {
        let oauth_provider = required_oauth_provider(provider.name())?;
        let runtime_auth = oauth_provider.runtime_auth(credential);
        config.api_key = runtime_auth.api_key;
        if let Some(base_url) = runtime_auth.base_url.clone() {
            config.base_url = base_url;
        }
        config.headers = Some(merge_headers(config.headers, runtime_auth.headers));
        config.credential_resolver = Some(runtime_auth_resolver(
            provider.name().to_string(),
            provider.credential_name().map(str::to_string),
            credentials.clone(),
            oauth_provider,
        ));
    }

    match config.api.as_str() {
        "anthropic-messages" => {
            if credential.is_none() {
                return Err(ProviderConfigError(
                    "Anthropic-protocol models on openai-compatible providers require OAuth"
                        .to_string(),
                ));
            }
            let mut anthropic_config = AnthropicConfig::new(config.api_key.clone());
            anthropic_config.base_url = config.base_url.clone();
            anthropic_config.headers = config.headers.clone();
            anthropic_config.timeout_seconds = config.timeout_seconds;
            anthropic_config.max_retries = config.max_retries;
            anthropic_config.max_retry_delay_seconds = config.max_retry_delay_seconds;
            anthropic_config.provider_name = config.provider_name.clone();
            anthropic_config.bearer_auth = true;
            anthropic_config.credential_resolver = config.credential_resolver.clone();
            Ok(Arc::new(AnthropicProvider::new(anthropic_config)))
        }
        "google-generative-ai" => Ok(Arc::new(GoogleGenerativeAIProvider::new(config))),
        "mistral-conversations" => Ok(Arc::new(MistralConversationsProvider::new(config))),
        _ => Ok(Arc::new(OpenAICompatibleProvider::new(config))),
    }
}

fn create_codex_provider(
    codex: &OpenAICodexProviderConfig,
    credentials: &Arc<FileCredentialStore>,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Arc<dyn ModelProvider>, ProviderConfigError> {
    let reasoning_effort = codex_reasoning_effort(codex, model, thinking_level)?;
    let mut config = OpenAICodexConfig::new(codex_credential_resolver(
        codex.name.clone(),
        codex.credential_name.clone(),
        codex.api_key_env.clone(),
        credentials.clone(),
    ));
    config.base_url.clone_from(&codex.base_url);
    config.provider_name.clone_from(&codex.name);
    config.headers = Some(header_list(&codex.headers));
    config.timeout_seconds = codex.timeout_seconds;
    config.max_retries = u32::try_from(codex.max_retries).unwrap_or(u32::MAX);
    config.max_retry_delay_seconds = codex.max_retry_delay_seconds;
    config.reasoning_effort = reasoning_effort;
    Ok(Arc::new(OpenAICodexProvider::new(config)))
}

/// tau `_codex_reasoning_effort`: map the UI thinking level to a Codex
/// `reasoning.effort` value, validating it against the model's available modes.
fn codex_reasoning_effort(
    provider: &OpenAICodexProviderConfig,
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<Option<String>, ProviderConfigError> {
    let Some(thinking_level) = thinking_level else {
        return Ok(None);
    };
    let wrapped = ProviderConfig::OpenAICodex(provider.clone());
    if wrapped.thinking_parameter() != Some("reasoning.effort") {
        return Ok(None);
    }
    let levels = provider_thinking_levels(&wrapped, model);
    if levels.is_empty() {
        return Ok(None);
    }
    let normalized = normalize_thinking_level(Some(thinking_level)).map_err(ProviderConfigError)?;
    if !levels.iter().any(|level| level == &normalized) {
        let selected_model = model.unwrap_or(&provider.default_model);
        let available = levels.join(", ");
        return Err(ProviderConfigError(format!(
            "Thinking mode {normalized} is not available for {}:{selected_model}. Available modes: {available}",
            provider.name
        )));
    }
    if normalized == "off" {
        return Ok(None);
    }
    if normalized == "minimal" {
        return Ok(Some("low".to_string()));
    }
    Ok(Some(
        reasoning_effort_for_level(Some(&normalized)).map_err(ProviderConfigError)?,
    ))
}

/// tau `_oauth_credential`: the stored OAuth credential for an OAuth-backed
/// provider, or `None` when the provider has no credential name / is not an
/// OAuth provider / has nothing stored.
fn oauth_credential(
    provider: &ProviderConfig,
    credential_store: &FileCredentialStore,
) -> Option<OAuthCredential> {
    let credential_name = provider.credential_name()?;
    get_oauth_provider(provider.name())?;
    credential_store.get_oauth(credential_name).ok().flatten()
}

fn required_oauth_provider(name: &str) -> Result<Arc<dyn OAuthProvider>, ProviderConfigError> {
    get_oauth_provider(name).ok_or_else(|| {
        ProviderConfigError(format!("No OAuth implementation is registered for {name}"))
    })
}

/// Build a per-request auth resolver that refreshes and persists an OAuth
/// credential immediately before each call (tau `OAuthRuntimeCredentialResolver`).
fn runtime_auth_resolver(
    provider_name: String,
    credential_name: Option<String>,
    credential_store: Arc<FileCredentialStore>,
    oauth_provider: Arc<dyn OAuthProvider>,
) -> RuntimeProviderAuthResolver {
    Arc::new(
        move || -> BoxFuture<'static, Result<RuntimeProviderAuth, String>> {
            let provider_name = provider_name.clone();
            let credential_name = credential_name.clone();
            let credential_store = credential_store.clone();
            let oauth_provider = oauth_provider.clone();
            Box::pin(async move {
                let Some(credential_name) = credential_name else {
                    return Err(format!("Provider {provider_name} has no credential name"));
                };
                let credential = credential_store
                .get_oauth(&credential_name)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| {
                    format!(
                        "Missing OAuth credentials for {provider_name}. Run /login {provider_name}."
                    )
                })?;
                let client = ReqwestOAuthClient::default();
                let refreshed = oauth_provider
                    .refresh(&credential, &client, now_ms())
                    .await
                    .map_err(|err| err.to_string())?;
                if refreshed != credential {
                    credential_store
                        .set_oauth(&credential_name, refreshed.clone())
                        .map_err(|err| err.to_string())?;
                }
                let auth = oauth_provider.runtime_auth(&refreshed);
                Ok(RuntimeProviderAuth {
                    api_key: auth.api_key,
                    base_url: auth.base_url,
                    headers: auth.headers,
                })
            })
        },
    )
}

/// Build a Codex credential resolver (tau `OpenAICodexCredentialResolver`).
fn codex_credential_resolver(
    provider_name: String,
    credential_name: Option<String>,
    api_key_env: String,
    credential_store: Arc<FileCredentialStore>,
) -> rho_ai::env::OpenAICodexCredentialResolver {
    Arc::new(
        move || -> BoxFuture<'static, Result<OpenAICodexCredentials, String>> {
            let provider_name = provider_name.clone();
            let credential_name = credential_name.clone();
            let api_key_env = api_key_env.clone();
            let credential_store = credential_store.clone();
            Box::pin(async move {
                if let Some(credential_name) = credential_name.as_deref() {
                    if let Some(credential) = credential_store
                        .get_oauth(credential_name)
                        .map_err(|err| err.to_string())?
                    {
                        let credential =
                            codex_refresh_if_needed(credential_name, credential, &credential_store)
                                .await?;
                        let account_id = credential.account_id.ok_or_else(|| {
                            "OpenAI Codex OAuth credential is missing account_id".to_string()
                        })?;
                        return Ok(OpenAICodexCredentials {
                            access_token: credential.access,
                            account_id,
                        });
                    }
                }

                if let Ok(access_token) = std::env::var(&api_key_env) {
                    if !access_token.is_empty() {
                        let account_id =
                            account_id_from_access_token(&access_token).ok_or_else(|| {
                                format!("{api_key_env} must contain an OpenAI Codex access JWT")
                            })?;
                        return Ok(OpenAICodexCredentials {
                            access_token,
                            account_id,
                        });
                    }
                }

                Err(format!(
                    "Missing OpenAI Codex OAuth credentials. Run /login {provider_name}."
                ))
            })
        },
    )
}

async fn codex_refresh_if_needed(
    credential_name: &str,
    credential: OAuthCredential,
    credential_store: &FileCredentialStore,
) -> Result<OAuthCredential, String> {
    if !oauth_credential_is_expired(&credential, now_ms()) {
        return Ok(credential);
    }
    let client = ReqwestOAuthClient::default();
    let refreshed =
        crate::oauth::refresh_openai_codex_token(&credential.refresh, &client, now_ms())
            .await
            .map_err(|err| err.to_string())?;
    if refreshed != credential {
        credential_store
            .set_oauth(credential_name, refreshed.clone())
            .map_err(|err| err.to_string())?;
    }
    Ok(refreshed)
}

fn merge_headers(
    base: Option<rho_ai::types::HeaderList>,
    extra: Option<rho_ai::types::HeaderList>,
) -> rho_ai::types::HeaderList {
    let mut merged = base.unwrap_or_default();
    if let Some(extra) = extra {
        for (key, value) in extra {
            if let Some(existing) = merged.iter_mut().find(|(k, _)| *k == key) {
                existing.1 = value;
            } else {
                merged.push((key, value));
            }
        }
    }
    merged
}

fn header_list(headers: &indexmap::IndexMap<String, String>) -> rho_ai::types::HeaderList {
    headers
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_config() -> OpenAICodexProviderConfig {
        let mut config = OpenAICodexProviderConfig::new();
        config.name = "openai-codex".to_string();
        config.default_model = "gpt-5-codex".to_string();
        config.models = vec!["gpt-5-codex".to_string()];
        config
    }

    #[test]
    fn codex_reasoning_effort_none_without_thinking_level() {
        let config = codex_config();
        assert_eq!(codex_reasoning_effort(&config, None, None).unwrap(), None);
    }

    #[test]
    fn merge_headers_overrides_and_appends() {
        let base = vec![("a".to_string(), "1".to_string())];
        let extra = vec![
            ("a".to_string(), "2".to_string()),
            ("b".to_string(), "3".to_string()),
        ];
        let merged = merge_headers(Some(base), Some(extra));
        assert_eq!(
            merged,
            vec![
                ("a".to_string(), "2".to_string()),
                ("b".to_string(), "3".to_string()),
            ]
        );
    }
}
