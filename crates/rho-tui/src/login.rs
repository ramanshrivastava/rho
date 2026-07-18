//! Login-flow plumbing for the interactive TUI (tau's `_open_login` /
//! `OAuthLoginScreen` worker + `_handle_*_login_result` helpers).
//!
//! The OAuth entry points in `rho-coding` (`login_anthropic`,
//! `login_openai_codex`, `login_github_copilot`) are async, open a browser or
//! print a device code, and bind a local callback socket. The immediate-mode TUI
//! cannot block its event loop on them, so this module runs the login on a
//! background task that talks to the [`crate::modals::OAuthLoginModal`] over
//! channels: the task pushes [`OAuthUpdate`]s (auth URL / device code / progress
//! / prompt) to the UI, and the UI pushes the user's manual-code / prompt
//! responses back through a code channel.
//!
//! Credential persistence mirrors tau's `_handle_oauth_login_result` /
//! `_handle_login_result` / `_logout`: write the credential to the
//! [`FileCredentialStore`], upsert the saved provider config, and leave the
//! session swap to the caller (which owns `&mut CodingSession`).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::FutureExt;
use indexmap::IndexMap;
use rho_agent::types::JsonMap;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use rho_coding::catalog_loader::save_user_catalog_entries;
use rho_coding::credentials::{FileCredentialStore, OAuthCredential};
use rho_coding::oauth::{OAuthError, login_openai_codex};
use rho_coding::oauth_anthropic::login_anthropic;
use rho_coding::oauth_github_copilot::login_github_copilot;
use rho_coding::oauth_http::ReqwestOAuthClient;
use rho_coding::oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectPrompt,
};
use rho_coding::paths::RhoPaths;
use rho_coding::provider_catalog::{
    BUILTIN_PROVIDER_CATALOG, ProviderCatalogEntry, builtin_provider_entry,
};
use rho_coding::provider_config::{
    OpenAICompatibleProviderConfig, load_provider_settings, provider_config_from_catalog_entry,
    upsert_openai_compatible_provider, upsert_saved_provider,
};

use crate::modals::CustomProviderDraft;

/// Which built-in OAuth login flow a provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthLoginKind {
    /// Anthropic Claude Pro/Max browser flow.
    Anthropic,
    /// `OpenAI` Codex (`ChatGPT`) browser flow.
    OpenAiCodex,
    /// `GitHub` Copilot device flow.
    GithubCopilot,
}

/// Map a provider name to its built-in OAuth login flow, if any.
#[must_use]
pub fn oauth_login_kind(provider_name: &str) -> Option<OAuthLoginKind> {
    match provider_name {
        "anthropic" => Some(OAuthLoginKind::Anthropic),
        "openai-codex" => Some(OAuthLoginKind::OpenAiCodex),
        "github-copilot" => Some(OAuthLoginKind::GithubCopilot),
        _ => None,
    }
}

/// A UI update pushed from the background login task to the OAuth modal.
#[derive(Debug, Clone)]
pub enum OAuthUpdate {
    /// A browser authorization URL and optional instructions.
    Auth {
        /// URL to open.
        url: String,
        /// Optional instructions to show.
        instructions: Option<String>,
    },
    /// Device-code details for a device flow.
    DeviceCode {
        /// Verification URL to visit.
        verification_uri: String,
        /// Short code to enter.
        user_code: String,
    },
    /// A progress / status message.
    Progress(String),
    /// A provider text prompt (the modal collects a line of input for it).
    Prompt {
        /// Prompt message.
        message: String,
        /// Whether an empty response is acceptable.
        allow_empty: bool,
    },
    /// A provider selection prompt (the modal presents the options for the user to
    /// choose the account/organization to authenticate).
    Select {
        /// Prompt message.
        message: String,
        /// Selectable `(id, label)` options; the chosen id flows back on the code
        /// channel like a manual-code entry.
        options: Vec<(String, String)>,
    },
}

