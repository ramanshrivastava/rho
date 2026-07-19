//! OAuth helpers for subscription-backed coding providers.
//!
//! Port of tau's `tau_coding/oauth.py`: the OpenAI Codex (ChatGPT subscription)
//! provider plus the shared primitives every provider reuses — PKCE, base64url,
//! `application/x-www-form-urlencoded` encoding, authorization-input parsing, the
//! `oauth_credential_is_expired` skew check, JWT account-id extraction, the token
//! exchange/refresh calls, and the local browser-callback server.
//!
//! ## Time seam
//!
//! tau reads `time.time()` directly. rho threads an explicit `now_ms` (integer
//! milliseconds, the same granularity as [`rho_agent::clock::Clock::now_ms`]) so
//! expiry and refresh are deterministic under test — production passes
//! `clock.now_ms()`.
//!
//! ## Network seam
//!
//! Token calls go through [`crate::oauth_http::OAuthHttpClient`]; tests inject a
//! mock, matching tau's `httpx.MockTransport`. **No unit test hits the network.**
//!
//! ## Interactive flows (manual only)
//!
//! [`login_openai_codex`] and the [`LocalOAuthServer`] drive an interactive
//! browser flow and are **not** unit-tested — see
//! `dev-notes/oauth-manual-checklist.md`.

#![allow(clippy::doc_markdown)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::unnecessary_literal_bound
)]

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, ToSocketAddrs as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest as _, Sha256};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use crate::credentials::OAuthCredential;
use crate::oauth_http::{OAuthHttpClient, OAuthHttpRequest};
use crate::oauth_types::{
    OAuthAuthInfo, OAuthFlowKind, OAuthLoginCallbacks, OAuthPrompt, OAuthProvider, OAuthRuntimeAuth,
};

/// Stable OpenAI Codex provider/credential id.
pub const OPENAI_CODEX_OAUTH_PROVIDER: &str = "openai-codex";
/// OpenAI Codex public OAuth client id.
pub const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// OpenAI Codex authorization endpoint.
pub const OPENAI_CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// OpenAI Codex token endpoint.
pub const OPENAI_CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// OpenAI Codex OAuth redirect URI (local callback).
pub const OPENAI_CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// OpenAI Codex requested scopes.
pub const OPENAI_CODEX_SCOPE: &str = "openid profile email offline_access";
/// JWT claim holding the ChatGPT auth object.
pub const OPENAI_CODEX_ACCOUNT_CLAIM: &str = "https://api.openai.com/auth";
/// Port the local OpenAI Codex callback server binds.
pub const OPENAI_CODEX_CALLBACK_PORT: u16 = 1455;
/// Refresh a token this many milliseconds before its stated expiry.
pub const TOKEN_REFRESH_SKEW_MS: i64 = 60_000;

const OPENAI_CODEX_FLOW_KINDS: [OAuthFlowKind; 1] = [OAuthFlowKind::Browser];

/// Raised when an OAuth flow cannot complete (tau `OAuthError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OAuthError(pub String);

/// Parsed OAuth authorization callback data (tau `AuthorizationCode`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizationCode {
    /// The authorization code, if present.
    pub code: Option<String>,
    /// The `state` value, if present.
    pub state: Option<String>,
}

/// OpenAI Codex OAuth authorization flow state (tau `AuthorizationFlow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationFlow {
    /// PKCE verifier.
    pub verifier: String,
    /// CSRF `state` value.
    pub state: String,
    /// The fully-built authorization URL.
    pub url: String,
}

/// Successful OAuth token response (tau `TokenResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenResponse {
    /// Access token.
    pub access: String,
    /// Refresh token.
    pub refresh: String,
    /// Expiry, in integer milliseconds.
    pub expires: i64,
}

/// Registered OpenAI Codex subscription OAuth behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAICodexOAuthProvider;

#[async_trait::async_trait]
impl OAuthProvider for OpenAICodexOAuthProvider {
    fn id(&self) -> &str {
        OPENAI_CODEX_OAUTH_PROVIDER
    }

