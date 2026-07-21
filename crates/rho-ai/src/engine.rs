//! The shared streaming envelope: HTTP attempt, retry loop, SSE line splitting,
//! and accumulator driving (tau's per-adapter `_stream(...)` iterator body).
//!
//! Every adapter's non-parsing machinery is identical in tau — the status/network
//! retry ladder, the cancellation checks, the opening `response_start`, the
//! "finalize unless fatal" tail. That lives here once, parameterized by:
//!
//! * an [`AttemptFetcher`] — performs one HTTP attempt (production: reqwest;
//!   tests: canned responses), returning a status + raw byte stream, so the
//!   envelope is exercised with **no sockets** in golden/retry tests;
//! * a [`ProviderParser`] factory — the per-endpoint SSE handler;
//! * an `is_retryable_status` predicate — the standard transient set for most
//!   providers, body-aware for Codex's terminal rate limits.
//!
//! Retries emit **no** canonical events (tau dropped `ProviderRetryEvent` at the
//! Pi boundary), so the only observable effect of a retry is another attempt.

use std::sync::Arc;

use bytes::Bytes;
use futures::stream::{BoxStream, Stream, StreamExt};
use rho_agent::provider::CancellationToken;
use rho_agent::provider_events::AssistantMessageEvent;

use crate::http_errors::provider_http_error_message;
use crate::retry::{retry_delay_seconds, wait_for_retry};
use crate::stream::{Delta, StreamAccumulator};
use crate::types::JsonMap;

/// One SSE `feed` result (tau's `(events, stop)` tuple), as [`Delta`]s.
#[derive(Debug, Default)]
pub struct Feed {
    /// Deltas produced by this line.
    pub deltas: Vec<Delta>,
    /// Whether the adapter should stop reading (a `[DONE]`/terminal event).
    pub stop: bool,
}

impl Feed {
    /// A feed that produced nothing and does not stop.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A feed carrying deltas and not stopping.
    #[must_use]
    pub fn deltas(deltas: Vec<Delta>) -> Self {
        Self {
            deltas,
            stop: false,
        }
    }

    /// A feed carrying deltas that stops the stream.
    #[must_use]
    pub fn stop(deltas: Vec<Delta>) -> Self {
        Self { deltas, stop: true }
    }
}

/// A per-endpoint SSE handler (tau's `_StreamParser` protocol).
pub trait ProviderParser: Send {
    /// Consume one SSE line (as yielded by the line splitter, terminator
    /// stripped) and return the deltas + stop flag.
    fn feed_line(&mut self, line: &str) -> Feed;

    /// Return the trailing tool-call and response-end deltas (tau `finalize`).
    fn finalize(&mut self) -> Vec<Delta>;

    /// Whether any model output was emitted (gates mid-stream retry).
    fn emitted_content(&self) -> bool;

    /// Whether a terminal error was already emitted (suppress `finalize`).
    fn fatal(&self) -> bool;
}

impl ProviderParser for Box<dyn ProviderParser> {
    fn feed_line(&mut self, line: &str) -> Feed {
        (**self).feed_line(line)
    }
    fn finalize(&mut self) -> Vec<Delta> {
        (**self).finalize()
    }
    fn emitted_content(&self) -> bool {
        (**self).emitted_content()
    }
    fn fatal(&self) -> bool {
        (**self).fatal()
    }
}

/// Raw response of one HTTP attempt.
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body as byte chunks (`Err` = mid-stream transport failure).
    pub body: BoxStream<'static, Result<Bytes, String>>,
}

/// A pre-response transport failure.
pub struct FetchError {
    /// Human-readable, secret-free message.
    pub message: String,
    /// Whether the failure is retryable (a network error) vs terminal (e.g. a
    /// credential-resolver failure, surfaced immediately like Codex's
    /// `except Exception`).
    pub retryable: bool,
}

/// A fetcher backed by an async closure — the seam every adapter plugs its
/// per-attempt resolver + reqwest call into (tau's `client.stream("POST", ...)`
/// inside the retry loop). `attempt` is 0-based.
pub struct ClosureFetcher<F> {
    f: F,
}

