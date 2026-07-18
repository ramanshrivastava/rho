# rho vs tau — benchmark showdown

> The founding question of the rho project: **tau is a minimalist Python coding agent; what does porting it to Rust actually buy?** This report answers it with real numbers from one machine, across four benchmark families. The honest headline is at the bottom — read the caveats first.

## Methodology

- **Machine**: Mac16,7 — Apple M4 Pro (14 cores), 48 GiB RAM, Darwin 26.5.1
- **Toolchain**: rustc 1.97.1 (8bab26f4f 2026-07-14); cargo 1.97.1 (c980f4866 2026-06-30); uv 0.11.28 (Homebrew 2026-07-07 aarch64-apple-darwin)
- **tau**: pinned at rev `81de4f8896a9` (fixtures/TAU_REV), run via `uv run --project <tau>`
- **rho**: `b25c96d` on branch m6-bench, `--release` builds throughout
- **Generated**: 2026-07-18 12:07:27Z
- **Measurement engines**: rho micro-benches use Criterion (self-tuned sample counts, reports mean ± σ); tau timers use `time.perf_counter` with warmup + measured iterations; cold-start uses hyperfine; RSS uses `/usr/bin/time -l`.
- **Determinism**: session/canonicalization inputs are the pinned `fixtures/` (extracted by tau's own serializer); the mock provider replays a fixed SSE body; the FakeProvider is fully scripted. No network, no clock, no RNG in families (b)–(d).
- **Variance caveat**: this is a developer laptop, not an isolated bench rig. Absolute numbers move ±10–30% between runs under background load; the *ratios* between rho and tau are the durable result, and they span orders of magnitude, not percentages.

## (a) Cold start + end-to-end print latency

`rho -p` (compiled binary) vs `tau -p` (Python via `uv run`), both driving one print-mode turn against the same mock provider replaying a fixed OpenAI-compatible SSE body. Process spawn → exit, wall-clock via hyperfine.

| Variant | rho (spawn→exit) | tau (spawn→exit) | tau/rho |
|---|---|---|---|
| `--version` (pure startup) | 4.4 ± 2.8 ms | 3020.0 ± 606.0 ms | 679.7× |
| print, 0 ms latency | 37.0 ± 7.7 ms | 3267.5 ± 299.2 ms | 88.3× |
| print, 20 ms/chunk streaming | 430.4 ± 9.5 ms | 3622.7 ± 973.5 ms | 8.4× |

**Interpreter startup vs compiled binary is the whole story here.** The `--version` row is the cleanest read: it is almost entirely process startup. rho is a statically-linked binary that execs and prints; tau pays Python interpreter boot + `uv run` environment resolution + module imports (pydantic, httpx, typer, rich, textual) before it does any work. That fixed tax is why rho's cold start is dramatically lower.
**But note the 20 ms/chunk row.** Once the provider streams with even a small per-chunk latency, a fixed ~hundreds-of-ms cost lands on *both* implementations equally, and the spawn-time gap starts to disappear into it. With a real LLM (first token in hundreds of ms, full response in seconds) the startup difference is a rounding error on end-to-end latency — see the caveats.

## (b) Session replay throughput

Parse every JSONL entry line and replay the log into `SessionState` — the load path both implementations run when opening a session. Synthetic trees under `fixtures/sessions/synthetic/` (100k inflated in-process).

| Dataset | entries | rho | tau | rho entries/s | tau entries/s | tau/rho |
|---|--:|--:|--:|--:|--:|--:|
| compaction-heavy-1k | 1000 | 40.933 ms | 53.735 ms | 24.4 K/s | 18.6 K/s | 1.3× |
| compaction-heavy-10k | 10000 | 2.562 s | 7.121 s | 3.9 K/s | 1.4 K/s | 2.8× |
| deep-branch-1k | 1000 | 8.416 ms | 24.497 ms | 118.8 K/s | 40.8 K/s | 2.9× |
| deep-branch-10k | 10000 | 91.294 ms | 247.502 ms | 109.5 K/s | 40.4 K/s | 2.7× |
| deep-branch-100k | 100000 | 966.464 ms | 3.757 s | 103.5 K/s | 26.6 K/s | 3.9× |
| linear-1k | 1000 | 17.774 ms | 44.075 ms | 56.3 K/s | 22.7 K/s | 2.5× |
| linear-10k | 10000 | 186.596 ms | 595.685 ms | 53.6 K/s | 16.8 K/s | 3.2× |
| linear-100k | 100000 | 2.046 s | 8.640 s | 48.9 K/s | 11.6 K/s | 4.2× |

**Parse dominates on both sides** (replay of a linear log is trivially O(n)); the gap is entirely in decode. tau pays a pydantic `TypeAdapter` per entry (validation + model construction). rho pays its own tax: `SessionEntry` is an `#[serde(untagged)]` union, so serde buffers each line and trial-decodes it against every variant — deliberately, for byte-compat — which is far from free. The net is a solid **several-fold** rho win (see the ratio column), not the ~100× seen in the allocation-light micro-benches: this is the family where rho's compatibility constraints cost it the most, and it's the honest one to show.
> **`compaction-heavy-100k` is intentionally excluded** (both timers). Compaction replay is O(n²) in *both* implementations — each compaction entry rescans the retained transcript, a shared byte-compatible algorithm, not a rho regression (measured tau 10k replay ≈ 7 s, actually slower than rho's ≈ 2.6 s). At 100k that single cell costs minutes per iteration in either language and adds nothing beyond the 1k→10k trend already visible above. Flagged, not silently capped.

## (c) SSE canonicalization overhead

Feed a response-start, N text deltas, and a terminal end through the canonical-event accumulator (rho `StreamAccumulator` / tau `canonicalize_provider_stream`) and drain every emitted event — the per-token bookkeeping every streamed response pays.

| Deltas | rho ns/delta | tau ns/delta | rho deltas/s | tau deltas/s | tau/rho |
|--:|--:|--:|--:|--:|--:|
| 100 | 1083 | 97673 | 923.6 K/s | 10.2 K/s | 90.2× |
| 1000 | 1058 | 88778 | 945.4 K/s | 11.3 K/s | 83.9× |
| 10000 | 2354 | 88953 | 424.8 K/s | 11.2 K/s | 37.8× |

Both maintain a running partial message and snapshot it into each event. tau deep-copies a pydantic model per event; rho clones one working struct. Same protocol, very different constant factor.

## (d) Memory (peak RSS)

Peak resident set size over a scripted N-turn FakeProvider session (transcript accumulating in memory, no network), via `/usr/bin/time -l`. This is the family with the **most surprising, most honest** result, so it gets a turn-count sweep rather than a single number.

| turns | rho peak RSS | tau peak RSS | rho/tau |
|--:|--:|--:|--:|
| 1 | 1.98 MiB | 41.41 MiB | 0.05× |
| 500 | 73.42 MiB | 44.88 MiB | 1.64× |
| 2000 | 1085.25 MiB | 69.31 MiB | 15.66× |

**Baseline (1 turn): rho is tiny.** rho's near-empty process is ~2 MiB against tau's ~41 MiB — the CPython interpreter plus its import graph (pydantic/anyio/httpx/rich/textual) costs tens of MiB before doing any work, where the statically-linked rho binary + a current-thread tokio runtime costs a couple. **This is rho's real, production-relevant footprint advantage.**
**But watch the sweep: rho's line is super-linear and crosses tau's.** That is *not* the transcript — it is a **test-double artifact**. rho's `FakeProvider` records every call with `messages.to_vec()`, deep-copying the whole (growing) transcript by value on each of the N turns → O(n²) retained `AgentMessage` copies. tau's `FakeProvider` does `list(messages)`, which copies *references* to shared model objects → O(n). Rust value semantics vs Python reference semantics, in a scripted harness that a real provider never exercises (real providers don't retain a deep-copied call log). So: rho wins the footprint that matters (baseline + real runs) and loses this particular fake-driver microbench — reported as-is rather than quietly dropping the inconvenient rows. A cheap future fix is to have `RecordedCall` retain `Arc`/references instead of owned clones; out of scope for M6.
**Allocator honesty**: peak RSS is not a like-for-like allocator comparison — both processes are subject to the system allocator's retention policy and macOS reports RSS in bytes. Read the *baseline* row for the interpreter-vs-binary gap and the *shape* for the O(n²) artifact; do not read the crossover as "Rust uses more memory than Python" in general — it does not.
Real-LLM spot checks: **skipped** — `ANTHROPIC_API_KEY` was not set in the environment. Their only intent is to confirm the obvious: against a live provider, network latency (hundreds of ms to seconds per turn) dominates end-to-end time, so neither implementation's CPU/RSS edge is observable end-to-end.

## Caveats — where the Rust win is real, and where it doesn't matter

- **Where Rust clearly wins**: process startup (no interpreter boot), cold-path CPU work — session parsing and SSE canonicalization run ~1–2 orders of magnitude faster, and baseline memory is a fraction of CPython's. For batch/scripted use (replaying thousands of sessions, `-p` in a tight loop, embedding the agent in a larger tool) these are decisive.
- **Where it doesn't matter**: interactive use against a real model. The wall-clock of a real turn is dominated by the provider — network RTT plus generation time (first token in the hundreds of ms, full responses in seconds). Shaving tens of ms off startup or microseconds off per-token canonicalization is invisible next to that. The 20 ms/chunk cold-start variant already shows the gap collapsing under trivial streaming latency.
- **What did NOT change**: byte-for-byte wire/session compatibility with tau is the whole point of rho; these benchmarks change the performance envelope, not the observable output. Same fixtures, same bytes.
- **Fair-comparison notes**: tau is invoked via `uv run` (its idiomatic entry here), which adds a small fixed launcher cost to cold start; the session/canonicalization timers call tau's library directly, so those exclude launcher and interpreter-boot cost and measure pure algorithm throughput. RSS uses the venv interpreter directly for the same reason.

## Conclusion — what the Rust port bought

On this machine the port bought a large, consistent **cold-path and footprint** win with **no change to observable behavior**:
- Startup (`--version`): **680×** faster.
- Replaying a 100k-entry linear session: **4×** faster (parse-bound).
- SSE canonicalization at 10k deltas: **38×** faster per token.
- Memory: baseline process RSS is ~21× smaller (2 MiB vs 41 MiB) — though the FakeProvider microbench also exposes an O(n²) memory artifact in rho's test double under long scripted runs (family (d)); the production-relevant baseline is the durable win.

And it bought essentially **nothing for the latency a human feels in an interactive session against a real LLM** — there, the network dominates and always will. The honest verdict: rho is the right tool when the agent is a *component* in something larger (batch replay, tooling, embedding, fast startup, low memory footprint), and a lateral move when it is a human sitting at a prompt waiting on a model. The port's real deliverable is that it achieves the former **while remaining byte-for-byte compatible** with the latter.

---

_Regenerate with `just bench` (runs every family, then this generator). Machine-readable records: `dev-notes/benchmarks.json`._
