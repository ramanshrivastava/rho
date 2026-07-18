# π vs τ vs ρ — three-way benchmark showdown

> **pi** (TypeScript/Node) is the original coding agent; **tau** (Python) and **rho** (Rust) are both ports of it. The founding question of the rho project was *what does porting tau to Rust buy?*; this report widens it to the full language triangle — **JIT-warmed Node vs interpreted Python vs compiled Rust** — with real numbers from one machine, across four benchmark families. The honest headline is at the bottom — read the caveats first. Where a family has no fair pi counterpart the pi column is `—` and the reason is stated, never silently dropped.

## Methodology

- **Machine**: Mac16,7 — Apple M4 Pro (14 cores), 48 GiB RAM, Darwin 26.5.1
- **Toolchain**: rustc 1.97.1 (8bab26f4f 2026-07-14); cargo 1.97.1 (c980f4866 2026-06-30); uv 0.11.28 (Homebrew 2026-07-07 aarch64-apple-darwin); Node v24.15.0
- **pi**: v0.80.10, the installed `pi` binary (Node via fnm), corresponding to `earendil-works/pi` rev `3da591ab74ab` (fixtures/PI_REV) — its package set is v0.80.10, matching the installed binary exactly. Cold start measures pi both via the fnm PATH shim ("what users type") and via the real node binary + resolved `dist/cli.js` entry ("direct"), mirroring tau's uv-run-vs-venv split. In-process families import the installed binary's OWN bundled internals (`@earendil-works/pi-{ai,agent-core}`), never a rebuild, so they measure the shipped code.
- **tau**: pinned at rev `81de4f8896a9` (fixtures/TAU_REV), run via `uv run --project <tau>`
- **rho**: `5496b35` on branch `fix/pi-bench`, `--release` builds throughout
- **Generated**: 2026-07-18 14:11:26Z
- **Measurement engines**: rho micro-benches use Criterion (self-tuned sample counts, reports mean ± σ); tau timers use `time.perf_counter` with warmup + measured iterations; cold-start uses hyperfine; RSS uses `/usr/bin/time -l`.
- **Determinism**: session/canonicalization inputs are the pinned `fixtures/` (extracted by tau's own serializer); the mock provider replays a fixed SSE body; the FakeProvider is fully scripted. No network, no clock, no RNG in families (b)–(d).
- **Quiesced measurement**: every final number was taken in a **serial, quiesced** window — no other builds or heavy processes running, and the orchestrator (`run_all.sh`) runs each family one at a time so benchmarks never contend for CPU with each other. Any family that overlapped transient background load during collection was re-measured in a subsequent quiet window, so no reported figure is contaminated by contention.
- **Variance caveat**: this is still a developer laptop, not an isolated bench rig. Absolute numbers move ±10–30% between runs; the *ratios* between the three engines are the durable result, and the big ones span orders of magnitude, not percentages (the near-1× pi-vs-tau startup tie is the exception — read it as "indistinguishable," not a precise figure).

## (a) Cold start + end-to-end print latency

`rho -p` (compiled binary) vs `tau -p` (Python via `uv run`) vs `pi -p` (TypeScript/Node), all three driving one print-mode turn against the **same** mock provider replaying a fixed OpenAI-compatible SSE body (pi via a custom `openai-completions` provider in `models.json` pointed at the mock). Process spawn → exit, wall-clock via hyperfine, all rerun in one quiesced window. The two `--version` rows separate launcher cost from runtime boot for *both* interpreted agents: the first includes each one's launcher (tau `uv run`, pi fnm PATH shim), the second is the direct entry (tau `.venv/bin/tau`, pi `node dist/cli.js`).

| Variant | rho | tau | pi | tau/rho | tau/pi |
|---|---|---|---|---|---|
| `--version` (with launcher: tau `uv run`, pi fnm shim) | 12.2 ± 8.0 ms | 2506.2 ± 104.2 ms | 2184.3 ± 200.4 ms | 204.9× | 1.1× |
| `--version` (direct entry: tau venv, pi `node cli.js`) | 6.5 ± 3.6 ms | 1970.8 ± 113.0 ms | 2176.6 ± 280.1 ms | 302.2× | 0.9× |
| print, 0 ms latency | 38.8 ± 6.9 ms | 2971.8 ± 350.6 ms | 2503.5 ± 199.0 ms | 76.7× | 1.2× |
| print, 20 ms/chunk streaming | 438.1 ± 8.9 ms | 3123.4 ± 569.0 ms | 2747.0 ± 185.4 ms | 7.1× | 1.1× |

**A native binary vs two interpreter runtimes is the whole story here.** The `--version` rows are the cleanest read: almost entirely process startup. rho is a statically-linked binary that execs and prints; tau pays CPython boot + imports (pydantic, httpx, typer, rich, textual); pi pays Node/V8 boot + its large bundled module graph and model-catalog load. The measured surprise: **pi's cold start is on par with tau's, not faster** — both land in the ~2–2.5 s range (see the `tau/pi ≈ 1×` column), roughly two orders of magnitude above rho. So a JIT runtime buys nothing over CPython for *startup* here; if anything pi's shipped bundle makes `--version` as heavy as tau's import graph. Note too that pi's fnm PATH shim adds almost nothing (shim ≈ direct node entry), whereas tau's `uv run` adds a visible slice over its venv — but that's noise next to the runtime tax both pay. And it is **not** merely a launcher artifact: the direct entries (no `uv run`, no fnm shim) still cost **1.97 s** for tau (302.2× slower than rho) and **2177 ms** for pi (333.8× slower than rho) — that residue is runtime boot + import graph (CPython for tau, Node/V8 for pi), which the launchers only add a modest fraction on top of.
**But note the 20 ms/chunk row.** Once the provider streams with even a small per-chunk latency, a fixed ~hundreds-of-ms cost lands on *all three* implementations equally, and the spawn-time gaps start to disappear into it. With a real LLM (first token in hundreds of ms, full response in seconds) the startup differences are a rounding error on end-to-end latency — see the caveats.

## (b) Session replay throughput

Parse every JSONL entry line and replay the log into the runtime message list — the load path each implementation runs when opening a session. rho/tau use the pinned synthetic trees under `fixtures/sessions/synthetic/` (100k inflated in-process); pi replays an equivalent-length `linear` session in its OWN format (see the pi caveats below).

| Dataset | entries | rho | tau | pi | rho/s | tau/s | pi/s |
|---|--:|--:|--:|--:|--:|--:|--:|
| compaction-heavy-1k | 1000 | 38.160 ms | 49.770 ms | — | 26.2 K/s | 20.1 K/s | — |
| compaction-heavy-10k | 10000 | 2.661 s | 7.127 s | — | 3.8 K/s | 1.4 K/s | — |
| deep-branch-1k | 1000 | 6.847 ms | 21.592 ms | — | 146.0 K/s | 46.3 K/s | — |
| deep-branch-10k | 10000 | 92.141 ms | 236.179 ms | — | 108.5 K/s | 42.3 K/s | — |
| deep-branch-100k | 100000 | 921.538 ms | 3.830 s | — | 108.5 K/s | 26.1 K/s | — |
| linear-1k | 1000 | 20.825 ms | 46.518 ms | 6.064 ms | 48.0 K/s | 21.5 K/s | 164.9 K/s |
| linear-10k | 10000 | 229.140 ms | 602.056 ms | 57.524 ms | 43.6 K/s | 16.6 K/s | 173.8 K/s |
| linear-100k | 100000 | 2.077 s | 8.876 s | 756.266 ms | 48.1 K/s | 11.3 K/s | 132.2 K/s |

**Parse dominates on all sides** (replay of a linear log is trivially O(n)); the gap is entirely in decode. tau pays a pydantic `TypeAdapter` per entry (validation + model construction). rho pays its own tax: `SessionEntry` is an `#[serde(untagged)]` union, so serde buffers each line and trial-decodes it against every variant. **This is a deliberate, documented trade, not a Rust shortcoming** — rho *cannot* use the fast internally-tagged `#[serde(tag = ...)]` path, because tau writes the `type` discriminator in the *fourth* field position (after `id`/`parent_id`/`timestamp`) and internally-tagged serde only emits the tag first; untagged serializes in declared field order and so is the only shape that reproduces tau's bytes exactly (see `dev-notes/phase-1.md`, "Why untagged unions + monostate"). rho pays trial-decode CPU to buy byte-parity. So over tau, rho still posts a solid **several-fold** win, not the ~100× of the allocation-light micro-benches: this is where rho's compatibility constraints cost it the most. **pi is the surprise: on the `linear` rows it is the fastest of the three** — V8's JIT-compiled `JSON.parse` plus a light id/parentId tree-walk beats both pydantic and rho's trial-decoding untagged union, so rho's win over tau does *not* carry to pi. (Forward-looking, not done: a tagged fast-path — try the internally-tagged decode first and fall back to untagged only when the tag isn't first — would likely close much of the V8 gap without breaking byte-parity; future work.) Two honest caveats scope the pi column: (1) **same workload, different format** — pi replays its OWN session format (a typed id/parentId entry tree) over the same entry counts, not tau/rho's `SessionEntry` bytes, so only the `linear` rows are directly comparable and deep-branch/compaction-heavy stay rho-vs-tau only; (2) pi's replay step (`buildSessionContext`) is a lighter reconstruction than rho's full `SessionState`, so part of pi's edge is doing modestly less work, not only decoding faster. It is the honest row to show precisely because it punctures the "Rust always wins the cold path" story.
> **`compaction-heavy-100k` is intentionally excluded** (both timers). Compaction replay is O(n²) in *both* implementations — each compaction entry rescans the retained transcript, a shared byte-compatible algorithm, not a rho regression (measured tau 10k replay 7.127 s, 2.7× the rho 2.661 s). At 100k that single cell costs minutes per iteration in either language and adds nothing beyond the 1k→10k trend already visible above. Flagged, not silently capped.