/// Current wall-clock time in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Build the OAuth callbacks that bridge a background login task to the TUI.
fn build_callbacks(
    updates: &UnboundedSender<OAuthUpdate>,
    codes: Arc<Mutex<UnboundedReceiver<String>>>,
) -> OAuthLoginCallbacks {
    let auth_tx = updates.clone();
    let device_tx = updates.clone();
    let progress_tx = updates.clone();
    let prompt_tx = updates.clone();
    let prompt_codes = codes.clone();
    let select_tx = updates.clone();
    let select_codes = codes.clone();
    let manual_codes = codes;

    OAuthLoginCallbacks {
        on_auth: Box::new(move |info: OAuthAuthInfo| {
            let _ = auth_tx.send(OAuthUpdate::Auth {
                url: info.url,
                instructions: info.instructions,
            });
        }),
        on_device_code: Box::new(move |info: OAuthDeviceCodeInfo| {
            let _ = device_tx.send(OAuthUpdate::DeviceCode {
                verification_uri: info.verification_uri,
                user_code: info.user_code,
            });
        }),
        on_prompt: Box::new(move |prompt: OAuthPrompt| {
            let tx = prompt_tx.clone();
            let codes = prompt_codes.clone();
            async move {
                let _ = tx.send(OAuthUpdate::Prompt {
                    message: prompt.message,
                    allow_empty: prompt.allow_empty,
                });
                let mut guard = codes.lock().await;
                match guard.recv().await {
                    Some(value) => Ok(value),
                    None => Err(OAuthError("Login cancelled".to_string())),
                }
            }
            .boxed()
        }),
        on_select: Box::new(move |prompt: OAuthSelectPrompt| {
            let tx = select_tx.clone();
            let codes = select_codes.clone();
            async move {
                // A single option needs no user choice (mirrors auto-select when the
                // provider offers exactly one account/organization).
                if prompt.options.len() <= 1 {
                    return Ok(prompt.options.first().map(|option| option.id.clone()));
                }
                let options: Vec<(String, String)> = prompt
                    .options
                    .iter()
                    .map(|option| (option.id.clone(), option.label.clone()))
                    .collect();
                let valid_ids: Vec<String> = options.iter().map(|(id, _)| id.clone()).collect();
                let _ = tx.send(OAuthUpdate::Select {
                    message: prompt.message,
                    options,
                });
                // The modal sends back the chosen option id on the code channel.
                let mut guard = codes.lock().await;
                match guard.recv().await {
                    Some(value) if valid_ids.contains(&value) => Ok(Some(value)),
                    // A blank / unrecognized response means the user cancelled.
                    Some(_) | None => Err(OAuthError("Login cancelled".to_string())),
                }
            }
            .boxed()
        }),
        on_progress: Some(Box::new(move |message: &str| {
            let _ = progress_tx.send(OAuthUpdate::Progress(message.to_string()));
        })),
        on_manual_code_input: Some(Box::new(move || {
            let codes = manual_codes.clone();
            async move {
                let mut guard = codes.lock().await;
                match guard.recv().await {
                    Some(value) => Ok(value),
                    None => Err(OAuthError("Login cancelled".to_string())),
                }
            }
            .boxed()
        })),
        method: None,
    }
}

/// Spawn the background OAuth login task for `kind`.
///
/// The task drives the real (browser/device) flow and resolves to the fetched
/// credential or a human-readable error. The UI reflects progress via `updates`
/// and feeds manual input back via `codes`.
#[must_use]
pub fn spawn_oauth_login(
    kind: OAuthLoginKind,
    updates: UnboundedSender<OAuthUpdate>,
    codes: Arc<Mutex<UnboundedReceiver<String>>>,
) -> JoinHandle<Result<OAuthCredential, String>> {
    tokio::spawn(async move {
        let callbacks = build_callbacks(&updates, codes);
        let client = ReqwestOAuthClient::default();
        let now = now_ms();
        let result = match kind {
            OAuthLoginKind::Anthropic => login_anthropic(&callbacks, &client, now, true).await,
            OAuthLoginKind::OpenAiCodex => {
                login_openai_codex(&callbacks, &client, now, true, "tau").await
            }
            OAuthLoginKind::GithubCopilot => {
                login_github_copilot(&callbacks, &client, None, now).await
            }
        };
        result.map_err(|err| err.0)
    })
}

/// Persist an OAuth credential and its saved provider config (tau
/// `_handle_oauth_login_result`, before the session swap).
///
/// Returns the provider's display name on success. The caller then calls
/// `session.reload_provider_settings()` + `session.set_provider(name, false)`.
pub fn persist_oauth_login(
    store: &FileCredentialStore,
    entry: &ProviderCatalogEntry,
    credential: &OAuthCredential,
    paths: Option<&RhoPaths>,
) -> Result<String, String> {
    let Some(credential_name) = entry.credential_name.as_deref() else {
        return Err(format!(
            "Provider {} does not support saved credentials.",
            entry.name
        ));
    };
    store
        .set_oauth(credential_name, credential.clone())
        .map_err(|err| err.0)?;
    // Roll back the just-written credential if persisting the provider config
    // fails, so we never leave a saved credential without its provider entry.
    if let Err(err) = upsert_builtin_provider(store, &entry.name, paths) {
        let _ = store.delete(credential_name);
        return Err(err);
    }
    Ok(entry.display_name.clone())
}

