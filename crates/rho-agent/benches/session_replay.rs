//! M6 family (b): session-replay throughput.
//!
//! Times the hot path a session load takes in both rho and tau — parse every
//! JSONL entry line, then replay the entry log into a [`SessionState`]. The
//! datasets are the pinned synthetic trees under
//! `fixtures/sessions/synthetic/` (1k/10k/100k × linear/deep-branch/
//! compaction-heavy); the 100k trees ship gzipped and are inflated in-process
//! so the bench is self-contained (`cargo bench` needs no pre-step).
//!
//! Criterion is the measurement engine on the rho side; `tools/bench/
//! tau_session_replay.py` is the tau counterpart. `tools/bench/gen_report.py`
//! normalizes both into the tables in `dev-notes/benchmarks.md`. Throughput is
//! declared in entries so Criterion reports entries/sec directly.
//!
//! CI compiles this (`cargo bench --no-run`) but never runs it — wall-clock in
//! shared CI is noise. Run it locally via `just bench`.
#![allow(missing_docs, clippy::doc_markdown)]

use std::hint::black_box;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use flate2::read::GzDecoder;
use rho_agent::session::jsonl::entries_from_json_lines;
use rho_agent::session::memory::SessionState;

/// The `fixtures/sessions/synthetic` directory, resolved from the crate root.
fn synthetic_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sessions/synthetic")
}

/// Read a synthetic dataset, inflating `*.jsonl.gz` transparently.
fn read_dataset(name: &str) -> String {
    let dir = synthetic_dir();
    let plain = dir.join(format!("{name}.jsonl"));
    if plain.exists() {
        return std::fs::read_to_string(&plain)
            .unwrap_or_else(|e| panic!("read {}: {e}", plain.display()));
    }
    let gz = dir.join(format!("{name}.jsonl.gz"));
    let bytes = std::fs::read(&gz).unwrap_or_else(|e| panic!("read {}: {e}", gz.display()));
    let mut out = String::new();
    GzDecoder::new(&bytes[..])
        .read_to_string(&mut out)
        .unwrap_or_else(|e| panic!("inflate {}: {e}", gz.display()));
    out
}

/// Datasets to bench. `compaction-heavy-100k` is deliberately excluded: compaction
/// replay is O(n²) in BOTH rho and tau (each compaction entry rescans the retained
/// messages — a shared, byte-compatible algorithm, not a rho regression; measured
/// tau 10k replay ≈ 7 s, actually slower than rho). At 100k that cell takes minutes
/// per iteration in either implementation and adds no signal beyond the 1k→10k
/// trend the other rows already show. The exclusion is intentional and logged, not
/// a silent cap.
fn datasets() -> Vec<String> {
    let families = ["linear", "deep-branch", "compaction-heavy"];
    let sizes = ["1k", "10k", "100k"];
    let mut out = Vec::new();
    for family in families {
        for size in sizes {
            if family == "compaction-heavy" && size == "100k" {
                continue;
            }
            out.push(format!("{family}-{size}"));
        }
    }
    out
}

fn bench_session_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_replay");
    // Large trees are slow; keep sample counts at the Criterion floor so a full
    // sweep finishes in a couple of minutes. Criterion still reports mean + σ.
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for name in datasets() {
        let content = read_dataset(&name);
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        let n_entries = lines.len() as u64;

        group.throughput(Throughput::Elements(n_entries));
        group.bench_function(&name, |b| {
            b.iter(|| {
                // Parse every line, then replay the log — exactly what a
                // `SessionStorage::read_all` + `SessionState::from_entries` load
                // does, and what the tau timer measures.
                let entries =
                    entries_from_json_lines(black_box(&lines)).expect("synthetic fixtures parse");
                let state = SessionState::from_entries(black_box(&entries));
                black_box(state);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_session_replay);
criterion_main!(benches);
