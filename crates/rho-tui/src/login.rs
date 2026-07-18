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
use rho_coding::provider_catalog::{ProviderCatalogEntry, builtin_provider_entry};
use rho_coding::provider_config::{
    OpenAICompatibleProviderConfig, provider_config_from_catalog_entry,
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
            async move { Ok(prompt.options.first().map(|option| option.id.clone())) }.boxed()
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
    upsert_builtin_provider(store, &entry.name)?;
    Ok(entry.display_name.clone())
}

/// Persist an API-key credential and its saved provider config (tau
/// `_handle_login_result`, before the session swap).
pub fn persist_api_key_login(
    store: &FileCredentialStore,
    entry: &ProviderCatalogEntry,
    api_key: &str,
) -> Result<String, String> {
    let Some(credential_name) = entry.credential_name.as_deref() else {
        return Err(format!(
            "Provider {} does not support saved credentials.",
            entry.name
        ));
    };
    store.set(credential_name, api_key).map_err(|err| err.0)?;
    upsert_builtin_provider(store, &entry.name)?;
    Ok(entry.display_name.clone())
}

fn upsert_builtin_provider(store: &FileCredentialStore, name: &str) -> Result<(), String> {
    let provider = provider_config_from_catalog_entry(name).map_err(|err| err.to_string())?;
    upsert_saved_provider(provider, false, None, None, Some(store))
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
    save_user_catalog_entries(&[catalog_entry], None).map_err(|err| err.to_string())?;
    store
        .set(&draft.provider_name, &draft.api_key)
        .map_err(|err| err.0)?;
    let settings = rho_coding::provider_config::load_provider_settings(None, Some(store))
        .map_err(|err| err.to_string())?;
    let updated = upsert_openai_compatible_provider(&settings, provider, false)
        .map_err(|err| err.to_string())?;
    rho_coding::provider_config::save_provider_settings(&updated, None)
        .map_err(|err| err.to_string())?;
    Ok(draft.provider_name.clone())
}

/// Remove a provider's stored credentials (tau `_logout`). Returns whether a
/// credential was actually present (so the caller can mirror tau's messaging).
pub fn logout_provider(store: &FileCredentialStore, provider_name: &str) -> Result<bool, String> {
    let Some(entry) = builtin_provider_entry(provider_name) else {
        return Err(format!("Unknown provider: {provider_name}"));
    };
    let Some(credential_name) = entry.credential_name.as_deref() else {
        return Ok(false);
    };
    let has_entry = store.get(credential_name).map_err(|err| err.0)?.is_some()
        || store
            .get_oauth(credential_name)
            .map_err(|err| err.0)?
            .is_some();
    if !has_entry {
        return Ok(false);
    }
    store.delete(credential_name).map_err(|err| err.0)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_coding::credentials::OAuthCredential;

    fn temp_store() -> (tempfile::TempDir, FileCredentialStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        (dir, store)
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
        let (_dir, store) = temp_store();
        let entry = builtin_provider_entry("anthropic").expect("anthropic entry");
        let credential = OAuthCredential::new("access-token", "refresh-token", now_ms() + 60_000);
        let display = persist_oauth_login(&store, &entry, &credential).expect("persist");
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
        let (_dir, store) = temp_store();
        let entry = builtin_provider_entry("openai").expect("openai entry");
        let display = persist_api_key_login(&store, &entry, "sk-test-key").expect("persist");
        assert_eq!(display, entry.display_name);
        let stored = store
            .get(entry.credential_name.as_deref().unwrap())
            .expect("read")
            .expect("some key");
        assert_eq!(stored, "sk-test-key");
    }

    #[test]
    fn logout_reports_presence() {
        let (_dir, store) = temp_store();
        let entry = builtin_provider_entry("openai").expect("openai entry");
        // Nothing stored yet.
        assert!(!logout_provider(&store, "openai").expect("logout"));
        // Store a key, then logout removes it and reports true.
        persist_api_key_login(&store, &entry, "sk-test-key").expect("persist");
        assert!(logout_provider(&store, "openai").expect("logout"));
        assert!(
            store
                .get(entry.credential_name.as_deref().unwrap())
                .expect("read")
                .is_none()
        );
    }
}