/// Persist an API-key credential and its saved provider config (tau
/// `_handle_login_result`, before the session swap).
pub fn persist_api_key_login(
    store: &FileCredentialStore,
    entry: &ProviderCatalogEntry,
    api_key: &str,
    paths: Option<&RhoPaths>,
) -> Result<String, String> {
    let Some(credential_name) = entry.credential_name.as_deref() else {
        return Err(format!(
            "Provider {} does not support saved credentials.",
            entry.name
        ));
    };
    store.set(credential_name, api_key).map_err(|err| err.0)?;
    // Roll back the just-written key if persisting the provider config fails.
    if let Err(err) = upsert_builtin_provider(store, &entry.name, paths) {
        let _ = store.delete(credential_name);
        return Err(err);
    }
    Ok(entry.display_name.clone())
}

fn upsert_builtin_provider(
    store: &FileCredentialStore,
    name: &str,
    paths: Option<&RhoPaths>,
) -> Result<(), String> {
    let provider = provider_config_from_catalog_entry(name).map_err(|err| err.to_string())?;
    upsert_saved_provider(provider, false, paths, None, Some(store))
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// Persist a custom OpenAI-compatible provider (tau
/// `_handle_custom_provider_login_result`, before the session swap).
///
/// Returns the provider's stable name on success.
pub fn persist_custom_provider(
    store: &FileCredentialStore,
    draft: &CustomProviderDraft,
    paths: Option<&RhoPaths>,
) -> Result<String, String> {
    let base_url = draft.base_url.trim_end_matches('/').to_string();
    let mut provider = OpenAICompatibleProviderConfig::new(draft.provider_name.clone());
    provider.base_url.clone_from(&base_url);
    provider.api_key_env.clone_from(&draft.api_key_env);
    provider.credential_name = Some(draft.provider_name.clone());
    provider.models.clone_from(&draft.models);
    provider.default_model.clone_from(&draft.default_model);
    let catalog_entry = ProviderCatalogEntry {
        name: draft.provider_name.clone(),
        display_name: draft.display_name.clone(),
        kind: "openai-compatible".to_string(),
        base_url: base_url.clone(),
        api_key_env: draft.api_key_env.clone(),
        credential_name: Some(draft.provider_name.clone()),
        models: draft.models.clone(),
        default_model: draft.default_model.clone(),
        docs_url: base_url,
        api: None,
        context_windows: None,
        headers: IndexMap::new(),
        compat: JsonMap::new(),
        model_metadata: IndexMap::new(),
        thinking_levels: None,
        thinking_models: Vec::new(),
        thinking_default: None,
        thinking_parameter: None,
        auth_methods: vec!["api_key".to_string()],
    };
    // Compute the updated provider settings from the current on-disk state BEFORE
    // any write, so an invalid config fails without persisting anything.
    let settings = load_provider_settings(paths, Some(store)).map_err(|err| err.to_string())?;
    let updated = upsert_openai_compatible_provider(&settings, provider, false)
        .map_err(|err| err.to_string())?;

    // Write the catalog entry (model metadata) first. A catalog entry without a
    // matching credential + provider entry is inert and is overwritten on retry,
    // so it needs no rollback.
    save_user_catalog_entries(&[catalog_entry], paths).map_err(|err| err.to_string())?;
    // Then the credential and provider settings — the pair that makes the provider
    // usable and discoverable by `/logout`. Roll back the credential if the
    // settings write fails so the two never drift.
    store
        .set(&draft.provider_name, &draft.api_key)
        .map_err(|err| err.0)?;
    if let Err(err) = rho_coding::provider_config::save_provider_settings(&updated, paths) {
        let _ = store.delete(&draft.provider_name);
        return Err(err.to_string());
    }
    Ok(draft.provider_name.clone())
}

/// A provider that currently has a stored credential, offered by the logout
/// picker (tau `_stored_credential_providers`, extended to saved custom
/// providers so credentials written by custom login can also be removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredentialProvider {
    /// Stable provider id.
    pub name: String,
    /// User-facing display name.
    pub display_name: String,
}

