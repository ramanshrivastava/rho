# Phase 6 — the benchmark showdown

M6 is the milestone the whole port pointed at: now that rho is byte-for-byte
compatible with tau, *what did the Rust rewrite actually buy?* This phase builds
the machinery to answer that honestly — four benchmark families, a reproducible
`just bench`, and a report generator that turns raw measurements into
`dev-notes/benchmarks.md` (+ a machine-readable `benchmarks.json`). The report
itself carries the numbers and the narrative; this journal is the engineering
log: how each harness is built, the tau-timing methodology, and the pitfalls a
future milestone must respect.

## What was built

```
crates/rho-agent/benches/session_replay.rs   # (b) criterion: parse+replay
crates/rho-ai/benches/sse_canonicalize.rs     # (c) criterion: StreamAccumulator
crates/rho-agent/examples/rss_session.rs      # (d) N-turn FakeProvider driver (RSS)
tools/bench/
  cold_start.sh        # (a) hyperfine rho -p vs tau -p vs mock-provider
  rss.sh               # (d) /usr/bin/time -l peak RSS, rho vs tau
  tau_session_replay.py# (b) tau timer
  tau_canonicalize.py  # (c) tau timer
  tau_rss_session.py   # (d) tau N-turn driver
  _common.py           # shared stats + normalized-JSON emit
  gen_report.py        # normalize everything -> dev-notes/benchmarks.{md,json}
  run_all.sh           # orchestrator behind `just bench`
```

Four families, each measuring a different axis:

- **(a) Cold start / e2e print latency** — process spawn→exit for one `-p` turn,
  compiled binary vs `uv run` Python, both against the mock provider. hyperfine,
  three variants (0 ms, 20 ms/chunk, `--version`).
- **(b) Session replay throughput** — parse every JSONL entry + replay into
  `SessionState`, over the pinned synthetic trees — 1k/10k/100k ×
  linear/deep-branch/compaction-heavy, **minus `compaction-heavy-100k`** (O(n²),
  see pitfalls), so **eight datasets**. Criterion vs a `perf_counter` timer.
- **(c) SSE canonicalization** — feed N text deltas through the canonical-event
  accumulator and drain events. Criterion vs `canonicalize_provider_stream`.
- **(d) Memory** — peak RSS over scripted FakeProvider sessions swept across
  1/500/2000 turns, plus a
  graceful-skip hook for real-LLM spot checks when `ANTHROPIC_API_KEY` is set.

## Criterion setup

`criterion` and `flate2` live in `[workspace.dependencies]`; the two benching
crates add them as dev-deps and declare `[[bench]] harness = false`. Design
choices worth remembering:

- **Throughput in domain units.** `group.throughput(Elements(n))` makes Criterion
  report entries/sec (b) and deltas/sec (c) directly, and lets `gen_report.py`
  derive ns-per-token without a second pass.
- **Self-contained inputs.** The 100k synthetic trees ship gzipped (fixture
  policy: `fixtures/` is read-only and never hand-edited), so the bench inflates
  them in-process with `flate2` rather than requiring a pre-step or writing into
  the read-only fixture dir. The tau timer mirrors this with `gzip.decompress`.
- **Bounded sweeps.** `sample_size(10)` (the Criterion floor) + short
  warm-up/measurement on the replay group keeps the full eight-dataset run to a
  couple of minutes; Criterion still reports mean ± σ. The SSE group uses default
  sampling (cheap) and excludes input construction via `iter_batched`.
- **Report ingestion.** Criterion writes `target/criterion/<group>/<id>/new/{benchmark,estimates}.json`.
  `benchmark.json` carries `group_id` / `function_id` / `value_str` /
  `throughput.Elements`; `estimates.json` carries `mean.point_estimate` (ns) and
  `std_dev.point_estimate`. The generator globs `*/new`, pairs those two files,
  and joins to the tau records by `(family, variant)`.

## tau-timing methodology

The tau side is deliberately *not* Criterion — it's a small `perf_counter`
harness (N warmup + M measured iterations, `statistics.fmean`/`stdev`) emitting
the **same normalized JSON shape** the generator consumes, so the two engines
meet in `gen_report.py` rather than pretending to be one tool. Principles kept
honest:

