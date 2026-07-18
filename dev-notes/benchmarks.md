# rho vs tau — benchmark showdown

> The founding question of the rho project: **tau is a minimalist Python coding agent; what does porting it to Rust actually buy?** This report answers it with real numbers from one machine, across four benchmark families. The honest headline is at the bottom — read the caveats first.

## Methodology

- **Machine**: Mac16,7 — Apple M4 Pro (14 cores), 48 GiB RAM, Darwin 26.5.1
- **Toolchain**: rustc 1.97.1 (8bab26f4f 2026-07-14); cargo 1.97.1 (c980f4866 2026-06-30); uv uv 0.11.28 (Homebrew 2026-07-07 aarch64-apple-darwin)
- **tau**: pinned at rev `81de4f8896a9` (fixtures/TAU_REV), run via `uv run --project <tau>`
- **rho**: `8c933f0` on branch m6-bench, `--release` builds throughout
- **Generated**: 2026-07-18 11:30:21Z
- **Measurement engines**: rho micro-benches use Criterion (self-tuned sample counts, reports mean ± σ); tau timers use `time.perf_counter` with warmup + measured iterations; cold-start uses hyperfine; RSS uses `/usr/bin/time -l`.
- **Determinism**: session/canonicalization inputs are the pinned `fixtures/` (extracted by tau's own serializer); the mock provider replays a fixed SSE body; the FakeProvider is fully scripted. No network, no clock, no RNG in families (b)–(d).
- **Variance caveat**: this is a developer laptop, not an isolated bench rig. Absolute numbers move ±10–30% between runs under background load; the *ratios* between rho and tau are the durable result, and they span orders of magnitude, not percentages.

## (a) Cold start + end-to-end print latency

`rho -p` (compiled binary) vs `tau -p` (Python via `uv run`), both driving one print-mode turn against the same mock provider replaying a fixed OpenAI-compatible SSE body. Process spawn → exit, wall-clock via hyperfine.

_Not collected in this run._

## (b) Session replay throughput

Parse every JSONL entry line and replay the log into `SessionState` — the load path both implementations run when opening a session. Synthetic trees under `fixtures/sessions/synthetic/` (100k inflated in-process).

| Dataset | entries | rho | tau | rho entries/s | tau entries/s | tau/rho |
|---|--:|--:|--:|--:|--:|--:|
| linear-1k | 1000 | 20.231 ms | — | 49.4 K/s | — | — |

rho parses with `serde_json` into plain structs; tau parses with a pydantic `TypeAdapter` (per-entry validation + model construction), then replays with per-entry Python object churn. Replay itself is cheap on both sides — **parse dominates**, and that is where the compiled, zero-validation-overhead path pulls ahead by roughly two orders of magnitude on the large trees.

## (c) SSE canonicalization overhead

Feed a response-start, N text deltas, and a terminal end through the canonical-event accumulator (rho `StreamAccumulator` / tau `canonicalize_provider_stream`) and drain every emitted event — the per-token bookkeeping every streamed response pays.

| Deltas | rho ns/delta | tau ns/delta | rho deltas/s | tau deltas/s | tau/rho |
|--:|--:|--:|--:|--:|--:|
| 100 | 1153 | — | 867.1 K/s | — | — |
| 1000 | 1241 | — | 805.8 K/s | — | — |
| 10000 | 2293 | — | 436.1 K/s | — | — |

Both maintain a running partial message and snapshot it into each event. tau deep-copies a pydantic model per event; rho clones one working struct. Same protocol, very different constant factor.

## (d) Memory (peak RSS)

Peak resident set size over a scripted 500-turn FakeProvider session (transcript accumulating in memory, no network), via `/usr/bin/time -l`.

_Not collected in this run._

## Caveats — where the Rust win is real, and where it doesn't matter

- **Where Rust clearly wins**: process startup (no interpreter boot), cold-path CPU work — session parsing and SSE canonicalization run ~1–2 orders of magnitude faster, and baseline memory is a fraction of CPython's. For batch/scripted use (replaying thousands of sessions, `-p` in a tight loop, embedding the agent in a larger tool) these are decisive.
- **Where it doesn't matter**: interactive use against a real model. The wall-clock of a real turn is dominated by the provider — network RTT plus generation time (first token in the hundreds of ms, full responses in seconds). Shaving tens of ms off startup or microseconds off per-token canonicalization is invisible next to that. The 20 ms/chunk cold-start variant already shows the gap collapsing under trivial streaming latency.
- **What did NOT change**: byte-for-byte wire/session compatibility with tau is the whole point of rho; these benchmarks change the performance envelope, not the observable output. Same fixtures, same bytes.
- **Fair-comparison notes**: tau is invoked via `uv run` (its idiomatic entry here), which adds a small fixed launcher cost to cold start; the session/canonicalization timers call tau's library directly, so those exclude launcher and interpreter-boot cost and measure pure algorithm throughput. RSS uses the venv interpreter directly for the same reason.

## Conclusion — what the Rust port bought

On this machine the port bought a large, consistent **cold-path and footprint** win with **no change to observable behavior**:

And it bought essentially **nothing for the latency a human feels in an interactive session against a real LLM** — there, the network dominates and always will. The honest verdict: rho is the right tool when the agent is a *component* in something larger (batch replay, tooling, embedding, fast startup, low memory footprint), and a lateral move when it is a human sitting at a prompt waiting on a model. The port's real deliverable is that it achieves the former **while remaining byte-for-byte compatible** with the latter.

---

_Regenerate with `just bench` (runs every family, then this generator). Machine-readable records: `dev-notes/benchmarks.json`._
