//! Wire-level regression tests for [`rho_ai::engine::send_reqwest`] header
//! semantics.
//!
//! tau builds provider headers as a Python dict and sends them with httpx, so
//! setting a header twice overwrites — exactly one value reaches the wire.
//! rho ports the same headers as a `Vec<(String, String)>`; applying them with
//! append semantics produced a duplicate `content-type` (one from
//! `RequestBuilder::json`, one from the provider list), which the `ChatGPT`
//! Codex backend rejects with `400 Unsupported content type`.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Bind a local server, capture one raw request head, and return the header
/// lines observed on the wire for a `send_reqwest` call with `headers`.
async fn captured_header_lines(headers: rho_ai::types::HeaderList) -> Vec<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            let n = stream.read(&mut byte).await.expect("read");
            if n == 0 {
                break;
            }
            head.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .expect("write");
        String::from_utf8_lossy(&head).into_owned()
    });

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/test");
    let payload = serde_json::json!({"ping": true});
    if let Err(error) = rho_ai::engine::send_reqwest(&client, &url, &headers, &payload).await {
        panic!("request failed: {}", error.message);
    }

    server
        .await
        .expect("server task")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn values_for<'a>(lines: &'a [String], name: &str) -> Vec<&'a str> {
    let prefix = format!("{name}:");
    lines
        .iter()
        .filter(|line| line.to_ascii_lowercase().starts_with(&prefix))
        .map(|line| line[prefix.len()..].trim())
        .collect()
}

/// The provider header list always carries `content-type: application/json`
/// (ported byte-for-byte from tau); `RequestBuilder::json` sets its own copy.
/// Exactly one must reach the wire or strict backends (`ChatGPT` Codex) 400.
#[tokio::test]
async fn content_type_reaches_the_wire_exactly_once() {
    let lines = captured_header_lines(vec![
        ("Authorization".to_string(), "Bearer test-token".to_string()),
        ("accept".to_string(), "text/event-stream".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ])
    .await;

    assert_eq!(
        values_for(&lines, "content-type"),
        vec!["application/json"],
        "expected exactly one content-type header, got: {lines:?}"
    );
    assert_eq!(values_for(&lines, "accept"), vec!["text/event-stream"]);
    assert_eq!(
        values_for(&lines, "authorization"),
        vec!["Bearer test-token"]
    );
}

/// tau's header dicts overwrite on duplicate keys, so a configured header a
/// provider also sets must resolve to the later (provider) value — last wins.
#[tokio::test]
async fn duplicate_names_in_the_list_resolve_last_wins() {
    let lines = captured_header_lines(vec![
        ("x-custom".to_string(), "configured".to_string()),
        ("x-custom".to_string(), "provider".to_string()),
    ])
    .await;

    assert_eq!(
        values_for(&lines, "x-custom"),
        vec!["provider"],
        "expected dict-style last-wins semantics, got: {lines:?}"
    );
}