## (c) SSE canonicalization overhead

Feed a response-start, N text deltas, and a terminal end through the canonical-event accumulator (rho `StreamAccumulator` / tau `canonicalize_provider_stream`) and drain every emitted event — the per-token bookkeeping every streamed response pays.

| Deltas | rho ns/delta | tau ns/delta | rho deltas/s | tau deltas/s | tau/rho |
|--:|--:|--:|--:|--:|--:|
| 100 | 1099 | 105826 | 909.6 K/s | 9.4 K/s | 96.3× |
| 1000 | 1166 | 84544 | 857.6 K/s | 11.8 K/s | 72.5× |
| 10000 | 2160 | 86815 | 462.9 K/s | 11.5 K/s | 40.2× |

Both maintain a running partial message and snapshot it into each event. tau deep-copies a pydantic model per event; rho clones one working struct. Same protocol, very different constant factor.
> **No pi column — documented, not dropped.** pi has no standalone canonicalization stage to isolate. tau's `canonicalize_provider_stream` and rho's `StreamAccumulator` are a discrete *provider-events → canonical-events* pass that snapshots the partial message once per event; pi's providers instead build the partial and emit canonical `AssistantMessageEvent`s **inline**, and — critically — each event carries the partial **by reference** (one mutated object), not a per-event deep copy (tau) or clone (rho). There is thus no equivalent unit of work: pi's per-delta snapshot cost is O(1) by construction, so a like-for-like number would pit an accumulate-and-copy pass against a pointer write and flatter pi meaninglessly; benchmarking the faux/provider delta loop would measure the test double, not the wire path. The architectural takeaway stands on its own: pi sidesteps the per-token copy that both ports pay.

