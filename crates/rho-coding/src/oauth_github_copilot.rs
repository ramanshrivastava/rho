//! GitHub Copilot OAuth device-code provider.
//!
//! Port of tau's `tau_coding/oauth_github_copilot.py`. The device login
//! ([`login_github_copilot`]) and token refresh ([`refresh_github_copilot_token`])
//! are unit-tested end-to-end with a mock HTTP client — the device flow needs no
//! local server, so it is fully reproducible (unlike the browser flows).

#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_possible_truncation, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use serde_json::Value;

use crate::credentials::OAuthCredential;
use crate::oauth::OAuthError;
use crate::oauth_device::{CancelSignal, DevicePollResult, poll_oauth_device_code};
use crate::oauth_http::{HttpMethod, OAuthHttpClient, OAuthHttpRequest, OAuthHttpResponse};
use crate::oauth_types::{
    OAuthDeviceCodeInfo, OAuthFlowKind, OAuthLoginCallbacks, OAuthPrompt, OAuthProvider,
    OAuthRuntimeAuth, oauth_metadata_string,
};

/// Stable GitHub Copilot provider/credential id.
pub const GITHUB_COPILOT_OAUTH_PROVIDER: &str = "github-copilot";
/// GitHub Copilot public OAuth client id.
pub const GITHUB_COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// GitHub Copilot API version header value.
pub const GITHUB_COPILOT_API_VERSION: &str = "2026-06-01";
/// Refresh a Copilot token this many milliseconds before its stated expiry.
pub const GITHUB_COPILOT_TOKEN_SKEW_MS: i64 = 5 * 60 * 1000;

const GITHUB_COPILOT_FLOW_KINDS: [OAuthFlowKind; 1] = [OAuthFlowKind::DeviceCode];

/// The fixed Copilot editor identification headers (tau `GITHUB_COPILOT_HEADERS`).
#[must_use]
pub fn github_copilot_headers() -> Vec<(String, String)> {
    vec![
        (
            "User-Agent".to_string(),
            "GitHubCopilotChat/0.35.0".to_string(),
        ),
        ("Editor-Version".to_string(), "vscode/1.107.0".to_string()),
        (
            "Editor-Plugin-Version".to_string(),
            "copilot-chat/0.35.0".to_string(),
        ),
        (
            "Copilot-Integration-Id".to_string(),
            "vscode-chat".to_string(),
        ),
    ]
}

fn github_copilot_user_agent() -> &'static str {
    "GitHubCopilotChat/0.35.0"
}

/// Validated GitHub device authorization response (tau `GitHubDeviceCode`).
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubDeviceCode {
    /// Opaque device code used when polling for the token.
    pub device_code: String,
    /// Short code the user types on the verification page.
    pub user_code: String,
    /// Verification URL the user visits.
    pub verification_uri: String,
    /// Suggested polling interval, in seconds.
    pub interval_seconds: f64,
    /// Time until the device code expires, in seconds.
    pub expires_in_seconds: f64,
}

/// Normalize a GitHub Enterprise URL/domain to a hostname (tau
/// `normalize_github_domain`).
#[must_use]
pub fn normalize_github_domain(value: &str) -> Option<String> {
    let stripped = value.trim();
    if stripped.is_empty() {
        return None;
    }
    let to_parse = if stripped.contains("://") {
        stripped.to_string()
    } else {
        format!("https://{stripped}")
    };
    let parsed = url::Url::parse(&to_parse).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    match parsed.host_str() {
        Some(host) if !host.is_empty() => Some(host.to_string()),
        _ => None,
    }
}

/// Derive the Copilot API URL encoded in a short-lived Copilot token (tau
/// `github_copilot_base_url`).
#[must_use]
pub fn github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        for field in token.split(';') {
            if let Some((key, value)) = field.split_once('=') {
                if key == "proxy-ep" && !value.is_empty() {
                    let host = value.strip_prefix("proxy.").unwrap_or(value);
                    return format!("https://api.{host}");
                }
            }
        }
    }
    if let Some(domain) = enterprise_domain.filter(|domain| !domain.is_empty()) {
        return format!("https://copilot-api.{domain}");
    }
    "https://api.individual.githubcopilot.com".to_string()
}