/// Whether the store holds a credential (API key or OAuth) under `credential_name`.
fn credential_store_has_entry(
    store: &FileCredentialStore,
    credential_name: &str,
) -> Result<bool, String> {
    Ok(store.get(credential_name).map_err(|err| err.0)?.is_some()
        || store
            .get_oauth(credential_name)
            .map_err(|err| err.0)?
            .is_some())
}

/// Resolve the credential name for a provider: built-in providers from the
/// catalog, otherwise saved (custom) providers from `providers.json`. Returns
/// `Err` only for a provider that is neither.
fn resolve_credential_name(
    store: &FileCredentialStore,
    provider_name: &str,
    paths: Option<&RhoPaths>,
) -> Result<Option<String>, String> {
    if let Some(entry) = builtin_provider_entry(provider_name) {
        return Ok(entry.credential_name.clone());
    }
    let settings = load_provider_settings(paths, Some(store)).map_err(|err| err.to_string())?;
    match settings
        .providers
        .iter()
        .find(|provider| provider.name() == provider_name)
    {
        Some(provider) => Ok(provider.credential_name().map(str::to_string)),
        None => Err(format!("Unknown provider: {provider_name}")),
    }
}

/// The providers with stored credentials (tau `_stored_credential_providers`),
/// including saved custom providers so custom logins are removable.
pub fn stored_credential_providers(
    store: &FileCredentialStore,
    paths: Option<&RhoPaths>,
) -> Vec<StoredCredentialProvider> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Built-in providers keyed by their catalog display name.
    for entry in BUILTIN_PROVIDER_CATALOG.iter() {
        if let Some(name) = entry.credential_name.as_deref() {
            if credential_store_has_entry(store, name).unwrap_or(false) {
                seen.insert(entry.name.clone());
                out.push(StoredCredentialProvider {
                    name: entry.name.clone(),
                    display_name: entry.display_name.clone(),
                });
            }
        }
    }
    // Saved custom providers from providers.json (their credential name equals the
    // provider id). Built-ins already listed above are skipped via `seen`.
    if let Ok(settings) = load_provider_settings(paths, Some(store)) {
        for provider in &settings.providers {
            if seen.contains(provider.name()) {
                continue;
            }
            if let Some(name) = provider.credential_name() {
                if credential_store_has_entry(store, name).unwrap_or(false) {
                    seen.insert(provider.name().to_string());
                    out.push(StoredCredentialProvider {
                        name: provider.name().to_string(),
                        display_name: provider.name().to_string(),
                    });
                }
            }
        }
    }
    out
}