    fn name(&self) -> &str {
        "OpenAI Codex (ChatGPT subscription)"
    }

    fn flow_kinds(&self) -> &[OAuthFlowKind] {
        &OPENAI_CODEX_FLOW_KINDS
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
        refresh_openai_codex_token(&credential.refresh, client, now_ms).await
    }

    fn runtime_auth(&self, credential: &OAuthCredential) -> OAuthRuntimeAuth {
        OAuthRuntimeAuth {
            api_key: credential.access.clone(),
            base_url: None,
            headers: None,
        }
    }
}

/// Return a PKCE verifier and its S256 challenge (tau `create_pkce_pair`).
///
/// The verifier is 64 random bytes, base64url-encoded (no padding); the
/// challenge is `base64url(sha256(verifier))`.
///
/// # Errors
/// Returns [`OAuthError`] if secure randomness is unavailable (tau parity: abort
/// rather than emit a predictable verifier).
pub fn create_pkce_pair() -> Result<(String, String), OAuthError> {
    let mut bytes = [0u8; 64];
    fill_random(&mut bytes)?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    Ok((verifier, challenge))
}

/// Create an OpenAI Codex OAuth authorization URL (tau
/// `create_openai_codex_authorization_flow`).
///
/// # Errors
/// Returns [`OAuthError`] if secure randomness is unavailable.
pub fn create_openai_codex_authorization_flow(
    originator: &str,
) -> Result<AuthorizationFlow, OAuthError> {
    let (verifier, challenge) = create_pkce_pair()?;
    let state = token_hex(16)?;
    let params: [(&str, &str); 10] = [
        ("response_type", "code"),
        ("client_id", OPENAI_CODEX_CLIENT_ID),
        ("redirect_uri", OPENAI_CODEX_REDIRECT_URI),
        ("scope", OPENAI_CODEX_SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", originator),
    ];
    let url = format!("{OPENAI_CODEX_AUTHORIZE_URL}?{}", urlencode_form(&params));
    Ok(AuthorizationFlow {
        verifier,
        state,
        url,
    })
}

/// Parse a pasted redirect URL, query string, `code#state` pair, or raw code
/// (tau `parse_authorization_input`).
#[must_use]
pub fn parse_authorization_input(value: &str) -> AuthorizationCode {
    let stripped = value.trim();
    if stripped.is_empty() {
        return AuthorizationCode::default();
    }

    if let Some((_scheme, rest)) = split_url_scheme(stripped) {
        // A full URL: take the query component.
        let query = rest
            .split_once('?')
            .map_or("", |(_, query)| query.split(['#']).next().unwrap_or(""));
        let params = parse_qs(query);
        return AuthorizationCode {
            code: first_query_value(&params, "code"),
            state: first_query_value(&params, "state"),
        };
    }

    if let Some((code, state)) = stripped.split_once('#') {
        return AuthorizationCode {
            code: non_empty(code),
            state: non_empty(state),
        };
    }

    if stripped.contains("code=") {
        let params = parse_qs(stripped);
        return AuthorizationCode {
            code: first_query_value(&params, "code"),
            state: first_query_value(&params, "state"),
        };
    }

    AuthorizationCode {
        code: Some(stripped.to_string()),
        state: None,
    }
}

/// Return whether an OAuth credential should be refreshed before use (tau
/// `oauth_credential_is_expired`). `now_ms` is the current time in integer
/// milliseconds (e.g. `clock.now_ms()`).
#[must_use]
pub fn oauth_credential_is_expired(credential: &OAuthCredential, now_ms: i64) -> bool {
    now_ms >= credential.expires - TOKEN_REFRESH_SKEW_MS
}

/// Exchange an OpenAI Codex authorization code for OAuth tokens (tau
/// `exchange_openai_codex_authorization_code`).
pub async fn exchange_openai_codex_authorization_code(
    code: &str,
    verifier: &str,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
) -> Result<TokenResponse, OAuthError> {
    let raw = post_openai_codex_token(
        client,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", OPENAI_CODEX_REDIRECT_URI),
        ],
        "exchange",
    )
    .await?;
    let access = required_token_field(&raw, "access_token", "exchange")?;
    let refresh = required_token_field(&raw, "refresh_token", "exchange")?;
    let expires = token_expiry(&raw, &access, "exchange", now_ms)?;
    Ok(TokenResponse {
        access,
        refresh,
        expires,
    })
}

