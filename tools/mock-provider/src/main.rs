//! CLI entrypoint for the mock provider server.
//!
//! Serves one recorded SSE body on an address, with configurable chunk size and
//! per-chunk latency. Used by golden tests (spawned in-process via the library)
//! and, in M6, as a standalone benchmark fixture server.

use std::time::Duration;

use clap::Parser;
use mock_provider::MockConfig;

/// Replay a recorded provider SSE body over HTTP.
#[derive(Parser, Debug)]
#[command(
    name = "mock-provider",
    about = "Replay recorded provider SSE bodies for tests/benchmarks."
)]
struct Args {
    /// Path to the `.sse` body to replay for every POST.
    #[arg(long)]
    body: std::path::PathBuf,

    /// HTTP status code to return.
    #[arg(long, default_value_t = 200)]
    status: u16,

    /// Bytes per streamed chunk (0 = send the whole body at once).
    #[arg(long, default_value_t = 0)]
    chunk_size: usize,

    /// Milliseconds to wait before each chunk.
    #[arg(long, default_value_t = 0)]
    latency_ms: u64,

    /// Address to bind (host:port).
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,

    /// `content-type` header to send.
    #[arg(long, default_value = "text/event-stream")]
    content_type: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let body = std::fs::read(&args.body)?;
    let mut config = MockConfig::sse(body)
        .with_status(args.status)
        .with_latency(Duration::from_millis(args.latency_ms));
    config.content_type = args.content_type;
    if args.chunk_size > 0 {
        config = config.with_chunk_size(args.chunk_size);
    }

    // The library binds an ephemeral port; for the CLI we honor `--addr` by
    // binding here and reusing the server's request handler indirectly is not
    // exposed, so re-bind explicitly.
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    let addr = listener.local_addr()?;
    println!(
        "mock-provider listening on http://{addr} (status {})",
        config.status
    );
    // Reuse MockServer by spawning on its own ephemeral port would ignore --addr;
    // instead serve directly here with the same behavior.
    mock_provider::serve(listener, config).await;
    Ok(())
}
