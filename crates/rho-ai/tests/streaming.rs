//! Streaming-transport regression tests over the real reqwest path + the
//! in-process mock server: the per-read (not total) timeout, and the byte-level
//! line splitter reassembling a multi-byte UTF-8 character split across chunks.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use mock_provider::{MockConfig, MockServer};
use rho_agent::clock::{Clock, FixedClock};
use rho_agent::provider::ModelProvider;
use rho_ai::{OpenAICompatibleConfig, OpenAICompatibleProvider};
use support::user;

fn read_fixture(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/sse")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()))
}

async fn collect_jsonl(mut stream: rho_agent::provider::AssistantEventStream) -> String {
    let mut lines = Vec::new();
    while let Some(event) = stream.next().await {
        lines.push(serde_json::to_string(&event).expect("serialize event"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A stream whose **total** duration exceeds the configured timeout but whose
/// per-chunk gaps stay under it must succeed — proving `create_client` uses a
/// per-read timeout (like httpx), not a total deadline. With the old
/// `.timeout()` this would abort mid-stream.
#[tokio::test]
async fn read_timeout_allows_a_slow_total_stream() {
    let body = read_fixture("openai_compatible/text.sse");
    // ~130-byte body in ~40-byte chunks (≈4 chunks) at 200ms/chunk ≈ 0.8s total,
    // each gap 0.2s. Client timeout 0.5s: a total deadline would trip, a per-read
    // timeout won't.
    let config = MockConfig::sse(body)
        .with_chunk_size(40)
        .with_latency(Duration::from_millis(200));
    let server = MockServer::spawn(config).await;

    let client = rho_ai::http::create_client(0.5);
    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k").with_base_url(server.base_url()),
    )
    .with_client(client)
    .with_clock(Arc::new(FixedClock::fixture()) as Arc<dyn Clock>);

    let stream = provider.stream_response("gpt-x", "You are Tau.", &[user("Say hello")], &[], None);
    let actual = collect_jsonl(stream).await;
    server.shutdown().await;

    // Full success, byte-identical to the golden — nothing was aborted.
    assert_eq!(actual, read_fixture("openai_compatible/text.events.jsonl"));
}

/// A multi-byte UTF-8 character (`é` = 0xC3 0xA9) split across single-byte chunks
/// must reassemble intact — exercising the mock's byte chunking and the engine's
/// byte-level `LineSplitter` end to end.
#[tokio::test]
async fn multibyte_utf8_split_across_chunks_reassembles() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"caf\u{e9}\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\u{2728}\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let config = MockConfig::sse(body.as_bytes().to_vec()).with_chunk_size(1);
    let server = MockServer::spawn(config).await;

    let provider = OpenAICompatibleProvider::new(
        OpenAICompatibleConfig::new("k").with_base_url(server.base_url()),
    )
    .with_clock(Arc::new(FixedClock::fixture()) as Arc<dyn Clock>);

    let stream = provider.stream_response("gpt-x", "You are Tau.", &[user("hi")], &[], None);
    let jsonl = collect_jsonl(stream).await;
    server.shutdown().await;

    // The final `done` message's text must be the reassembled multi-byte content.
    let done: serde_json::Value = jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == "done")
        .expect("a done event");
    let text = done["message"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text, "caf\u{e9}\u{2728}",
        "multi-byte content must survive chunking"
    );
}