/// Run GitHub's device flow and exchange its token for Copilot auth (tau
/// `login_github_copilot`).
pub async fn login_github_copilot(
    callbacks: &OAuthLoginCallbacks,
    client: &dyn OAuthHttpClient,
    cancel: Option<Arc<CancelSignal>>,
    now_ms: i64,
) -> Result<OAuthCredential, OAuthError> {
    let mut prompt = OAuthPrompt::new("GitHub Enterprise URL/domain (blank for github.com)");
    prompt.placeholder = Some("company.ghe.com".to_string());
    prompt.allow_empty = true;
    let domain_input = (callbacks.on_prompt)(prompt).await?;
    if cancel.as_ref().is_some_and(|cancel| cancel.is_set()) {
        return Err(OAuthError("Login cancelled".to_string()));
    }
    let enterprise_domain = normalize_github_domain(&domain_input);
    if !domain_input.trim().is_empty() && enterprise_domain.is_none() {
        return Err(OAuthError(
            "Invalid GitHub Enterprise URL/domain".to_string(),
        ));
    }
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");

    let device = start_device_flow(domain, client).await?;
    (callbacks.on_device_code)(OAuthDeviceCodeInfo {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(device.expires_in_seconds),
    });
    let github_token = poll_github_access_token(domain, &device, client, cancel).await?;
    if let Some(progress) = &callbacks.on_progress {
        progress("Exchanging GitHub token for Copilot access...");
    }
    let mut metadata = rho_agent::types::JsonMap::new();
    if let Some(enterprise_domain) = &enterprise_domain {
        metadata.insert(
            "enterprise_domain".to_string(),
            Value::from(enterprise_domain.clone()),
        );
    }
    let seed = OAuthCredential {
        access: github_token.clone(),
        refresh: github_token,
        expires: 1,
        account_id: None,
        metadata,
    };
    refresh_github_copilot_token(&seed, client, now_ms).await
}

/// Exchange a long-lived GitHub token for a short-lived Copilot token (tau
/// `refresh_github_copilot_token`).
pub async fn refresh_github_copilot_token(
    credential: &OAuthCredential,
    client: &dyn OAuthHttpClient,
    _now_ms: i64,
) -> Result<OAuthCredential, OAuthError> {
    let enterprise_domain = oauth_metadata_string(&credential.metadata, "enterprise_domain");
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");

    let mut headers = vec![
        ("Accept".to_string(), "application/json".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {}", credential.refresh),
        ),
    ];
    headers.extend(github_copilot_headers());
    let response = client
        .send(OAuthHttpRequest {
            method: HttpMethod::Get,
            url: format!("https://api.{domain}/copilot_internal/v2/token"),
            headers,
            body: Vec::new(),
        })
        .await?;
    let raw = response_object(&response, "Copilot token", false)?;
    let token = required_string(&raw, "token", "Copilot token")?;
    let expires = match raw.get("expires_at") {
        Some(Value::Number(number)) => {
            let millis = if let Some(int_value) = number.as_i64() {
                int_value.saturating_mul(1000)
            } else {
                (number.as_f64().unwrap_or(0.0) * 1000.0) as i64
            };
            millis - GITHUB_COPILOT_TOKEN_SKEW_MS
        }
        _ => {
            return Err(OAuthError(
                "Copilot token response missing expires_at".to_string(),
            ));
        }
    };
    Ok(OAuthCredential {
        access: token,
        refresh: credential.refresh.clone(),
        expires,
        account_id: credential.account_id.clone(),
        metadata: credential.metadata.clone(),
    })
}

async fn start_device_flow(
    domain: &str,
    client: &dyn OAuthHttpClient,
) -> Result<GitHubDeviceCode, OAuthError> {
    let response = client
        .send(OAuthHttpRequest::post_form(
            format!("https://{domain}/login/device/code"),
            &[
                ("client_id", GITHUB_COPILOT_CLIENT_ID),
                ("scope", "read:user"),
            ],
            &[
                ("Accept", "application/json"),
                ("User-Agent", github_copilot_user_agent()),
            ],
        ))
        .await?;
    let raw = response_object(&response, "GitHub device code", false)?;
    let uri = required_string(&raw, "verification_uri", "GitHub device code")?;
    if !is_trusted_verification_uri(&uri) {
        return Err(OAuthError(
            "Untrusted verification_uri in device code response".to_string(),
        ));
    }
    let interval = match raw.get("interval") {
        None => 5.0,
        Some(value) => match value.as_f64() {
            Some(seconds) => seconds,
            None => {
                return Err(OAuthError(
                    "GitHub device code response has invalid interval".to_string(),
                ));
            }
        },
    };
    let Some(expires_in) = raw.get("expires_in").and_then(Value::as_f64) else {
        return Err(OAuthError(
            "GitHub device code response missing expires_in".to_string(),
        ));
    };
    Ok(GitHubDeviceCode {
        device_code: required_string(&raw, "device_code", "GitHub device code")?,
        user_code: required_string(&raw, "user_code", "GitHub device code")?,
        verification_uri: uri,
        interval_seconds: interval,
        expires_in_seconds: expires_in,
    })
}

