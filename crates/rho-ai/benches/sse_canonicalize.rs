//! M6 family (c): SSE canonicalization overhead.
//!
//! Micro-benchmarks the accumulator that turns pre-parsed provider signals into
//! canonical `AssistantMessageEvent`s — rho's [`StreamAccumulator`], the direct
//! analogue of tau's `canonicalize_provider_stream`. We feed a `response_start`,
//! N text deltas, and a terminal `End`, and count the canonical events emitted.
//! Declaring throughput in *deltas* makes Criterion report deltas/sec; the
//! report generator also derives ns-per-token-delta (mean / N).
//!
//! The tau counterpart is `tools/bench/tau_canonicalize.py`, which drives
//! `canonicalize_provider_stream` over the same shape of ProviderTextDelta
//! events. This isolates the per-token bookkeeping + snapshot cost that every
//! streamed response pays, independent of the network.
//!
//! CI compiles this (`cargo bench --no-run`); it runs locally via `just bench`.
#![allow(missing_docs, clippy::doc_markdown)]

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rho_agent::clock::{Clock, FixedClock};
use rho_agent::messages::AssistantMessage;
use rho_ai::stream::{Delta, StreamAccumulator};

/// Feed `n` text deltas through a fresh accumulator and drain every canonical
/// event, returning the total event count (kept live via `black_box`).
fn run_canonicalize(n: usize, clock: &Arc<dyn Clock>) -> usize {
    let mut acc = StreamAccumulator::new("anthropic-messages", "anthropic", "bench-model", clock);
    let mut events = acc.response_start().len();
    for _ in 0..n {
        events += acc.apply(Delta::Text("tok ".to_string())).len();
    }
    events += acc
        .apply(Delta::End {
            message: AssistantMessage::default(),
            finish_reason: Some("stop".to_string()),
        })
        .len();
    events += acc.finish().len();
    events
}

fn bench_sse_canonicalize(c: &mut Criterion) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::fixture());

    let mut group = c.benchmark_group("sse_canonicalize");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for n in [100_usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| black_box(run_canonicalize(black_box(n), &clock)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_sse_canonicalize);
criterion_main!(benches);