- **Measure the same work.** The replay timer calls `entries_from_json_lines`
  then `SessionState.from_entries` — the exact library calls a session load runs
  — not the async `JsonlSessionStorage.read_all` wrapper (that would add file I/O
  the Rust bench doesn't count). Parse and replay are timed together because that
  is what "load a session" costs.
- **Isolate the unit under test.** In the canonicalization timer the
  `ProviderEvent` list is built **once** and re-yielded each iteration — building
  pydantic models is provider-parse work, not canonicalization, so charging it
  per-iteration would flatter Rust unfairly. Matches the Rust bench, which only
  re-allocates a trivial `"tok "` string per delta.
- **Amortize event-loop startup.** `canonicalize_provider_stream` is async; the
  timer drives all iterations inside a single `anyio.run` rather than
  `anyio.run`-per-iteration, so event-loop spin-up isn't charged to each sample.
- **Per-size iteration scaling.** A 100k tau parse is ~9 s (see pitfalls), so the
  replay timer uses `(warmup, measured)` of `(3,30)`/`(2,10)`/`(1,3)` for
  1k/10k/100k. Enough for a stable mean without a 10-minute sweep; a `--scale`
  flag dials it for smoke runs.

## Pitfalls (read before touching M6 again)

1. **tau's variadic-arg CLI quirk.** `tau setup` is a *positional* on the root
   Typer callback, dispatched only when `positional_args == ["setup"]` exactly.
   Passing `tau setup --base-url X --model Y` puts the options *after* the
   positional, click folds them in such that the setup branch is missed, and the
   **TUI launches instead** (it then blocks forever — this looked like a hang).
   The working form is **options before the positional**:
   `tau --provider openai --model gpt-x --base-url URL --api-key-env … setup`.
   `cold_start.sh` encodes this. (Print mode is `-p`-driven with no positional,
   so ordering there is irrelevant.)
2. **Config isolation without wrecking uv.** rho honors `RHO_HOME`; tau hard-codes
   `Path.home()/.tau` with no override, so the only lever is `HOME`. Setting a
   throwaway `HOME` for tau is fine **provided** `UV_CACHE_DIR` is pinned to the
   real cache and the project venv already exists (it lives in `<tau>/.venv`, not
   `HOME`) — otherwise uv re-resolves under the empty home and every invocation is
   slow and noisy. `cold_start.sh` sets both; the user's real `~/.tau` / `~/.rho`
   are never touched.
3. **Parse dominates, and it's slow in Python.** For a 100k linear tree, tau
   spends ~9.2 s in `entries_from_json_lines` (pydantic `TypeAdapter` per entry)
   and ~0.17 s in `from_entries`. This is *the* headline finding for family (b),
   but it also means the tau timer must scale iterations down for big trees or CI
   patience evaporates.
4. **Compaction replay is O(n²) — in both languages.** `apply_compaction`
   rebuilds the retained-message list, and a compaction-heavy log applies many of
   them, so replay scales quadratically. This is *shared* with tau (its
   `_apply_compaction` does the same; measured tau `compaction-heavy-10k` replay ≈
   7 s vs rho ≈ 2.6 s — rho is faster but the curve is the same), so it is a
   byte-compatible property, not a rho regression. Consequence: both timers skip
   `compaction-heavy-100k` (minutes per iteration, no new signal past the 1k→10k
   trend). The skip is explicit and reported, per the "no silent caps" rule. If a
   later milestone makes compaction linear, it must change *both* sides or the
   comparison stops being apples-to-apples.
5. **Clippy `--all-targets` lints benches too.** The workspace sets
   `missing_docs = warn` and CI runs clippy with `-D warnings` over
   `--all-targets`, which compiles benches + examples. `criterion_group!` expands
   to an undocumented item, and pedantic `doc_markdown` trips on `FakeProvider`
   etc. in module docs. Each bench/example carries
   `#![allow(missing_docs, clippy::doc_markdown)]` — appropriate for harness
   files, not for library code.
6. **RSS: measure the worker, not the launcher — and the fake double surprised
   us.** `rss.sh` runs the Rust example binary and the **venv python directly**
   (not via `uv run`), so `/usr/bin/time -l`'s "maximum resident set size"
   reflects the actual process (bytes, on Darwin). The surprise: at a 1-turn
   baseline rho is ~2 MiB vs tau's ~41 MiB (interpreter + import graph), but rho's
   RSS grows **super-linearly** and *crosses* tau's around 500 turns (rho ≈ 73
   MiB, tau ≈ 45 MiB; at 2000 turns rho ≈ 1.1 GiB, tau ≈ 69 MiB). Root cause: rho's
   `FakeProvider` records each call with `messages.to_vec()`, deep-copying the
   growing transcript *by value* on every turn → O(n²) retained `AgentMessage`
   copies; tau's `list(messages)` copies *references* → O(n). Rust value semantics
   vs Python reference semantics, in a test double a real provider never
   exercises. We sweep turn counts and report it straight (baseline win + O(n²)
   artifact) rather than cherry-picking the baseline. A cheap follow-up: have
   `RecordedCall` hold `Arc`/references. This is exactly the kind of thing a
   benchmark exists to surface.
7. **Machine noise is real.** These run on a developer laptop; absolute numbers
   swing ±10–30% under background load. `run_all.sh` is strictly serial for this
   reason, and the report leads with *ratios* (orders of magnitude) rather than
   treating any single millisecond figure as gospel.

## CI

Benchmarks must **compile** in CI but never **run** (wall-clock on shared runners
is noise). The new `benches compile` job runs `cargo bench --workspace --no-run`
+ `cargo build --workspace --examples`; `just bench-check` is the local mirror.
Actual measurement is a developer-invoked `just bench`.

## The answer

See `dev-notes/benchmarks.md` for the numbers and the full narrative. The
one-line version: the port bought a large, durable **cold-path CPU + startup +
memory** win (roughly 1–2 orders of magnitude on parse/canonicalization, a
fraction of CPython's baseline RSS) **with zero change to observable behavior** —
and bought essentially nothing for the latency a human feels talking to a real
model, where the network dominates. rho earns its keep as a *component* (batch
replay, tooling, embedding, fast/lean startup); it's a lateral move as a seat at
an interactive prompt. That it manages the former while staying byte-for-byte
compatible with the latter is the whole point.