fn is_trusted_verification_uri(uri: &str) -> bool {
    match url::Url::parse(uri) {
        Ok(parsed) => {
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some_and(|host| !host.is_empty())
        }
        Err(_) => false,
    }
}

async fn poll_github_access_token(
    domain: &str,
    device: &GitHubDeviceCode,
    client: &dyn OAuthHttpClient,
    cancel: Option<Arc<CancelSignal>>,
) -> Result<String, OAuthError> {
    let url = format!("https://{domain}/login/oauth/access_token");
    let poll = || {
        let url = url.clone();
        let device_code = device.device_code.clone();
        async move {
            let response = client
                .send(OAuthHttpRequest::post_form(
                    url,
                    &[
                        ("client_id", GITHUB_COPILOT_CLIENT_ID),
                        ("device_code", &device_code),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ],
                    &[
                        ("Accept", "application/json"),
                        ("User-Agent", github_copilot_user_agent()),
                    ],
                ))
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => return DevicePollResult::<String>::failed(error.0),
            };
            let raw = match response_object(&response, "GitHub device token", true) {
                Ok(raw) => raw,
                Err(error) => return DevicePollResult::failed(error.0),
            };
            if let Some(access_token) = raw.get("access_token").and_then(Value::as_str) {
                if !access_token.is_empty() {
                    return DevicePollResult::complete(access_token.to_string());
                }
            }
            match raw.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => DevicePollResult::pending(),
                Some("slow_down") => {
                    DevicePollResult::slow_down(raw.get("interval").and_then(Value::as_f64))
                }
                _ => {
                    let error = error_display(raw.get("error"));
                    let description = raw
                        .get("error_description")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty());
                    let suffix = description
                        .map(|value| format!(": {value}"))
                        .unwrap_or_default();
                    DevicePollResult::failed(format!("Device flow failed: {error}{suffix}"))
                }
            }
        }
    };

    poll_oauth_device_code(
        poll,
        Some(device.interval_seconds),
        Some(device.expires_in_seconds),
        true,
        cancel,
    )
    .await
}