impl<F, Fut> ClosureFetcher<F>
where
    F: FnMut(u32) -> Fut + Send,
    Fut: std::future::Future<Output = Result<HttpResponse, FetchError>> + Send,
{
    /// Wrap an async closure `(attempt) -> Result<HttpResponse, FetchError>`.
    pub fn new(f: F) -> Self {
        Self { f }
    }

    async fn fetch(&mut self, attempt: u32) -> Result<HttpResponse, FetchError> {
        (self.f)(attempt).await
    }
}

/// The retry budget + delay cap for the envelope (tau config fields).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum retry attempts.
    pub max_retries: u32,
    /// Retry delay cap in seconds.
    pub max_retry_delay_seconds: f64,
}

/// Parameters describing the provider for error messages.
#[derive(Debug, Clone)]
pub struct EngineParams {
    /// Human-readable provider name (error prefix).
    pub provider_name: String,
    /// Requested model id (error prefix + accumulator stamp).
    pub model: String,
    /// Retry policy.
    pub policy: RetryPolicy,
}

/// Run the shared streaming envelope, yielding canonical events.
///
/// This owns `acc`, calls `fetcher` per attempt, drives `parser_factory()`'s
/// parser over the SSE lines, and applies the resulting deltas. It reproduces
/// tau's iterator body exactly, including the post-loop
/// `canonicalize`-style finalization (`acc.finish()`).
#[allow(clippy::too_many_lines)]
pub fn run<F, Fut, PF, P, R>(
    mut acc: StreamAccumulator,
    params: EngineParams,
    signal: Option<Arc<dyn CancellationToken>>,
    mut fetcher: ClosureFetcher<F>,
    parser_factory: PF,
    is_retryable_status: R,
) -> impl Stream<Item = AssistantMessageEvent>
where
    F: FnMut(u32) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<HttpResponse, FetchError>> + Send + 'static,
    PF: Fn() -> P + Send + 'static,
    P: ProviderParser + 'static,
    R: Fn(u16, &str) -> bool + Send + 'static,
{
    async_stream::stream! {
        let policy = params.policy;
        let mut attempt: u32 = 0;
        'retry: loop {
            let mut parser = parser_factory();
            match fetcher.fetch(attempt).await {
                Err(fetch_error) => {
                    if fetch_error.retryable
                        && !parser.emitted_content()
                        && attempt < policy.max_retries
                    {
                        let delay = retry_delay_seconds(attempt, policy.max_retry_delay_seconds);
                        attempt += 1;
                        if !wait_for_retry(delay, signal.as_ref()).await {
                            break 'retry;
                        }
                        continue 'retry;
                    }
                    for event in acc.error(fetch_error.message, Some(attempts_map(attempt + 1))) {
                        yield event;
                    }
                    break 'retry;
                }
                Ok(response) => {
                    if response.status >= 400 {
                        let body_text = read_body_text(response.body).await;
                        if attempt < policy.max_retries
                            && is_retryable_status(response.status, &body_text)
                        {
                            let delay =
                                retry_delay_seconds(attempt, policy.max_retry_delay_seconds);
                            attempt += 1;
                            if !wait_for_retry(delay, signal.as_ref()).await {
                                break 'retry;
                            }
                            continue 'retry;
                        }
                        let message = provider_http_error_message(
                            &params.provider_name,
                            response.status,
                            &body_text,
                            Some(&params.model),
                        );
                        for event in
                            acc.error(message, Some(status_error_map(response.status, &body_text, attempt + 1)))
                        {
                            yield event;
                        }
                        break 'retry;
                    }

                    for event in acc.response_start() {
                        yield event;
                    }

                    let mut splitter = LineSplitter::new();
                    let mut body = response.body;
                    let mut transport_error: Option<String> = None;
                    let mut cancelled = false;

                    'lines: loop {
                        if signal.as_ref().is_some_and(|s| s.is_cancelled()) {
                            cancelled = true;
                            break 'lines;
                        }
                        match body.next().await {
                            None => {
                                for line in splitter.flush() {
                                    let feed = parser.feed_line(&line);
                                    for delta in feed.deltas {
                                        for event in acc.apply(delta) {
                                            yield event;
                                        }
                                    }
                                    if feed.stop {
                                        break 'lines;
                                    }
                                }
                                break 'lines;
                            }
                            Some(Err(err)) => {
                                transport_error = Some(err);
                                break 'lines;
                            }
                            Some(Ok(chunk)) => {
                                for line in splitter.push(&chunk) {
                                    let feed = parser.feed_line(&line);
                                    for delta in feed.deltas {
                                        for event in acc.apply(delta) {
                                            yield event;
                                        }
                                    }
                                    if feed.stop {
                                        break 'lines;
                                    }
                                }
                            }
                        }
                    }

                    if cancelled {
                        // tau returns from the generator; canonicalize's post-loop
                        // block then emits the "ended without terminal" error.
                        break 'retry;
                    }

                    if let Some(err) = transport_error {
                        if !parser.emitted_content() && attempt < policy.max_retries {
                            let delay =
                                retry_delay_seconds(attempt, policy.max_retry_delay_seconds);
                            attempt += 1;
                            if !wait_for_retry(delay, signal.as_ref()).await {
                                break 'retry;
                            }
                            continue 'retry;
                        }
                        for event in acc.error(err, Some(attempts_map(attempt + 1))) {
                            yield event;
                        }
                        break 'retry;
                    }

                    if !parser.fatal() {
                        for delta in parser.finalize() {
                            for event in acc.apply(delta) {
                                yield event;
                            }
                        }
                    }
                    break 'retry;
                }
            }
        }

        for event in acc.finish() {
            yield event;
        }
    }
}