/// Refresh OpenAI Codex OAuth credentials (tau `refresh_openai_codex_token`).
pub async fn refresh_openai_codex_token(
    refresh_token: &str,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
) -> Result<OAuthCredential, OAuthError> {
    let raw = post_openai_codex_token(
        client,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
            ("refresh_token", refresh_token),
        ],
        "refresh",
    )
    .await?;
    let access = required_token_field(&raw, "access_token", "refresh")?;
    let next_refresh =
        optional_token_field(&raw, "refresh_token").unwrap_or_else(|| refresh_token.to_string());
    let account_id = account_id_from_access_token(&access).ok_or_else(|| {
        OAuthError("Failed to extract OpenAI account id from refreshed access token".to_string())
    })?;
    let expires = token_expiry(&raw, &access, "refresh", now_ms)?;
    Ok(OAuthCredential {
        access,
        refresh: next_refresh,
        expires,
        account_id: Some(account_id),
        metadata: rho_agent::types::JsonMap::new(),
    })
}

/// Extract the ChatGPT account id from an OpenAI Codex access JWT (tau
/// `account_id_from_access_token`).
#[must_use]
pub fn account_id_from_access_token(access_token: &str) -> Option<String> {
    let payload = access_token_payload(access_token)?;
    let auth = payload.get(OPENAI_CODEX_ACCOUNT_CLAIM)?;
    let account_id = auth.get("chatgpt_account_id")?.as_str()?;
    if account_id.trim().is_empty() {
        None
    } else {
        Some(account_id.trim().to_string())
    }
}

fn access_token_expiry(access_token: &str) -> Option<i64> {
    let payload = access_token_payload(access_token)?;
    let exp = payload.get("exp")?.as_number()?;
    if let Some(int_exp) = exp.as_i64() {
        if int_exp > 0 {
            Some(int_exp.saturating_mul(1000))
        } else {
            None
        }
    } else {
        let float_exp = exp.as_f64()?;
        if float_exp > 0.0 {
            Some((float_exp * 1000.0) as i64)
        } else {
            None
        }
    }
}

fn access_token_payload(access_token: &str) -> Option<serde_json::Map<String, Value>> {
    let parts: Vec<&str> = access_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = base64url_decode(parts[1]).ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

async fn post_openai_codex_token(
    client: &dyn OAuthHttpClient,
    data: &[(&str, &str)],
    action: &str,
) -> Result<serde_json::Map<String, Value>, OAuthError> {
    let response = client
        .send(OAuthHttpRequest::post_form(
            OPENAI_CODEX_TOKEN_URL,
            data,
            &[],
        ))
        .await?;
    if response.is_error() {
        // Status only — never surface the response body (it can echo the
        // submitted token), matching the Anthropic path's deliberate redaction.
        return Err(OAuthError(format!(
            "OpenAI Codex token {action} failed ({})",
            response.status
        )));
    }
    match response.json_value() {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(OAuthError(format!(
            "OpenAI Codex token {action} response must be a JSON object"
        ))),
    }
}

fn required_token_field(
    raw: &serde_json::Map<String, Value>,
    field: &str,
    action: &str,
) -> Result<String, OAuthError> {
    match raw.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        // Field name only — the raw map holds access/refresh tokens; never dump it.
        _ => Err(OAuthError(format!(
            "OpenAI Codex token {action} response missing {field}"
        ))),
    }
}

