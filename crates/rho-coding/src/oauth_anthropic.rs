//! Anthropic Claude Pro/Max OAuth provider.
//!
//! Port of tau's `tau_coding/oauth_anthropic.py`. The token refresh
//! ([`refresh_anthropic_token`]) is unit-tested with a mock HTTP client; the
//! browser login ([`login_anthropic`]) is interactive and manual-only (see
//! `dev-notes/oauth-manual-checklist.md`).

#![allow(clippy::doc_markdown)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::unnecessary_literal_bound
)]

use serde_json::Value;

use crate::credentials::OAuthCredential;
use crate::oauth::{
    OAuthError, create_pkce_pair, oauth_credential_is_expired, parse_authorization_input,
    start_local_oauth_server, urlencode_form,
};
use crate::oauth_http::{OAuthHttpClient, OAuthHttpRequest};
use crate::oauth_types::{
    OAuthAuthInfo, OAuthFlowKind, OAuthLoginCallbacks, OAuthPrompt, OAuthProvider, OAuthRuntimeAuth,
};

/// Stable Anthropic provider/credential id.
pub const ANTHROPIC_OAUTH_PROVIDER: &str = "anthropic";
/// Anthropic public OAuth client id.
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944a1962f5e";
/// Anthropic authorization endpoint.
pub const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Anthropic token endpoint.
pub const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Anthropic OAuth redirect URI (local callback).
pub const ANTHROPIC_REDIRECT_URI: &str = "http://localhost:53692/callback";
/// Anthropic requested scopes.
pub const ANTHROPIC_SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Port the local Anthropic callback server binds.
pub const ANTHROPIC_CALLBACK_PORT: u16 = 53692;
/// Refresh an Anthropic token this many milliseconds before its stated expiry.
pub const ANTHROPIC_TOKEN_SKEW_MS: i64 = 5 * 60 * 1000;

const ANTHROPIC_FLOW_KINDS: [OAuthFlowKind; 1] = [OAuthFlowKind::Browser];

/// Refresh Anthropic OAuth credentials (tau `refresh_anthropic_token`).
pub async fn refresh_anthropic_token(
    refresh_token: &str,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
) -> Result<OAuthCredential, OAuthError> {
    let data = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": ANTHROPIC_CLIENT_ID,
        "refresh_token": refresh_token,
    });
    anthropic_token_request(&data, client, "refresh", Some(refresh_token), now_ms).await
}

async fn anthropic_token_request(
    data: &Value,
    client: &dyn OAuthHttpClient,
    action: &str,
    previous_refresh: Option<&str>,
    now_ms: i64,
) -> Result<OAuthCredential, OAuthError> {
    let response = client
        .send(OAuthHttpRequest::post_json(
            ANTHROPIC_TOKEN_URL,
            data,
            &[("Accept", "application/json")],
        ))
        .await?;
    if response.is_error() {
        // Deliberately omit the response body (may carry the token) — parity with
        // tau, which redacts everything but the status.
        return Err(OAuthError(format!(
            "Anthropic token {action} failed ({})",
            response.status
        )));
    }
    let Ok(Value::Object(raw)) = response.json_value() else {
        return Err(OAuthError(format!(
            "Anthropic token {action} response must be an object"
        )));
    };
    let access = required_string(&raw, "access_token", action)?;
    let refresh =
        optional_string(&raw, "refresh_token").or_else(|| previous_refresh.map(str::to_string));
    let expires_in = raw.get("expires_in");
    let Some(refresh) = refresh else {
        return Err(OAuthError(format!(
            "Anthropic token {action} response missing refresh_token"
        )));
    };
    let expires_in = match expires_in.and_then(Value::as_f64) {
        Some(value) if value > 0.0 => value,
        _ => {
            return Err(OAuthError(format!(
                "Anthropic token {action} response missing expires_in"
            )));
        }
    };
    let expires = (now_ms as f64 + expires_in * 1000.0 - ANTHROPIC_TOKEN_SKEW_MS as f64) as i64;
    Ok(OAuthCredential {
        access,
        refresh,
        expires,
        account_id: None,
        metadata: rho_agent::types::JsonMap::new(),
    })
}

fn required_string(
    raw: &serde_json::Map<String, Value>,
    name: &str,
    action: &str,
) -> Result<String, OAuthError> {
    match raw.get(name).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(OAuthError(format!(
            "Anthropic token {action} response missing {name}"
        ))),
    }
}

fn optional_string(raw: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    match raw.get(name).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

/// Registered Anthropic Claude subscription OAuth behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicOAuthProvider;

#[async_trait::async_trait]
impl OAuthProvider for AnthropicOAuthProvider {
    fn id(&self) -> &str {
        ANTHROPIC_OAUTH_PROVIDER
    }

    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    fn flow_kinds(&self) -> &[OAuthFlowKind] {
        &ANTHROPIC_FLOW_KINDS
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        client: &dyn OAuthHttpClient,
        now_ms: i64,
    ) -> Result<OAuthCredential, OAuthError> {
        if !oauth_credential_is_expired(credential, now_ms) {
            return Ok(credential.clone());
        }
        refresh_anthropic_token(&credential.refresh, client, now_ms).await
    }

    fn runtime_auth(&self, credential: &OAuthCredential) -> OAuthRuntimeAuth {
        OAuthRuntimeAuth {
            api_key: credential.access.clone(),
            base_url: None,
            headers: Some(vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {}", credential.access),
                ),
                (
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,oauth-2025-04-20".to_string(),
                ),
                ("user-agent".to_string(), "claude-cli/tau".to_string()),
                ("x-app".to_string(), "cli".to_string()),
            ]),
        }
    }
}