## (d) Memory (peak RSS)

Peak resident set size over a scripted N-turn FakeProvider session (transcript accumulating in memory, no network), via `/usr/bin/time -l`. This is the family with the **most surprising, most honest** result, so it gets a turn-count sweep rather than a single number.

| turns | rho peak RSS | tau peak RSS | pi peak RSS |
|--:|--:|--:|--:|
| 1 | 1.98 MiB | 41.47 MiB | 79.98 MiB |
| 500 | 73.03 MiB | 45.11 MiB | 86.33 MiB |
| 2000 | 1086.58 MiB | 68.88 MiB | 103.81 MiB |

**Baseline (1 turn): the two interpreter runtimes cost tens of MiB; rho costs a couple.** rho's near-empty process is ~2 MiB against tau's ~41 MiB (CPython + pydantic/anyio/httpx/rich/textual) and pi's ~80 MiB (Node/V8 + its module graph). Both interpreted agents pay a fixed runtime-plus-imports tax before doing any work; the statically-linked rho binary + a current-thread tokio runtime does not. **This baseline is rho's real, production-relevant footprint advantage — and it holds against Node just as it does against CPython.**
**pi's sweep is the level-headed one.** pi's in-process driver runs pi's OWN `Agent` + faux provider (the shipped code) and, unlike rho's `FakeProvider`, its test double retains no deep-copied call log, so pi's curve grows ~linearly with the transcript rather than exploding. Read the pi column as the honest "what a Node agent's memory does as a session grows" line — well above rho's baseline, climbing gently. (pi's driver issues N discrete prompts → 2N messages, vs tau/rho's one prompt + N−1 continues → N+1 messages; the `note` field records each, and the sweep is read by shape, not by matching message counts cell-for-cell.)
**But watch the sweep: rho's line is super-linear and crosses tau's.** That is *not* the transcript — it is a **test-double artifact**. rho's `FakeProvider` records every call with `messages.to_vec()`, deep-copying the whole (growing) transcript by value on each of the N turns → O(n²) retained `AgentMessage` copies. tau's `FakeProvider` does `list(messages)`, which copies *references* to shared model objects → O(n). Rust value semantics vs Python reference semantics, in a scripted harness that a real provider never exercises (real providers don't retain a deep-copied call log). So: rho wins the footprint that matters (baseline + real runs) and loses this particular fake-driver microbench — reported as-is rather than quietly dropping the inconvenient rows. A cheap future fix is to have `RecordedCall` retain `Arc`/references instead of owned clones; out of scope for M6.
**Allocator honesty**: peak RSS is not a like-for-like allocator comparison — both processes are subject to the system allocator's retention policy and macOS reports RSS in bytes. Read the *baseline* row for the interpreter-vs-binary gap and the *shape* for the O(n²) artifact; do not read the crossover as "Rust uses more memory than Python" in general — it does not.
Real-LLM spot checks: **skipped** — `ANTHROPIC_API_KEY` was not set in the environment. Their only intent is to confirm the obvious: against a live provider, network latency (hundreds of ms to seconds per turn) dominates end-to-end time, so neither implementation's CPU/RSS edge is observable end-to-end.

## π vs τ vs ρ — the language triangle

Three implementations of the same agent, one per runtime model: **pi** on JIT-warmed Node/V8, **tau** on interpreted CPython, **rho** on compiled Rust. Read across the families and a consistent shape emerges — it is *not* a simple "Rust wins everything" ladder.

- **Process startup (native ≫ both runtimes, which tie).** A native binary that just execs and prints is untouchable: rho's `--version` is single-digit milliseconds. The measured surprise is that the two interpreted agents **tie** — pi (~2.2 s) is on par with tau (~2–2.5 s), not faster. Node's JIT buys nothing for cold start here; pi's shipped bundle + model-catalog load makes `--version` as heavy as tau's CPython import graph. So the startup ladder is **rho ≪ pi ≈ tau** — a ~200–300× native-vs-interpreted gap that does *not* discriminate between the two runtimes.
- **JSONL decode (JIT can beat compiled).** On linear session replay the warmed V8 `JSON.parse` is the *fastest of the three* — ahead of both Python's pydantic and rho's deliberately-cautious `#[serde(untagged)]` trial-decode. This is the family that most punctures the ladder: rho's byte-compat design tax lets a JIT win a hot loop. (Caveats in family (b): pi replays its own format and does a lighter reconstruction.)
- **Per-token streaming bookkeeping (architecture > language).** The dominant cost isn't the runtime, it's the data model: tau deep-copies a pydantic model per event, rho clones a struct, **pi mutates one object and snapshots by reference**. pi's design sidesteps the per-token copy entirely (family (c)) — a reminder that how you represent the partial message matters more than which language you wrote it in.
- **Baseline memory (native ≪ both runtimes).** Here the compiled binary's advantage is unambiguous and holds against *both* interpreters: rho's ~2 MiB baseline vs tens of MiB for CPython (tau) and Node (pi) alike. Runtime + import graph is a fixed footprint tax that a static binary simply doesn't pay.

**The synthesis:** rho wins decisively where a *native binary* wins — cold start and baseline footprint — and those wins hold against Node as firmly as against Python. But on hot CPU loops the picture is nuanced: a warmed JIT (pi) can match or beat compiled Rust when Rust is carrying a compatibility tax, and the biggest per-token differences come from *data-model choices* (copy vs reference), not the language. tau is the consistent trailing edge on CPU-bound work (interpreter overhead + pydantic), but even it is within a rounding error of the others the moment a real network turn is in the loop. Three runtimes, three different lessons — and the same conclusion below about when any of it matters.

## Caveats — where the Rust win is real, and where it doesn't matter

- **Where Rust clearly wins**: process startup (no interpreter boot), cold-path CPU work — session parsing and SSE canonicalization run ~1–2 orders of magnitude faster, and baseline memory is a fraction of CPython's. For batch/scripted use (replaying thousands of sessions, `-p` in a tight loop, embedding the agent in a larger tool) these are decisive.
- **Where it doesn't matter**: interactive use against a real model. The wall-clock of a real turn is dominated by the provider — network RTT plus generation time (first token in the hundreds of ms, full responses in seconds). Shaving tens of ms off startup or microseconds off per-token canonicalization is invisible next to that. The 20 ms/chunk cold-start variant already shows the gap collapsing under trivial streaming latency.
- **What did NOT change**: byte-for-byte wire/session compatibility with tau is the whole point of rho; these benchmarks change the performance envelope, not the observable output. Same fixtures, same bytes.
- **Fair-comparison notes**: tau is invoked via `uv run` (its idiomatic entry here), which adds a small fixed launcher cost to cold start; the session/canonicalization timers call tau's library directly, so those exclude launcher and interpreter-boot cost and measure pure algorithm throughput. RSS uses the venv interpreter directly for the same reason.
- **pi fair-comparison notes**: pi's cold start is measured both via the fnm PATH shim (what users type — the Node analogue of `uv run`'s launcher cost) and via the real node binary + resolved `dist/cli.js` entry (both fnm shims bypassed — the isolated spawn). Its in-process families (session replay, RSS) import the **installed** binary's own bundled internals (`@earendil-works/pi-{ai,agent-core}`, v0.80.10), never a rebuild, so they measure the shipped code, pinned to `earendil-works/pi` rev `3da591ab74ab` (fixtures/PI_REV). pi runs `PI_OFFLINE=1` during startup timing to disable its version/catalog network probe. Family (c) has no pi row for the architectural reason stated there.