/// Assemble the accumulator + engine for one adapter response and box the event
/// stream (the shared tail every adapter's `stream_response` calls).
///
/// `api`/`provider` label the assistant message; `error_provider_name` prefixes
/// HTTP error messages (tau uses `config.provider_name` there, which for
/// Google/Mistral differs from the canonical `provider` label).
#[allow(clippy::too_many_arguments)]
pub fn provider_stream<F, Fut, PF, P, R>(
    api: impl Into<String>,
    provider: impl Into<String>,
    error_provider_name: impl Into<String>,
    model: &str,
    clock: &Arc<dyn rho_agent::clock::Clock>,
    policy: RetryPolicy,
    signal: Option<Arc<dyn CancellationToken>>,
    fetch: F,
    parser_factory: PF,
    is_retryable: R,
) -> rho_agent::provider::AssistantEventStream
where
    F: FnMut(u32) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<HttpResponse, FetchError>> + Send + 'static,
    PF: Fn() -> P + Send + 'static,
    P: ProviderParser + 'static,
    R: Fn(u16, &str) -> bool + Send + 'static,
{
    let acc = StreamAccumulator::new(api, provider, model, clock);
    let params = EngineParams {
        provider_name: error_provider_name.into(),
        model: model.to_string(),
        policy,
    };
    run(
        acc,
        params,
        signal,
        ClosureFetcher::new(fetch),
        parser_factory,
        is_retryable,
    )
    .boxed()
}

/// Build `{"attempts": n}` (tau's network/generic error `data`).
fn attempts_map(attempts: u32) -> JsonMap {
    let mut map = JsonMap::new();
    map.insert("attempts".to_string(), serde_json::json!(attempts));
    map
}

/// Build `{"status_code":…, "body":…, "attempts":…}` in tau's key order
/// (the golden `error.events.jsonl` asserts this exact object).
fn status_error_map(status_code: u16, body: &str, attempts: u32) -> JsonMap {
    let mut map = JsonMap::new();
    map.insert("status_code".to_string(), serde_json::json!(status_code));
    map.insert("body".to_string(), serde_json::json!(body));
    map.insert("attempts".to_string(), serde_json::json!(attempts));
    map
}

