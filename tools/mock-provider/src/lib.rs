//! A tiny axum server that replays a recorded provider SSE body byte-for-byte.
//!
//! It answers **any** `POST` path with a configured status + body, so it can
//! stand in for every provider endpoint shape (`/v1/messages`,
//! `/chat/completions`, `/v1/responses`, `/codex/responses`,
//! `/models/…:streamGenerateContent`, …) — the adapters only care that the bytes
//! come back. The body can be chunked with a configurable size and per-chunk
//! latency (to exercise the adapters' incremental line splitting and, in M6, to
//! model realistic streaming). Every request body is captured so tests can assert
//! the request payload byte-for-byte.
//!
//! This crate has **no** dependency on `rho-ai`/`rho-agent`: it is pure bytes in,
//! bytes out, so the golden tests can depend on it without a cycle.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::any;
use bytes::Bytes;

/// How the server replays one canned response.
#[derive(Clone)]
pub struct MockConfig {
    /// HTTP status to return.
    pub status: u16,
    /// The exact response body bytes (an SSE transcript).
    pub body: Vec<u8>,
    /// If set, split the body into chunks of at most this many bytes.
    pub chunk_size: Option<usize>,
    /// Delay before each chunk.
    pub latency: Duration,
    /// `content-type` header to send.
    pub content_type: String,
}

impl MockConfig {
    /// A 200 SSE response of `body`, unchunked, no latency.
    #[must_use]
    pub fn sse(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            chunk_size: None,
            latency: Duration::ZERO,
            content_type: "text/event-stream".to_string(),
        }
    }

    /// Set the HTTP status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Set the per-chunk size (bytes).
    #[must_use]
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = Some(chunk_size);
        self
    }

    /// Set the per-chunk latency.
    #[must_use]
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }
}

/// Chooses the response for one request.
#[derive(Clone)]
enum Responder {
    /// A fixed config (chunkable, with latency).
    Static(Arc<MockConfig>),
    /// A per-request `(status, body)` selector (e.g. attempt-dependent).
    Dynamic(Arc<dyn Fn() -> (u16, Vec<u8>) + Send + Sync>),
}

#[derive(Clone)]
struct AppState {
    responder: Responder,
    captured: Arc<Mutex<Vec<Vec<u8>>>>,
}

/// A running mock server; drop [`MockServer::shutdown`] or let it fall out of
/// scope to stop it.
pub struct MockServer {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<Vec<u8>>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

/// Build the router (and its request-capture handle) for a responder.
fn router(responder: Responder) -> (Router, Arc<Mutex<Vec<Vec<u8>>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = AppState {
        responder,
        captured: captured.clone(),
    };
    let app = Router::new()
        .fallback(any(handle_request))
        .with_state(state);
    (app, captured)
}

/// Serve `config` on an already-bound `listener` until the process exits (the
/// CLI entrypoint).
pub async fn serve(listener: tokio::net::TcpListener, config: MockConfig) {
    let (app, _captured) = router(Responder::Static(Arc::new(config)));
    let _ = axum::serve(listener, app).await;
}

impl MockServer {
    /// Bind to an ephemeral localhost port and start serving `config`.
    pub async fn spawn(config: MockConfig) -> Self {
        Self::spawn_responder(Responder::Static(Arc::new(config))).await
    }

    /// Bind to an ephemeral localhost port and answer each request with a
    /// dynamically selected `(status, body)` — e.g. attempt-dependent for retry
    /// tests. The body is sent unchunked with no latency.
    pub async fn spawn_with(selector: impl Fn() -> (u16, Vec<u8>) + Send + Sync + 'static) -> Self {
        Self::spawn_responder(Responder::Dynamic(Arc::new(selector))).await
    }

    async fn spawn_responder(responder: Responder) -> Self {
        let (app, captured) = router(responder);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            let _ = server.await;
        });
        Self {
            addr,
            captured,
            shutdown: Some(tx),
            handle,
        }
    }

    /// The base URL (`http://127.0.0.1:PORT`) to point an adapter at.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The request bodies received so far, in order.
    #[must_use]
    pub fn captured_requests(&self) -> Vec<Vec<u8>> {
        self.captured.lock().expect("captured lock").clone()
    }

    /// Stop the server.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }
}

async fn handle_request(
    State(state): State<AppState>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    state
        .captured
        .lock()
        .expect("captured lock")
        .push(body.to_vec());

    let (status, chunks, latency, content_type) = match &state.responder {
        Responder::Static(config) => (
            config.status,
            split_chunks(&config.body, config.chunk_size),
            config.latency,
            config.content_type.clone(),
        ),
        Responder::Dynamic(selector) => {
            let (status, body) = selector();
            (
                status,
                vec![body],
                Duration::ZERO,
                "text/event-stream".to_string(),
            )
        }
    };

    let stream = async_stream::stream! {
        for chunk in chunks {
            if !latency.is_zero() {
                tokio::time::sleep(latency).await;
            }
            yield Ok::<Bytes, std::convert::Infallible>(Bytes::from(chunk));
        }
    };

    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header("content-type", content_type)
        .body(Body::from_stream(stream))
        .expect("build response")
}

fn split_chunks(body: &[u8], chunk_size: Option<usize>) -> Vec<Vec<u8>> {
    match chunk_size {
        Some(size) if size > 0 && !body.is_empty() => {
            body.chunks(size).map(<[u8]>::to_vec).collect()
        }
        _ => vec![body.to_vec()],
    }
}