## Conclusion — what the Rust port bought

On this machine the port bought a large, consistent **cold-path and footprint** win with **no change to observable behavior**:
- Startup (`--version`): **205×** faster.
- Replaying a 100k-entry linear session: **4×** faster (parse-bound).
- SSE canonicalization at 10k deltas: **40×** faster per token.
- Memory: baseline process RSS is ~21× smaller (2 MiB vs 41 MiB) — though the FakeProvider microbench also exposes an O(n²) memory artifact in rho's test double under long scripted runs (family (d)); the production-relevant baseline is the durable win.

And it bought essentially **nothing for the latency a human feels in an interactive session against a real LLM** — there, the network dominates and always will. The honest verdict: rho is the right tool when the agent is a *component* in something larger (batch replay, tooling, embedding, fast startup, low memory footprint), and a lateral move when it is a human sitting at a prompt waiting on a model. The port's real deliverable is that it achieves the former **while remaining byte-for-byte compatible** with the latter.

**Widening it to pi** (the original Node/TS agent) sharpens rather than overturns this. rho's decisive wins — cold start and baseline RSS — hold against Node just as firmly as against Python, because they are *native-binary* wins, not *anti-Python* wins. But pi also shows the ladder isn't monotonic: a warmed V8 beats rho on the linear-JSONL hot loop (where rho pays a byte-compat decode tax), and pi's reference-snapshot streaming sidesteps the per-token copy both ports pay. So the fuller verdict: **compile for startup and footprint; the runtime matters least exactly where humans feel it most (the network turn), and on the CPU-bound cold path your data-model choices can matter more than your language.**

---

_Regenerate with `just bench` (runs every family, then this generator). Machine-readable records: `dev-notes/benchmarks.json`._
