//! HTTP client construction shared by provider adapters (tau `tau_ai/http.py`).
//!
//! ## SOCKS normalization
//!
//! tau normalizes proxy *environment variables* before constructing an
//! `httpx.AsyncClient` (`normalized_proxy_environment`), rewriting the generic
//! `socks://` scheme — which httpx rejects — to `socks5://`. rho reaches the same
//! end differently: reqwest takes proxies through its builder, so
//! [`create_client`] reads the same proxy env vars, applies
//! [`normalize_proxy_url`], and installs explicit [`reqwest::Proxy`] entries
//! (SOCKS support comes from reqwest's `socks` feature). The pure
//! [`normalize_proxy_url`] is a direct port of tau's helper (and its tests).
//!
//! ## TLS
//!
//! reqwest is built with `rustls-tls` (no OpenSSL), matching the workspace's
//! dependency pin; there is no per-request TLS state to reproduce.

use std::time::Duration;

/// Proxy env vars tau normalizes, in its order (tau `_PROXY_ENV_VARS`).
const PROXY_ENV_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Return a reqwest-compatible proxy URL (tau `normalize_proxy_url`).
///
/// A generic `socks://` scheme is rewritten to `socks5://`; every explicit
/// scheme is returned unchanged. The `socks://` prefix match is
/// case-insensitive, mirroring tau's `.lower().startswith(...)`.
#[must_use]
pub fn normalize_proxy_url(proxy_url: &str) -> String {
    const GENERIC: &str = "socks://";
    if proxy_url.len() >= GENERIC.len() && proxy_url[..GENERIC.len()].eq_ignore_ascii_case(GENERIC)
    {
        format!("socks5://{}", &proxy_url[GENERIC.len()..])
    } else {
        proxy_url.to_string()
    }
}

/// Build a streaming HTTP client with tau's proxy normalization applied
/// (tau `create_async_client`).
///
/// `timeout_seconds` bounds the whole request; provider adapters stream the
/// response body incrementally within that budget, matching tau's httpx timeout.
pub fn create_client(timeout_seconds: f64) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs_f64(timeout_seconds))
        // Take explicit control of proxies (see below) instead of reqwest's
        // auto-detection, so the generic `socks://` scheme is normalized first.
        .no_proxy();
    for proxy in env_proxies() {
        builder = builder.proxy(proxy);
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Read the proxy env vars tau honors, normalize them, and map each to a
/// [`reqwest::Proxy`]. Unparseable values are skipped (best effort, as httpx
/// would simply fail to route them).
fn env_proxies() -> Vec<reqwest::Proxy> {
    let mut proxies = Vec::new();
    for name in PROXY_ENV_VARS {
        let Ok(raw) = std::env::var(name) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let url = normalize_proxy_url(&raw);
        let built = match name {
            "HTTP_PROXY" | "http_proxy" => reqwest::Proxy::http(&url),
            "HTTPS_PROXY" | "https_proxy" => reqwest::Proxy::https(&url),
            _ => reqwest::Proxy::all(&url),
        };
        if let Ok(proxy) = built {
            proxies.push(proxy);
        }
    }
    proxies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_generic_socks_scheme() {
        assert_eq!(
            normalize_proxy_url("socks://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(
            normalize_proxy_url("SOCKS://user:pass@proxy.local:1080"),
            "socks5://user:pass@proxy.local:1080"
        );
    }

    #[test]
    fn leaves_explicit_schemes_unchanged() {
        assert_eq!(
            normalize_proxy_url("socks5://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(
            normalize_proxy_url("socks5h://127.0.0.1:1080"),
            "socks5h://127.0.0.1:1080"
        );
        assert_eq!(
            normalize_proxy_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn builds_a_client() {
        let _client = create_client(1.0);
    }
}