fn error_display(error: Option<&Value>) -> String {
    match error {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn response_object(
    response: &OAuthHttpResponse,
    label: &str,
    accept_oauth_error: bool,
) -> Result<serde_json::Map<String, Value>, OAuthError> {
    if response.is_error() && !accept_oauth_error {
        return Err(OAuthError(format!(
            "{label} request failed ({})",
            response.status
        )));
    }
    match response.json_value() {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(OAuthError(format!("{label} response must be an object"))),
        Err(_) => Err(OAuthError(format!("{label} response was not valid JSON"))),
    }
}

fn required_string(
    raw: &serde_json::Map<String, Value>,
    name: &str,
    label: &str,
) -> Result<String, OAuthError> {
    match raw.get(name).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(OAuthError(format!("{label} response missing {name}"))),
    }
}

/// Registered GitHub Copilot OAuth behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitHubCopilotOAuthProvider;

#[async_trait::async_trait]
impl OAuthProvider for GitHubCopilotOAuthProvider {
    fn id(&self) -> &str {
        GITHUB_COPILOT_OAUTH_PROVIDER
    }

    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn flow_kinds(&self) -> &[OAuthFlowKind] {
        &GITHUB_COPILOT_FLOW_KINDS
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        client: &dyn OAuthHttpClient,
        now_ms: i64,
    ) -> Result<OAuthCredential, OAuthError> {
        if !crate::oauth::oauth_credential_is_expired(credential, now_ms) {
            return Ok(credential.clone());
        }
        refresh_github_copilot_token(credential, client, now_ms).await
    }

    fn runtime_auth(&self, credential: &OAuthCredential) -> OAuthRuntimeAuth {
        let enterprise_domain = oauth_metadata_string(&credential.metadata, "enterprise_domain");
        OAuthRuntimeAuth {
            api_key: credential.access.clone(),
            base_url: Some(github_copilot_base_url(
                Some(&credential.access),
                enterprise_domain.as_deref(),
            )),
            headers: Some(github_copilot_headers()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_http::MockHttpClient;

    fn callbacks(
        device_codes: Arc<std::sync::Mutex<Vec<OAuthDeviceCodeInfo>>>,
    ) -> OAuthLoginCallbacks {
        OAuthLoginCallbacks {
            on_auth: Box::new(|_info| {}),
            on_device_code: Box::new(move |info| device_codes.lock().unwrap().push(info)),
            on_prompt: Box::new(|_prompt| Box::pin(async { Ok(String::new()) })),
            on_select: Box::new(|_prompt| Box::pin(async { Ok(None) })),
            on_progress: None,
            on_manual_code_input: None,
            method: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn device_login_and_token_exchange() {
        let device_codes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let client = MockHttpClient::new(|request| {
            let url = url::Url::parse(&request.url).unwrap();
            match url.path() {
                "/login/device/code" => {
                    assert!(
                        String::from_utf8_lossy(&request.body)
                            .contains(&format!("client_id={GITHUB_COPILOT_CLIENT_ID}"))
                    );
                    OAuthHttpResponse::json(
                        200,
                        &serde_json::json!({
                            "device_code": "device-secret",
                            "user_code": "ABCD-1234",
                            "verification_uri": "https://github.com/login/device",
                            "interval": 0,
                            "expires_in": 60,
                        }),
                    )
                }
                "/login/oauth/access_token" => OAuthHttpResponse::json(
                    200,
                    &serde_json::json!({ "access_token": "github-token" }),
                ),
                "/copilot_internal/v2/token" => {
                    assert_eq!(
                        request
                            .headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                            .map(|(_, value)| value.as_str()),
                        Some("Bearer github-token")
                    );
                    OAuthHttpResponse::json(
                        200,
                        &serde_json::json!({
                            "token": "tid=1;exp=9999999999;proxy-ep=proxy.business.githubcopilot.com",
                            "expires_at": 9_999_999_999_i64,
                        }),
                    )
                }
                other => panic!("unexpected request path: {other}"),
            }
        });

        let credential = login_github_copilot(&callbacks(device_codes.clone()), &client, None, 0)
            .await
            .unwrap();

        assert_eq!(
            *device_codes.lock().unwrap(),
            vec![OAuthDeviceCodeInfo {
                user_code: "ABCD-1234".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
                interval_seconds: Some(0.0),
                expires_in_seconds: Some(60.0),
            }]
        );
        assert_eq!(credential.refresh, "github-token");
        assert!(credential.access.starts_with("tid=1"));
        assert_eq!(
            github_copilot_base_url(Some(&credential.access), None),
            "https://api.business.githubcopilot.com"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_untrusted_device_verification_uri() {
        let client = MockHttpClient::new(|_request| {
            OAuthHttpResponse::json(
                200,
                &serde_json::json!({
                    "device_code": "device",
                    "user_code": "code",
                    "verification_uri": "file:///tmp/not-safe",
                    "interval": 5,
                    "expires_in": 60,
                }),
            )
        });
        let device_codes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let error = login_github_copilot(&callbacks(device_codes), &client, None, 0)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Untrusted verification_uri"));
    }

    #[tokio::test]
    async fn refresh_preserves_enterprise_metadata() {
        let client = MockHttpClient::new(|request| {
            let url = url::Url::parse(&request.url).unwrap();
            assert_eq!(url.host_str(), Some("api.ghe.example.com"));
            OAuthHttpResponse::json(
                200,
                &serde_json::json!({ "token": "copilot", "expires_at": 9_999_999_999_i64 }),
            )
        });
        let mut metadata = rho_agent::types::JsonMap::new();
        metadata.insert(
            "enterprise_domain".to_string(),
            Value::from("ghe.example.com"),
        );
        let original = OAuthCredential {
            access: "old".to_string(),
            refresh: "github-token".to_string(),
            expires: 1,
            account_id: None,
            metadata,
        };
        let refreshed = refresh_github_copilot_token(&original, &client, 0)
            .await
            .unwrap();
        assert_eq!(refreshed.metadata, original.metadata);
        assert_eq!(
            normalize_github_domain("https://ghe.example.com/path").as_deref(),
            Some("ghe.example.com")
        );
        assert_eq!(
            github_copilot_base_url(None, Some("ghe.example.com")),
            "https://copilot-api.ghe.example.com"
        );
    }
}