/// Remove a provider's stored credentials (tau `_logout`). Returns whether a
/// credential was actually present (so the caller can mirror tau's messaging).
/// Handles built-in and saved custom providers alike.
pub fn logout_provider(
    store: &FileCredentialStore,
    provider_name: &str,
    paths: Option<&RhoPaths>,
) -> Result<bool, String> {
    let Some(credential_name) = resolve_credential_name(store, provider_name, paths)? else {
        return Ok(false);
    };
    if !credential_store_has_entry(store, &credential_name)? {
        return Ok(false);
    }
    store.delete(&credential_name).map_err(|err| err.0)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_coding::credentials::OAuthCredential;

    /// A temp credential store plus an isolated [`RhoPaths`] so provider-settings
    /// and catalog writes stay off the user's real `~/.rho` during tests.
    fn temp_store() -> (tempfile::TempDir, FileCredentialStore, RhoPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        let paths = RhoPaths::new(dir.path().join("home"), dir.path().join("agents"));
        (dir, store, paths)
    }

    #[test]
    fn oauth_login_kind_maps_builtin_providers() {
        assert_eq!(
            oauth_login_kind("anthropic"),
            Some(OAuthLoginKind::Anthropic)
        );
        assert_eq!(
            oauth_login_kind("openai-codex"),
            Some(OAuthLoginKind::OpenAiCodex)
        );
        assert_eq!(
            oauth_login_kind("github-copilot"),
            Some(OAuthLoginKind::GithubCopilot)
        );
        assert_eq!(oauth_login_kind("openai"), None);
    }

    #[test]
    fn persist_oauth_login_stores_credential() {
        let (_dir, store, paths) = temp_store();
        let entry = builtin_provider_entry("anthropic").expect("anthropic entry");
        let credential = OAuthCredential::new("access-token", "refresh-token", now_ms() + 60_000);
        let display =
            persist_oauth_login(&store, &entry, &credential, Some(&paths)).expect("persist");
        assert_eq!(display, entry.display_name);
        let stored = store
            .get_oauth(entry.credential_name.as_deref().unwrap())
            .expect("read")
            .expect("some credential");
        assert_eq!(stored.access, "access-token");
        assert_eq!(stored.refresh, "refresh-token");
    }

    #[test]
    fn persist_api_key_login_stores_key() {
        let (_dir, store, paths) = temp_store();
        let entry = builtin_provider_entry("openai").expect("openai entry");
        let display =
            persist_api_key_login(&store, &entry, "sk-test-key", Some(&paths)).expect("persist");
        assert_eq!(display, entry.display_name);
        let stored = store
            .get(entry.credential_name.as_deref().unwrap())
            .expect("read")
            .expect("some key");
        assert_eq!(stored, "sk-test-key");
    }

    #[test]
    fn logout_reports_presence() {
        let (_dir, store, paths) = temp_store();
        let entry = builtin_provider_entry("openai").expect("openai entry");
        // Nothing stored yet.
        assert!(!logout_provider(&store, "openai", Some(&paths)).expect("logout"));
        // Store a key, then logout removes it and reports true.
        persist_api_key_login(&store, &entry, "sk-test-key", Some(&paths)).expect("persist");
        assert!(logout_provider(&store, "openai", Some(&paths)).expect("logout"));
        assert!(
            store
                .get(entry.credential_name.as_deref().unwrap())
                .expect("read")
                .is_none()
        );
    }

    fn custom_draft(name: &str) -> CustomProviderDraft {
        CustomProviderDraft {
            provider_name: name.to_string(),
            display_name: format!("{name} (custom)"),
            base_url: "https://api.example.com/v1".to_string(),
            api_key_env: "EXAMPLE_API_KEY".to_string(),
            models: vec!["example-model".to_string()],
            default_model: "example-model".to_string(),
            api_key: "sk-custom-key".to_string(),
        }
    }

    #[test]
    fn custom_provider_credentials_are_removable_via_logout() {
        let (_dir, store, paths) = temp_store();
        let draft = custom_draft("my-custom");
        let name = persist_custom_provider(&store, &draft, Some(&paths)).expect("persist custom");
        assert_eq!(name, "my-custom");
        // The credential lives under the provider id and is discoverable via the
        // saved provider settings (not the built-in catalog).
        assert!(builtin_provider_entry("my-custom").is_none());
        let listed = stored_credential_providers(&store, Some(&paths));
        assert!(listed.iter().any(|p| p.name == "my-custom"));
        // Logout resolves the custom provider's credential name and removes it.
        assert!(logout_provider(&store, "my-custom", Some(&paths)).expect("logout"));
        assert!(store.get("my-custom").expect("read").is_none());
        assert!(!logout_provider(&store, "my-custom", Some(&paths)).expect("logout again"));
    }

    #[test]
    fn logout_unknown_provider_errors() {
        let (_dir, store, paths) = temp_store();
        let err = logout_provider(&store, "does-not-exist", Some(&paths)).expect_err("unknown");
        assert!(err.contains("Unknown provider"));
    }

    #[test]
    fn persist_api_key_rolls_back_credential_on_provider_failure() {
        let (_dir, store, paths) = temp_store();
        // `provider_config_from_catalog_entry` rejects a non-built-in name, so the
        // provider upsert fails and the credential write must be rolled back.
        let entry = ProviderCatalogEntry {
            name: "not-a-builtin".to_string(),
            display_name: "Not A Builtin".to_string(),
            kind: "openai-compatible".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key_env: "X".to_string(),
            credential_name: Some("not-a-builtin".to_string()),
            models: vec!["m".to_string()],
            default_model: "m".to_string(),
            docs_url: "https://api.example.com/v1".to_string(),
            api: None,
            context_windows: None,
            headers: IndexMap::new(),
            compat: JsonMap::new(),
            model_metadata: IndexMap::new(),
            thinking_levels: None,
            thinking_models: Vec::new(),
            thinking_default: None,
            thinking_parameter: None,
            auth_methods: vec!["api_key".to_string()],
        };
        assert!(persist_api_key_login(&store, &entry, "sk-test-key", Some(&paths)).is_err());
        // The credential must not survive the failed provider upsert.
        assert!(store.get("not-a-builtin").expect("read").is_none());
    }
}