fn optional_token_field(raw: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    match raw.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

fn token_expiry(
    raw: &serde_json::Map<String, Value>,
    access_token: &str,
    action: &str,
    now_ms: i64,
) -> Result<i64, OAuthError> {
    match raw.get("expires_in") {
        Some(Value::Number(number)) => {
            let millis = if let Some(int_value) = number.as_i64() {
                int_value.saturating_mul(1000)
            } else if let Some(float_value) = number.as_f64() {
                (float_value * 1000.0) as i64
            } else {
                0
            };
            Ok(now_ms.saturating_add(millis))
        }
        Some(Value::Null) | None => access_token_expiry(access_token).ok_or_else(|| {
            // No raw-map dump: it holds the access/refresh tokens.
            OAuthError(format!(
                "OpenAI Codex token {action} response missing expiry"
            ))
        }),
        Some(_) => Err(OAuthError(format!(
            "OpenAI Codex token {action} response has invalid expires_in"
        ))),
    }
}

/// Base64url-decode a value, adding padding as tau's `_base64url_decode` does.
pub(crate) fn base64url_decode(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(value.trim_end_matches('='))
}

/// URL-encode form key/value pairs like Python's `urllib.parse.urlencode`
/// (`quote_plus`: unreserved kept, space → `+`, else `%XX`).
#[must_use]
pub fn urlencode_form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        quote_plus_into(key, &mut out);
        out.push('=');
        quote_plus_into(value, &mut out);
    }
    out
}

fn quote_plus_into(value: &str, out: &mut String) {
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
}

/// Parse a query string like Python's `urllib.parse.parse_qs` (default
/// `keep_blank_values=False`, `&`-separated; `+` and `%XX` decoded).
fn parse_qs(query: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let (key, value) = match field.split_once('=') {
            Some((key, value)) => (percent_decode_plus(key), percent_decode_plus(value)),
            None => (percent_decode_plus(field), String::new()),
        };
        if value.is_empty() {
            continue; // keep_blank_values=False
        }
        pairs.push((key, value));
    }
    pairs
}

fn first_query_value(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| non_empty(value))
}

