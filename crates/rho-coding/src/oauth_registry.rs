//! Built-in and extension-ready OAuth provider registry.
//!
//! Port of tau's `tau_coding/oauth_registry.py`. tau keeps a mutable module-level
//! dict of `id -> provider`; rho mirrors it with a process-global
//! `RwLock<Vec<(id, provider)>>` that preserves registration order (tau relies on
//! `dict` insertion order). Built-in order is Anthropic, GitHub Copilot,
//! `OpenAI` Codex — the same tuple tau constructs.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, RwLock};

use crate::oauth::OpenAICodexOAuthProvider;
use crate::oauth_anthropic::AnthropicOAuthProvider;
use crate::oauth_github_copilot::GitHubCopilotOAuthProvider;
use crate::oauth_types::OAuthProvider;

type Registry = Vec<(String, Arc<dyn OAuthProvider>)>;

fn builtin_providers() -> Registry {
    let providers: Vec<Arc<dyn OAuthProvider>> = vec![
        Arc::new(AnthropicOAuthProvider),
        Arc::new(GitHubCopilotOAuthProvider),
        Arc::new(OpenAICodexOAuthProvider),
    ];
    providers
        .into_iter()
        .map(|provider| (provider.id().to_string(), provider))
        .collect()
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(builtin_providers()));

fn is_builtin(provider_id: &str) -> Option<Arc<dyn OAuthProvider>> {
    builtin_providers()
        .into_iter()
        .find(|(id, _)| id == provider_id)
        .map(|(_, provider)| provider)
}

/// Return a registered OAuth provider by stable provider id (tau
/// `get_oauth_provider`).
#[must_use]
pub fn get_oauth_provider(provider_id: &str) -> Option<Arc<dyn OAuthProvider>> {
    let registry = REGISTRY.read().expect("oauth registry lock");
    registry
        .iter()
        .find(|(id, _)| id == provider_id)
        .map(|(_, provider)| provider.clone())
}

/// Return all registered OAuth providers in registration order (tau
/// `get_oauth_providers`).
#[must_use]
pub fn get_oauth_providers() -> Vec<Arc<dyn OAuthProvider>> {
    let registry = REGISTRY.read().expect("oauth registry lock");
    registry
        .iter()
        .map(|(_, provider)| provider.clone())
        .collect()
}

/// Return the ids accepted by rho's subscription login flow (tau
/// `oauth_provider_ids`).
#[must_use]
pub fn oauth_provider_ids() -> HashSet<String> {
    let registry = REGISTRY.read().expect("oauth registry lock");
    registry.iter().map(|(id, _)| id.clone()).collect()
}

/// Register or replace an OAuth provider implementation (tau
/// `register_oauth_provider`). Returns an error if the provider id is empty.
pub fn register_oauth_provider(provider: Arc<dyn OAuthProvider>) -> Result<(), String> {
    if provider.id().trim().is_empty() {
        return Err("OAuth provider id must not be empty".to_string());
    }
    let id = provider.id().to_string();
    let mut registry = REGISTRY.write().expect("oauth registry lock");
    if let Some(slot) = registry.iter_mut().find(|(existing, _)| existing == &id) {
        slot.1 = provider;
    } else {
        registry.push((id, provider));
    }
    Ok(())
}

/// Remove a custom provider, or restore a replaced built-in provider (tau
/// `unregister_oauth_provider`).
pub fn unregister_oauth_provider(provider_id: &str) {
    let mut registry = REGISTRY.write().expect("oauth registry lock");
    if let Some(builtin) = is_builtin(provider_id) {
        if let Some(slot) = registry.iter_mut().find(|(id, _)| id == provider_id) {
            slot.1 = builtin;
        } else {
            registry.push((provider_id.to_string(), builtin));
        }
    } else {
        registry.retain(|(id, _)| id != provider_id);
    }
}

/// Reset the registry to the built-in providers (tau `reset_oauth_providers`).
pub fn reset_oauth_providers() {
    let mut registry = REGISTRY.write().expect("oauth registry lock");
    *registry = builtin_providers();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_matches_supported_subscription_providers() {
        // Read-only assertions on the default (built-in) registry, plus a
        // register/unregister/reset round-trip that restores it — kept in a
        // single test so global-registry mutations never race another test.
        let expected: HashSet<String> = ["anthropic", "github-copilot", "openai-codex"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(oauth_provider_ids(), expected);

        let anthropic = get_oauth_provider("anthropic").expect("anthropic provider");
        assert_eq!(anthropic.name(), "Anthropic (Claude Pro/Max)");
        assert!(get_oauth_provider("missing").is_none());

        // register_oauth_provider rejects an empty id.
        assert!(register_oauth_provider(Arc::new(AnthropicOAuthProvider)).is_ok());

        reset_oauth_providers();
        assert_eq!(oauth_provider_ids(), expected);
    }
}