/// Read a whole (error) body to a lossy-decoded string (tau
/// `(await response.aread()).decode(errors="replace")`).
async fn read_body_text(mut body: BoxStream<'static, Result<Bytes, String>>) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(bytes) => buf.extend_from_slice(&bytes),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Incremental line splitter matching httpx `aiter_lines` for the terminators
/// real SSE providers emit: split on `\n` (stripping a preceding `\r`, so both
/// `\n` and `\r\n` frame lines), yield each line **without** its terminator, and
/// yield a final unterminated line at EOF. A **lone** `\r` (not followed by `\n`)
/// is *not* treated as a separator — httpx's `str.splitlines` would split it, but
/// no real provider frames SSE with bare `\r`, and byte-level incremental
/// lone-`\r` handling would need to defer a trailing `\r` across chunk boundaries
/// for no practical gain. Buffers across chunk boundaries at the byte level, so a
/// multi-byte UTF-8 character split across TCP reads is decoded only once whole.
struct LineSplitter {
    buffer: Vec<u8>,
}

impl LineSplitter {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=pos).collect();
            line.pop(); // drop '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // drop '\r' of a '\r\n' pair
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }

    fn flush(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let mut line = std::mem::take(&mut self.buffer);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        vec![String::from_utf8_lossy(&line).into_owned()]
    }
}

/// Send one reqwest POST and adapt it to an [`HttpResponse`] (the production
/// [`AttemptFetcher`] body). A send failure maps to a retryable [`FetchError`].
pub async fn send_reqwest(
    client: &reqwest::Client,
    url: &str,
    headers: &crate::types::HeaderList,
    body: &serde_json::Value,
) -> Result<HttpResponse, FetchError> {
    // Apply the header list with dict semantics (last value wins), matching
    // tau's httpx header dicts. `RequestBuilder::header` appends instead, which
    // duplicated `content-type` alongside the one `.json()` sets — the ChatGPT
    // Codex backend rejects that with `400 Unsupported content type`.
    let mut header_map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let name = match reqwest::header::HeaderName::try_from(name.as_str()) {
            Ok(name) => name,
            Err(err) => {
                return Err(FetchError {
                    message: format!("invalid header name {name:?}: {err}"),
                    retryable: false,
                });
            }
        };
        let value = match reqwest::header::HeaderValue::try_from(value.as_str()) {
            Ok(value) => value,
            Err(err) => {
                return Err(FetchError {
                    message: format!("invalid value for header {name:?}: {err}"),
                    retryable: false,
                });
            }
        };
        header_map.insert(name, value);
    }
    let request = client.post(url).json(body).headers(header_map);
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| e.to_string()))
                .boxed();
            Ok(HttpResponse { status, body })
        }
        Err(err) => Err(FetchError {
            message: err.to_string(),
            retryable: true,
        }),
    }
}

/// Build an [`HttpResponse`] from an in-memory body (for the mock/canned path and
/// direct golden injection).
#[must_use]
pub fn canned_response(status: u16, body: impl Into<Vec<u8>>) -> HttpResponse {
    let bytes = Bytes::from(body.into());
    HttpResponse {
        status,
        body: futures::stream::once(async move { Ok(bytes) }).boxed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_matches_aiter_lines() {
        let mut s = LineSplitter::new();
        let mut lines = s.push(b"data: {\"a\":1}\n\ndata: [DONE]\n\n");
        lines.extend(s.flush());
        assert_eq!(
            lines,
            vec![
                "data: {\"a\":1}".to_string(),
                String::new(),
                "data: [DONE]".to_string(),
                String::new(),
            ]
        );
    }

    #[test]
    fn splitter_handles_chunk_boundaries_and_crlf() {
        let mut s = LineSplitter::new();
        let mut lines = s.push(b"da");
        assert!(lines.is_empty());
        lines.extend(s.push(b"ta: x\r\n"));
        assert_eq!(lines, vec!["data: x".to_string()]);
        // Unterminated trailing line is flushed at EOF.
        lines = s.push(b"tail");
        assert!(lines.is_empty());
        assert_eq!(s.flush(), vec!["tail".to_string()]);
    }
}