fn percent_decode_plus(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                let decoded = bytes
                    .get(index + 1)
                    .and_then(|b| hex_value(*b))
                    .zip(bytes.get(index + 2).and_then(|b| hex_value(*b)));
                if let Some((hi, lo)) = decoded {
                    out.push((hi << 4) | lo);
                    index += 3;
                } else {
                    out.push(b'%');
                    index += 1;
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Split `scheme://rest` when both a scheme and a non-empty authority are
/// present (the condition tau's `urlparse(...).scheme and .netloc` requires).
fn split_url_scheme(value: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = value.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return None;
    }
    // netloc is the part up to the first '/', '?' or '#'; require it non-empty.
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if netloc_end == 0 {
        return None;
    }
    Some((scheme, rest))
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn fill_random(buffer: &mut [u8]) -> Result<(), OAuthError> {
    // Match tau: PKCE verifiers / CSRF state come from `secrets` (→ `os.urandom`),
    // which *raises* when no entropy source exists — it never falls back to
    // predictable bytes. A deterministic fallback here would emit a guessable
    // verifier + state precisely when entropy fails, so abort instead.
    getrandom::getrandom(buffer)
        .map_err(|err| OAuthError(format!("secure randomness unavailable: {err}")))
}

fn token_hex(byte_count: usize) -> Result<String, OAuthError> {
    let mut bytes = vec![0u8; byte_count];
    fill_random(&mut bytes)?;
    let mut out = String::with_capacity(byte_count * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Interactive browser flow (manual only — see dev-notes/oauth-manual-checklist.md)
// ---------------------------------------------------------------------------

/// A running local OAuth callback server (tau `_LocalOAuthServer`).
///
/// Accepts one browser redirect on `127.0.0.1:<port>`, validates `state`, and
/// yields the authorization code. **Interactive; not unit-tested.**
pub struct LocalOAuthServer {
    stop: Arc<AtomicBool>,
    code_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Option<String>>>>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LocalOAuthServer {
    /// Wait for the callback server to receive an authorization code.
    pub async fn wait_for_code(&self) -> Option<String> {
        let receiver = self.code_rx.lock().await.take();
        match receiver {
            Some(receiver) => receiver.await.ok().flatten(),
            None => None,
        }
    }

    /// Resolve the pending wait without an authorization code.
    pub fn cancel_wait(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stop the local callback server.
    pub fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Bind a nonblocking loopback listener with `SO_REUSEADDR` set (on Unix).
///
/// Mirrors tau's `ThreadingHTTPServer` (Python's `HTTPServer` sets
/// `allow_reuse_address = 1`): on Unix, `SO_REUSEADDR` lets a fixed callback port
/// be rebound while a prior connection lingers in `TIME_WAIT`, which is exactly
/// the rapid-retry case that otherwise fails on the fixed OAuth ports.
///
/// `SO_REUSEADDR` is **not** set on Windows: there its semantics let a *different*
/// process bind an already-listening address+port and race for the browser
/// redirect. That is a callback-hijack risk here — the Anthropic flow uses the
/// PKCE verifier as `state`, so a stolen redirect leaks both the code and the
/// verifier. Windows' default (exclusive) ownership is what we want, and the
/// `TIME_WAIT` rebind problem this fixes is a Unix concern.
///
/// Like `std::net::TcpListener::bind`, every address `host:port` resolves to is
/// tried in turn; the first that binds wins, otherwise the last error propagates.
fn bind_reuse_addr(host: &str, port: u16) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let mut last_err: Option<std::io::Error> = None;
    for addr in (host, port).to_socket_addrs()? {
        let bind_one = || -> std::io::Result<TcpListener> {
            let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
            // Unix only — see the doc comment for the Windows hijack rationale.
            #[cfg(not(windows))]
            socket.set_reuse_address(true)?;
            socket.bind(&addr.into())?;
            socket.listen(128)?;
            let listener: TcpListener = socket.into();
            listener.set_nonblocking(true)?;
            Ok(listener)
        };
        match bind_one() {
            Ok(listener) => return Ok(listener),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!("no socket address resolved for {host}:{port}"),
        )
    }))
}

/// Start the local OAuth callback server (tau `_start_local_oauth_server`).
///
/// Returns `None` if the port cannot be bound (mirroring tau's `except OSError`).
/// **Interactive; not unit-tested.**
#[must_use]
pub fn start_local_oauth_server(
    state: &str,
    callback_port: u16,
    callback_path: &str,
    success_message: &str,
) -> Option<LocalOAuthServer> {
    let host = std::env::var("TAU_OAUTH_CALLBACK_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let listener = match bind_reuse_addr(host.as_str(), callback_port) {
        Ok(listener) => listener,
        Err(error) => {
            // tau's `ThreadingHTTPServer` sets `allow_reuse_address = 1`; without
            // SO_REUSEADDR the fixed callback port lingers in TIME_WAIT after a
            // prior `/login`, so a rapid retry fails to bind. Don't swallow it
            // silently (that drops the user to manual paste with no warning).
            eprintln!(
                "warning: OAuth loopback server could not bind {host}:{callback_port} ({error}); falling back to manual code paste"
            );
            return None;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let thread_stop = stop.clone();
    let state = state.to_string();
    let callback_path = callback_path.to_string();
    let success_message = success_message.to_string();

    let handle = std::thread::spawn(move || {
        let mut sender = Some(tx);
        loop {
            if thread_stop.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let code =
                        handle_callback(&mut stream, &state, &callback_path, &success_message);
                    if let Some(code) = code {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(Some(code));
                        }
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        if let Some(sender) = sender.take() {
            let _ = sender.send(None);
        }
    });

    Some(LocalOAuthServer {
        stop,
        code_rx: tokio::sync::Mutex::new(Some(rx)),
        thread: std::sync::Mutex::new(Some(handle)),
    })
}

fn handle_callback(
    stream: &mut std::net::TcpStream,
    state: &str,
    callback_path: &str,
    success_message: &str,
) -> Option<String> {
    let mut buffer = [0u8; 4096];
    let read = stream.read(&mut buffer).ok()?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let request_line = request.lines().next().unwrap_or("");
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };

    if path != callback_path {
        write_http_response(stream, 404, &oauth_html("Callback route not found."));
        return None;
    }
    let params = parse_qs(query);
    if first_query_value(&params, "state").as_deref() != Some(state) {
        write_http_response(stream, 400, &oauth_html("OAuth state mismatch."));
        return None;
    }
    let Some(code) = first_query_value(&params, "code") else {
        write_http_response(stream, 400, &oauth_html("Missing authorization code."));
        return None;
    };
    write_http_response(stream, 200, &oauth_html(success_message));
    Some(code)
}

fn write_http_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let encoded = body.as_bytes();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        encoded.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(encoded);
    let _ = stream.flush();
}

fn oauth_html(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!("<!doctype html><meta charset=\"utf-8\"><title>Rho OAuth</title><p>{escaped}</p>")
}

/// Run OpenAI Codex OAuth and return refreshable credentials (tau
/// `login_openai_codex`). **Interactive; not unit-tested — see the manual
/// checklist.**
#[allow(clippy::too_many_arguments)]
pub async fn login_openai_codex(
    callbacks: &OAuthLoginCallbacks,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
    open_browser: bool,
    originator: &str,
) -> Result<OAuthCredential, OAuthError> {
    let flow = create_openai_codex_authorization_flow(originator)?;
    let server = start_local_oauth_server(
        &flow.state,
        OPENAI_CODEX_CALLBACK_PORT,
        "/auth/callback",
        "OpenAI authentication completed. You can close this window.",
    );

    (callbacks.on_auth)(OAuthAuthInfo {
        url: flow.url.clone(),
        instructions: Some("A browser window should open. Complete login to finish.".to_string()),
    });
    if open_browser {
        open_url(&flow.url);
    }

    let result = login_openai_codex_inner(callbacks, client, now_ms, &flow, server.as_ref()).await;
    if let Some(server) = server.as_ref() {
        server.close();
    }
    result
}

async fn login_openai_codex_inner(
    callbacks: &OAuthLoginCallbacks,
    client: &dyn OAuthHttpClient,
    now_ms: i64,
    flow: &AuthorizationFlow,
    server: Option<&LocalOAuthServer>,
) -> Result<OAuthCredential, OAuthError> {
    let mut code = wait_for_authorization_code(flow, server, callbacks).await?;
    if code.is_none() {
        let manual = (callbacks.on_prompt)(OAuthPrompt::new(
            "Paste the authorization code or full redirect URL:",
        ))
        .await?;
        let parsed = parse_authorization_input(&manual);
        validate_state(parsed.state.as_deref(), &flow.state)?;
        code = parsed.code;
    }
    let Some(code) = code.filter(|value| !value.is_empty()) else {
        return Err(OAuthError("Missing authorization code".to_string()));
    };
    if let Some(progress) = &callbacks.on_progress {
        progress("Exchanging authorization code...");
    }
    let token =
        exchange_openai_codex_authorization_code(&code, &flow.verifier, client, now_ms).await?;
    let account_id = account_id_from_access_token(&token.access).ok_or_else(|| {
        OAuthError("Failed to extract OpenAI account id from access token".to_string())
    })?;
    Ok(OAuthCredential {
        access: token.access,
        refresh: token.refresh,
        expires: token.expires,
        account_id: Some(account_id),
        metadata: rho_agent::types::JsonMap::new(),
    })
}

async fn wait_for_authorization_code(
    flow: &AuthorizationFlow,
    server: Option<&LocalOAuthServer>,
    callbacks: &OAuthLoginCallbacks,
) -> Result<Option<String>, OAuthError> {
    let raw = match (server, callbacks.on_manual_code_input.as_ref()) {
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
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed = parse_authorization_input(&raw);
    validate_state(parsed.state.as_deref(), &flow.state)?;
    Ok(parsed.code)
}

fn validate_state(state: Option<&str>, expected_state: &str) -> Result<(), OAuthError> {
    match state {
        Some(state) if state != expected_state => {
            Err(OAuthError("OAuth state mismatch".to_string()))
        }
        _ => Ok(()),
    }
}

/// Best-effort open of a URL in the user's browser (tau `webbrowser.open`).
pub(crate) fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", url]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let command = ("xdg-open", vec![url]);
    let _ = std::process::Command::new(command.0)
        .args(command.1)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_http::{MockHttpClient, OAuthHttpResponse};

    fn base64url(value: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(value)
    }

    fn jwt(account_id: &str, expires: Option<i64>) -> String {
        let mut payload = serde_json::Map::new();
        payload.insert(
            OPENAI_CODEX_ACCOUNT_CLAIM.to_string(),
            serde_json::json!({ "chatgpt_account_id": account_id }),
        );
        if let Some(expires) = expires {
            payload.insert("exp".to_string(), Value::from(expires));
        }
        let header = base64url(br#"{"alg":"none"}"#);
        let body = base64url(&serde_json::to_vec(&Value::Object(payload)).unwrap());
        format!("{header}.{body}.signature")
    }

    #[test]
    fn authorization_flow_includes_pkce_and_codex_params() {
        let flow = create_openai_codex_authorization_flow("tau-test").unwrap();
        assert!(
            flow.url
                .starts_with("https://auth.openai.com/oauth/authorize?")
        );
        let query = flow.url.split_once('?').unwrap().1;
        let params = parse_qs(query);
        let get = |key: &str| first_query_value(&params, key);
        assert_eq!(get("response_type").as_deref(), Some("code"));
        assert_eq!(get("client_id").as_deref(), Some(OPENAI_CODEX_CLIENT_ID));
        assert_eq!(
            get("redirect_uri").as_deref(),
            Some("http://localhost:1455/auth/callback")
        );
        assert_eq!(
            get("scope").as_deref(),
            Some("openid profile email offline_access")
        );
        assert_eq!(get("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(get("codex_cli_simplified_flow").as_deref(), Some("true"));
        assert_eq!(get("originator").as_deref(), Some("tau-test"));
        assert_eq!(get("state").as_deref(), Some(flow.state.as_str()));
        assert!(get("code_challenge").is_some());
        assert!(!flow.verifier.is_empty());
    }

    #[test]
    fn token_field_errors_never_leak_secrets() {
        // A token response that is missing a required field but *does* carry the
        // access/refresh tokens: the error must name the missing field only,
        // never serialize the map (which would surface the tokens in the
        // transcript). Matches the Anthropic path's status-only redaction.
        let mut raw = serde_json::Map::new();
        raw.insert(
            "access_token".to_string(),
            Value::String("SECRET-ACCESS".into()),
        );
        raw.insert(
            "refresh_token".to_string(),
            Value::String("SECRET-REFRESH".into()),
        );

        let err = required_token_field(&raw, "token_type", "exchange").unwrap_err();
        assert!(
            !err.0.contains("SECRET-ACCESS"),
            "leaked access token: {}",
            err.0
        );
        assert!(
            !err.0.contains("SECRET-REFRESH"),
            "leaked refresh token: {}",
            err.0
        );
        assert!(err.0.contains("missing token_type"));

        // The expiry-parse error path (no `expires_in`, non-JWT access token)
        // must likewise not echo the token map.
        let err = token_expiry(&raw, "not-a-jwt", "exchange", 0).unwrap_err();
        assert!(
            !err.0.contains("SECRET-ACCESS"),
            "leaked token in expiry error: {}",
            err.0
        );
        assert!(err.0.contains("missing expiry"));
    }

    #[test]
    fn validate_state_rejects_only_present_mismatched_state() {
        // Matching state passes (the normal CSRF-protected callback).
        assert!(validate_state(Some("state-1"), "state-1").is_ok());
        // A present but mismatched state is rejected (the CSRF guard fires).
        let err = validate_state(Some("attacker"), "state-1").unwrap_err();
        assert_eq!(err.0, "OAuth state mismatch");
        // An absent state is accepted — this faithfully matches tau's
        // `_validate_state` (`state is not None and state != expected`), which
        // some providers rely on; do not tighten without a tau change.
        assert!(validate_state(None, "state-1").is_ok());
    }

    #[test]
    fn parse_authorization_input_accepts_url_query_and_raw_code() {
        assert_eq!(
            parse_authorization_input("http://localhost:1455/auth/callback?code=abc&state=state-1")
                .code
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            parse_authorization_input("code=abc&state=state-1")
                .state
                .as_deref(),
            Some("state-1")
        );
        assert_eq!(
            parse_authorization_input("abc#state-1").state.as_deref(),
            Some("state-1")
        );
        assert_eq!(
            parse_authorization_input("abc").code.as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn account_id_from_access_token_reads_openai_auth_claim() {
        assert_eq!(
            account_id_from_access_token(&jwt("account-1", None)).as_deref(),
            Some("account-1")
        );
        assert_eq!(account_id_from_access_token("not-a-jwt"), None);
    }

    #[tokio::test]
    async fn refresh_openai_codex_token_returns_oauth_credential() {
        let access = jwt("account-2", None);
        let response_access = access.clone();
        let client = MockHttpClient::new(move |request| {
            let body = String::from_utf8_lossy(&request.body);
            assert!(body.contains("grant_type=refresh_token"));
            assert!(body.contains("client_id="));
            OAuthHttpResponse::json(
                200,
                &serde_json::json!({
                    "access_token": response_access,
                    "refresh_token": "new-refresh",
                    "expires_in": 3600,
                }),
            )
        });
        let now_ms = 1_700_000_000_123;
        let credential = refresh_openai_codex_token("old-refresh", &client, now_ms)
            .await
            .unwrap();
        assert_eq!(credential.access, access);
        assert_eq!(credential.refresh, "new-refresh");
        assert_eq!(credential.account_id.as_deref(), Some("account-2"));
        assert!(credential.expires > 0);
    }

    #[tokio::test]
    async fn refresh_openai_codex_token_preserves_refresh_and_reads_jwt_expiry() {
        let expires = 1_700_003_600; // seconds
        let access = jwt("account-3", Some(expires));
        let response_access = access.clone();
        let client = MockHttpClient::new(move |request| {
            let body = String::from_utf8_lossy(&request.body);
            assert!(body.contains("grant_type=refresh_token"));
            OAuthHttpResponse::json(200, &serde_json::json!({ "access_token": response_access }))
        });
        let credential = refresh_openai_codex_token("old-refresh", &client, 0)
            .await
            .unwrap();
        assert_eq!(credential.access, access);
        assert_eq!(credential.refresh, "old-refresh");
        assert_eq!(credential.account_id.as_deref(), Some("account-3"));
        assert_eq!(credential.expires, expires * 1000);
    }

    #[test]
    fn urlencode_form_matches_quote_plus() {
        assert_eq!(
            urlencode_form(&[("scope", "openid profile"), ("uri", "http://x/y")]),
            "scope=openid+profile&uri=http%3A%2F%2Fx%2Fy"
        );
    }

    #[test]
    fn loopback_server_rebinds_same_port_with_reuse_addr() {
        // Without SO_REUSEADDR the fixed callback port lingers in TIME_WAIT after
        // the first server closes, and this immediate rebind on the SAME port
        // fails — the exact rapid-`/login`-retry defect this fixes. With
        // SO_REUSEADDR both binds succeed.
        const PORT: u16 = 54999;

        let first = start_local_oauth_server("state-1", PORT, "/auth/callback", "ok");
        assert!(first.is_some(), "first bind on port {PORT} should succeed");
        first.unwrap().close();

        let second = start_local_oauth_server("state-2", PORT, "/auth/callback", "ok");
        assert!(
            second.is_some(),
            "immediate rebind on the same port {PORT} should succeed with SO_REUSEADDR"
        );
        second.unwrap().close();
    }

    #[test]
    fn is_expired_uses_skew() {
        let credential = OAuthCredential::new("a", "r", 1_000_000);
        // Exactly at (expires - skew) counts as expired.
        assert!(oauth_credential_is_expired(
            &credential,
            1_000_000 - TOKEN_REFRESH_SKEW_MS
        ));
        assert!(!oauth_credential_is_expired(
            &credential,
            1_000_000 - TOKEN_REFRESH_SKEW_MS - 1
        ));
    }
}