/// Run Anthropic's authorization-code + PKCE login flow (tau `login_anthropic`).
///
/// **Interactive; not unit-tested — see the manual checklist.**
pub async fn login_anthropic(
    callbacks: &OAuthLoginCallbacks,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
    open_browser: bool,
) -> Result<OAuthCredential, OAuthError> {
    let (verifier, challenge) = create_pkce_pair();
    let params: [(&str, &str); 8] = [
        ("code", "true"),
        ("client_id", ANTHROPIC_CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", ANTHROPIC_REDIRECT_URI),
        ("scope", ANTHROPIC_SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &verifier),
    ];
    let url = format!("{ANTHROPIC_AUTHORIZE_URL}?{}", urlencode_form(&params));
    let server = start_local_oauth_server(
        &verifier,
        ANTHROPIC_CALLBACK_PORT,
        "/callback",
        "Anthropic authentication completed. You can close this window.",
    );
    (callbacks.on_auth)(OAuthAuthInfo {
        url: url.clone(),
        instructions: Some(
            "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.".to_string(),
        ),
    });
    if open_browser {
        crate::oauth::open_url(&url);
    }

    let result = login_anthropic_inner(callbacks, client, now_ms, &verifier, server.as_ref()).await;
    if let Some(server) = server.as_ref() {
        server.close();
    }
    result
}

async fn login_anthropic_inner(
    callbacks: &OAuthLoginCallbacks,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
    verifier: &str,
    server: Option<&crate::oauth::LocalOAuthServer>,
) -> Result<OAuthCredential, OAuthError> {
    let value = match (server, callbacks.on_manual_code_input.as_ref()) {
        (Some(server), Some(manual)) => {
            let result = tokio::select! {
                code = server.wait_for_code() => code,
                manual = manual() => Some(manual?),
            };
            server.cancel_wait();
            result
        }
        (Some(server), None) => server.wait_for_code().await,
        (None, Some(manual)) => Some(manual().await?),
        (None, None) => None,
    };
    let value = if let Some(value) = value {
        value
    } else {
        let mut prompt = OAuthPrompt::new("Paste the authorization code or full redirect URL:");
        prompt.placeholder = Some(ANTHROPIC_REDIRECT_URI.to_string());
        (callbacks.on_prompt)(prompt).await?
    };
    let parsed = parse_authorization_input(&value);
    if let Some(state) = &parsed.state {
        if state != verifier {
            return Err(OAuthError("OAuth state mismatch".to_string()));
        }
    }
    let Some(code) = parsed.code.filter(|value| !value.is_empty()) else {
        return Err(OAuthError("Missing authorization code".to_string()));
    };
    if let Some(progress) = &callbacks.on_progress {
        progress("Exchanging authorization code for tokens...");
    }
    let data = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": ANTHROPIC_CLIENT_ID,
        "code": code,
        "state": parsed.state.clone().unwrap_or_else(|| verifier.to_string()),
        "redirect_uri": ANTHROPIC_REDIRECT_URI,
        "code_verifier": verifier,
    });
    anthropic_token_request(&data, client, "exchange", None, now_ms).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_http::{MockHttpClient, OAuthHttpResponse};

    fn header<'a>(request: &'a OAuthHttpRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn refresh_uses_json_and_redacts_failed_response() {
        let client = MockHttpClient::new(|request| {
            assert_eq!(request.url, ANTHROPIC_TOKEN_URL);
            assert_eq!(header(request, "content-type"), Some("application/json"));
            assert!(!request.body.is_empty());
            let body = String::from_utf8_lossy(&request.body);
            assert!(body.contains(ANTHROPIC_CLIENT_ID));
            OAuthHttpResponse::text_response(401, "secret-token-body")
        });
        let error = refresh_anthropic_token("refresh-secret", &client, 0)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("401"), "{message}");
        assert!(!message.contains("secret-token-body"), "{message}");
        assert!(!message.contains("refresh-secret"), "{message}");
    }

    #[tokio::test]
    async fn refresh_returns_provider_neutral_credential() {
        let client = MockHttpClient::new(|_request| {
            OAuthHttpResponse::json(
                200,
                &serde_json::json!({
                    "access_token": "anthropic-access",
                    "refresh_token": "anthropic-refresh",
                    "expires_in": 3600,
                }),
            )
        });
        let credential = refresh_anthropic_token("old-refresh", &client, 1_700_000_000_123)
            .await
            .unwrap();
        assert_eq!(credential.access, "anthropic-access");
        assert_eq!(credential.refresh, "anthropic-refresh");
        assert_eq!(credential.account_id, None);
        assert!(credential.expires > 0);
    }
}
