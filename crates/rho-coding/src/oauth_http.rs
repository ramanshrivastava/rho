//! HTTP client seam for OAuth token exchange/refresh.
//!
//! tau's OAuth code takes an optional `httpx.AsyncClient` and, in tests, injects
//! an `httpx.MockTransport(handler)` that intercepts requests and returns canned
//! responses without touching the network. reqwest has no equivalent transport
//! hook, so rho abstracts the few request shapes the OAuth flows need behind
//! [`OAuthHttpClient`]: production wires [`ReqwestOAuthClient`] (a thin wrapper
//! over the shared `rho_ai::http` client), while unit tests inject a mock that
//! matches on method/URL/headers/body — the faithful analog of tau's
//! `MockTransport`.

#![allow(clippy::doc_markdown)]

use serde_json::Value;

use crate::oauth::OAuthError;

/// HTTP method used by the OAuth flows (only GET/POST are needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
}

impl HttpMethod {
    /// The uppercase method name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One outbound OAuth HTTP request (method, URL, headers, raw body bytes).
#[derive(Debug, Clone)]
pub struct OAuthHttpRequest {
    /// Request method.
    pub method: HttpMethod,
    /// Absolute request URL.
    pub url: String,
    /// Request headers, in insertion order.
    pub headers: Vec<(String, String)>,
    /// Raw request body bytes (empty for GET).
    pub body: Vec<u8>,
}

impl OAuthHttpRequest {
    /// Build a POST with a URL-encoded form body (`application/x-www-form-urlencoded`).
    #[must_use]
    pub fn post_form(
        url: impl Into<String>,
        form: &[(&str, &str)],
        extra: &[(&str, &str)],
    ) -> Self {
        let mut headers = vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )];
        for (name, value) in extra {
            headers.push(((*name).to_string(), (*value).to_string()));
        }
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers,
            body: crate::oauth::urlencode_form(form).into_bytes(),
        }
    }

    /// Build a POST with a JSON body (`application/json`).
    #[must_use]
    pub fn post_json(url: impl Into<String>, value: &Value, extra: &[(&str, &str)]) -> Self {
        let mut headers = Vec::new();
        for (name, value) in extra {
            headers.push(((*name).to_string(), (*value).to_string()));
        }
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers,
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    /// Build a GET with the given headers.
    #[must_use]
    pub fn get(url: impl Into<String>, headers: &[(&str, &str)]) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            body: Vec::new(),
        }
    }
}

/// One OAuth HTTP response (status + raw body bytes).
#[derive(Debug, Clone)]
pub struct OAuthHttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

impl OAuthHttpResponse {
    /// Build a JSON response with the given status (test convenience).
    #[must_use]
    pub fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    /// Build a text response with the given status (test convenience).
    #[must_use]
    pub fn text_response(status: u16, text: impl Into<String>) -> Self {
        Self {
            status,
            body: text.into().into_bytes(),
        }
    }

    /// The response body decoded as UTF-8 (lossy), mirroring httpx `.text`.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Whether the status is an HTTP error (`>= 400`).
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.status >= 400
    }

    /// Parse the response body as JSON (mirrors httpx `.json()`).
    pub fn json_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

/// Async HTTP client used by the OAuth token flows (tau's injectable
/// `httpx.AsyncClient`).
#[async_trait::async_trait]
pub trait OAuthHttpClient: Send + Sync {
    /// Send one request and return the full response.
    async fn send(&self, request: OAuthHttpRequest) -> Result<OAuthHttpResponse, OAuthError>;
}

/// Production [`OAuthHttpClient`] backed by the shared `rho_ai::http` reqwest
/// client (same proxy normalization as every provider adapter).
#[derive(Clone)]
pub struct ReqwestOAuthClient {
    client: reqwest::Client,
}

impl ReqwestOAuthClient {
    /// Build a client with the given per-read timeout, in seconds.
    #[must_use]
    pub fn new(timeout_seconds: f64) -> Self {
        Self {
            client: rho_ai::http::create_client(timeout_seconds),
        }
    }
}

impl Default for ReqwestOAuthClient {
    fn default() -> Self {
        Self::new(60.0)
    }
}

#[async_trait::async_trait]
impl OAuthHttpClient for ReqwestOAuthClient {
    async fn send(&self, request: OAuthHttpRequest) -> Result<OAuthHttpResponse, OAuthError> {
        let mut builder = match request.method {
            HttpMethod::Get => self.client.get(&request.url),
            HttpMethod::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if request.method == HttpMethod::Post {
            builder = builder.body(request.body);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| OAuthError(error.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|error| OAuthError(error.to_string()))?
            .to_vec();
        Ok(OAuthHttpResponse { status, body })
    }
}

/// A mock [`OAuthHttpClient`] driven by a handler closure — the rho analog of
/// tau's `httpx.MockTransport(handler)`. Records every request for assertions.
#[cfg(test)]
pub(crate) struct MockHttpClient {
    handler: Box<dyn Fn(&OAuthHttpRequest) -> OAuthHttpResponse + Send + Sync>,
    requests: std::sync::Mutex<Vec<OAuthHttpRequest>>,
}

#[cfg(test)]
impl MockHttpClient {
    pub(crate) fn new(
        handler: impl Fn(&OAuthHttpRequest) -> OAuthHttpResponse + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn requests(&self) -> Vec<OAuthHttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl OAuthHttpClient for MockHttpClient {
    async fn send(&self, request: OAuthHttpRequest) -> Result<OAuthHttpResponse, OAuthError> {
        let response = (self.handler)(&request);
        self.requests.lock().unwrap().push(request);
        Ok(response)
    }
}
