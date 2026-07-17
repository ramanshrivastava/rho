//! Provider-neutral OAuth contracts used by rho's coding application.
//!
//! Port of tau's `tau_coding/oauth_types.py`. The interactive login callbacks
//! (tau's `AuthCallback`/`PromptCallback`/…) are modeled as boxed closures so a
//! frontend (TUI/CLI) can drive a login flow; the browser flows that use them
//! are behind the manual checklist (`dev-notes/oauth-manual-checklist.md`). The
//! device-code flow (GitHub Copilot) is exercised by unit tests with a mock HTTP
//! client.

#![allow(clippy::doc_markdown)]

use futures::future::BoxFuture;
use rho_agent::types::JsonMap;
use serde_json::Value;

use crate::credentials::OAuthCredential;
use crate::oauth::OAuthError;
use crate::oauth_http::OAuthHttpClient;

/// Interactive flow families an OAuth provider supports (tau `OAuthFlowKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFlowKind {
    /// Browser authorization-code flow with a local callback server.
    Browser,
    /// RFC 8628 device-code flow.
    DeviceCode,
}

impl OAuthFlowKind {
    /// The tau wire string for this flow kind (`"browser"` / `"device_code"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::DeviceCode => "device_code",
        }
    }
}

/// Authorization URL and optional instructions for a browser flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthInfo {
    /// Authorization URL to open in the browser.
    pub url: String,
    /// Optional user-facing instructions.
    pub instructions: Option<String>,
}

/// User-facing values returned by an OAuth device authorization request.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthDeviceCodeInfo {
    /// The short code the user types on the verification page.
    pub user_code: String,
    /// The verification URL the user visits.
    pub verification_uri: String,
    /// Suggested polling interval, in seconds.
    pub interval_seconds: Option<f64>,
    /// Time until the device code expires, in seconds.
    pub expires_in_seconds: Option<f64>,
}

/// Text input requested by an OAuth provider before or during login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthPrompt {
    /// The prompt message shown to the user.
    pub message: String,
    /// Optional placeholder / example value.
    pub placeholder: Option<String>,
    /// Whether an empty response is acceptable.
    pub allow_empty: bool,
}

impl OAuthPrompt {
    /// Build a required prompt with no placeholder.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            placeholder: None,
            allow_empty: false,
        }
    }
}

/// One choice in a provider-defined OAuth selection prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSelectOption {
    /// Stable option id returned when chosen.
    pub id: String,
    /// User-facing option label.
    pub label: String,
}

/// Selection input requested by an OAuth provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSelectPrompt {
    /// The prompt message shown to the user.
    pub message: String,
    /// The available options.
    pub options: Vec<OAuthSelectOption>,
}

/// Request authentication derived from a stored OAuth credential (tau
/// `OAuthRuntimeAuth`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuthRuntimeAuth {
    /// Resolved API key / bearer token.
    pub api_key: String,
    /// Optional base-URL override.
    pub base_url: Option<String>,
    /// Optional extra request headers (insertion order preserved).
    pub headers: Option<rho_ai::types::HeaderList>,
}

/// Fire-and-forget callback invoked with a browser authorization URL.
pub type AuthCallback = Box<dyn Fn(OAuthAuthInfo) + Send + Sync>;
/// Fire-and-forget callback invoked with device-code details.
pub type DeviceCodeCallback = Box<dyn Fn(OAuthDeviceCodeInfo) + Send + Sync>;
/// Async callback requesting a text response (returns the entered value).
pub type PromptCallback =
    Box<dyn Fn(OAuthPrompt) -> BoxFuture<'static, Result<String, OAuthError>> + Send + Sync>;
/// Async callback requesting a selection (returns the chosen option id, if any).
pub type SelectCallback = Box<
    dyn Fn(OAuthSelectPrompt) -> BoxFuture<'static, Result<Option<String>, OAuthError>>
        + Send
        + Sync,
>;
/// Async callback yielding a manually pasted authorization code.
pub type ManualCodeCallback =
    Box<dyn Fn() -> BoxFuture<'static, Result<String, OAuthError>> + Send + Sync>;
/// Fire-and-forget progress message callback.
pub type ProgressCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Frontend-independent callbacks available to an OAuth login flow (tau
/// `OAuthLoginCallbacks`).
pub struct OAuthLoginCallbacks {
    /// Called with the authorization URL for browser flows.
    pub on_auth: AuthCallback,
    /// Called with device-code details for device flows.
    pub on_device_code: DeviceCodeCallback,
    /// Called to request a text value from the user.
    pub on_prompt: PromptCallback,
    /// Called to request a selection from the user.
    pub on_select: SelectCallback,
    /// Optional progress reporter.
    pub on_progress: Option<ProgressCallback>,
    /// Optional manual authorization-code entry callback.
    pub on_manual_code_input: Option<ManualCodeCallback>,
    /// Optional flow-kind hint chosen by the frontend.
    pub method: Option<OAuthFlowKind>,
}

/// Provider-specific OAuth behavior registered with rho (tau `OAuthProvider`).
///
/// The integrator (`provider_runtime.rs`) needs [`Self::refresh`] and
/// [`Self::runtime_auth`]; login is provider-specific and driven separately.
/// [`Self::refresh`] takes an injected HTTP client and an explicit `now_ms`
/// clock reading so refresh logic is deterministically testable.
#[async_trait::async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Stable provider/credential identifier.
    fn id(&self) -> &str;

    /// User-facing provider name.
    fn name(&self) -> &str;

    /// Interactive flow families supported by this provider.
    fn flow_kinds(&self) -> &[OAuthFlowKind];

    /// Refresh an expired credential (returns it unchanged if still valid).
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        client: &dyn OAuthHttpClient,
        now_ms: i64,
    ) -> Result<OAuthCredential, OAuthError>;

    /// Convert stored credentials to request auth.
    fn runtime_auth(&self, credential: &OAuthCredential) -> OAuthRuntimeAuth;
}

/// Return one non-empty string from provider-specific OAuth metadata (tau
/// `oauth_metadata_string`).
#[must_use]
pub fn oauth_metadata_string(metadata: &JsonMap, name: &str) -> Option<String> {
    match metadata.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}
