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

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rho_agent::clock::{Clock, FixedClock};
use rho_agent::messages::AssistantMessage;
use rho_ai::stream::{Delta, StreamAccumulator};

/// Build the input signal list once: `n` text deltas plus a terminal `End`. This
/// is *provider-parse* work (constructing the signals), not canonicalization, so
/// it is built outside the timed region — matching `tau_canonicalize.py`, which
/// pre-builds its `ProviderEvent`s before timing. Criterion's `iter_batched`
/// clones this template in the (untimed) setup step so the measured region is
/// only the accumulator work, never the per-delta `String` allocation.
fn make_inputs(n: usize) -> Vec<Delta> {
    let mut deltas = Vec::with_capacity(n + 1);
    for _ in 0..n {
        deltas.push(Delta::Text("tok ".to_string()));
    }
    deltas.push(Delta::End {
        message: AssistantMessage::default(),
        finish_reason: Some("stop".to_string()),
    });
    deltas
}

/// Feed prepared deltas through a fresh accumulator and drain every canonical
/// event, returning the total event count (kept live via `black_box`).
fn run_canonicalize(deltas: Vec<Delta>, clock: &Arc<dyn Clock>) -> usize {
    let mut acc = StreamAccumulator::new("anthropic-messages", "anthropic", "bench-model", clock);
    let mut events = acc.response_start().len();
    for delta in deltas {
        events += acc.apply(delta).len();
    }
    events += acc.finish().len();
    events
}

fn bench_sse_canonicalize(c: &mut Criterion) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::fixture());

    let mut group = c.benchmark_group("sse_canonicalize");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for n in [100_usize, 1_000, 10_000] {
        let template = make_inputs(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &_n| {
            b.iter_batched(
                || template.clone(),
                |deltas| black_box(run_canonicalize(deltas, &clock)),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_sse_canonicalize);
criterion_main!(benches);
