//! TUI-polish (perf pass): transcript render cost on a large transcript.
//!
//! `build_transcript_lines` is the frame hot path — it re-parses markdown,
//! detects code fences, and word-wraps every settled turn. Before the perf
//! pass it ran on *every* frame (each 150 ms spinner tick during a run, and
//! each keystroke while composing). This bench times a single full rebuild of a
//! realistic multi-hundred-turn transcript so the cache win (a rebuild only
//! when the fingerprint actually changes) can be quoted before/after.
//!
//! Run locally via `cargo bench -p rho-tui`; CI compiles but never runs it.
#![allow(missing_docs, clippy::doc_markdown)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rho_tui::state::{ChatItemRole, TuiState};
use rho_tui::theme::tau_dark_theme;
use rho_tui::widgets::{TranscriptCache, build_transcript_lines};

/// Build a realistic transcript of `turns` user→assistant→thinking cycles, each
/// assistant turn carrying markdown, a bullet list, inline code, bold, and a
/// fenced code block — the shapes that make rendering expensive (markdown parse
/// + fence detection + word wrap for every settled turn).
fn big_state(turns: usize) -> TuiState {
    let mut state = TuiState::new();
    state.show_tool_results = true;
    state.show_thinking = true;
    for i in 0..turns {
        state.add_item(
            ChatItemRole::User,
            format!("Please refactor module {i} and explain the tradeoffs in detail."),
        );
        state.add_item(
            ChatItemRole::Assistant,
            format!(
                "## Plan for module {i}\n\nHere is what I'll do:\n\n- read `mod{i}.rs`\n- \
                 extract the **hot loop** into a helper\n- add a regression test that covers \
                 the boundary conditions and the overflow path\n\n```rust\nfn helper_{i}(x: u64) -> u64 {{\n    \
                 x.wrapping_mul(2).saturating_add({i})\n}}\n```\n\nThat keeps it readable and fast, \
                 and the wrapping semantics match the original [reference](https://example.com/{i})."
            ),
        );
        state.add_item(
            ChatItemRole::Thinking,
            format!(
                "Considering module {i}: the helper avoids a branch and the test pins behavior."
            ),
        );
    }
    state
}

fn bench_transcript(c: &mut Criterion) {
    let theme = tau_dark_theme();
    let mut group = c.benchmark_group("build_transcript_lines");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));
    for turns in [50usize, 200, 500] {
        let state = big_state(turns);
        // Uncached rebuild — the pre-cache per-frame cost.
        group.bench_function(format!("rebuild_{turns}turns_w100"), |b| {
            b.iter(|| black_box(build_transcript_lines(black_box(&state), &theme, 100)));
        });
        // Cache hit — the post-cache cost of a frame that changed nothing (idle
        // typing, an idle tick): one fingerprint hash + a Vec<Line> clone for the
        // Paragraph, no markdown parse or wrap.
        group.bench_function(format!("cache_hit_{turns}turns_w100"), |b| {
            let mut cache = TranscriptCache::default();
            let _ = cache.lines(&state, &theme, 100); // prime
            b.iter(|| black_box(cache.lines(black_box(&state), &theme, 100).to_vec()));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_transcript);
criterion_main!(benches);
