<p align="center">
  <img src="docs/assets/rho-header.svg" alt="rho — a coding-agent harness, oxidized. π → τ → ρ" width="100%" />
</p>

<p align="center">
  <strong>A small, fast terminal coding agent in Rust — a byte-compatible port of <a href="https://github.com/huggingface/tau">tau</a>.</strong>
</p>

<p align="center">
  <a href="https://github.com/huggingface/tau">tau (Python)</a>
  ·
  <a href="https://pi.dev/">Pi (the original)</a>
  ·
  <a href="dev-notes/">Dev journal</a>
  ·
  <a href="fixtures/">Golden fixtures</a>
</p>

---

## What is rho?

**rho is a coding agent that lives in your terminal**, written in Rust. It is a
full-parity port of [tau](https://github.com/huggingface/tau), the Python
teaching implementation of [Pi](https://pi.dev/)'s minimalist coding-agent
architecture.

The lineage is the name: Pi is **π**. tau is **τ = 2π** ("twotimespi"). rho is
**ρ** — the Greek *r*, for Rust. In physics ρ is *density*: the same agent,
compiled.

```text
π  →  τ  →  ρ
```

Like tau, rho is meant to be **read**. Every milestone ships with a
[dev-notes](dev-notes/) journal entry explaining which Rust idioms replaced
which Python patterns, and why — serde tagged unions for Pydantic models,
`Stream`s for async generators, traits for Protocols, wasmtime for a plugin
system Python gets for free.

## Byte-compatible, not just similar

rho reads and writes **tau's exact wire format**. A session started in tau
resumes in rho and vice versa. This is enforced, not aspirational: the
[fixtures/](fixtures/) directory contains golden files extracted from tau's own
serialization code (pinned at [`fixtures/TAU_REV`](fixtures/TAU_REV)), and every
wire type must round-trip **byte-identically** in CI.

This compatibility is delivered **milestone by milestone** — see [Status](#status)
below for exactly what is covered today. The wire types (M1) and the agent
loop / session state (M2) are byte-golden now; provider I/O, the coding tools,
and the full CLI land in later milestones, so end-to-end tau session resume is
not yet available from the `rho` binary.

## Architecture

Three layers with a strict dependency direction, enforced by Cargo's acyclic
crate graph:

```text
rho (bin) → rho-tui → rho-coding → rho-ai → rho-agent
```

| Crate | Role |
|---|---|
| [`rho-agent`](crates/rho-agent) | The portable brain: messages, events, agent loop, harness, session tree |
| [`rho-ai`](crates/rho-ai) | Provider adapters over raw HTTP/SSE — no vendor SDKs |
| [`rho-coding`](crates/rho-coding) | The application: coding tools, sessions, commands, skills, compaction |
| [`rho-tui`](crates/rho-tui) | Interactive terminal UI (ratatui) |
| [`rho-ext-host`](crates/rho-ext-host) / [`rho-ext-api`](crates/rho-ext-api) | Sandboxed WASM extension system (wasmtime) |
| [`rho`](crates/rho) | The `rho` binary: CLI, print mode, TUI wiring |

## Status

Ported milestone by milestone, gated on golden-fixture parity with tau:

| Milestone | Scope | Status |
|---|---|---|
| M0 | Workspace scaffold + golden fixtures extracted from tau | ✅ |
| M1 | Wire types with byte-identical serde | ✅ |
| M2 | Agent loop, harness, session tree, fake provider | 🚧 |
| M3 | All six providers (anthropic, openai-compatible, codex, google, mistral, fake) | ⏳ |
| M4 | Coding tools, print-mode CLI, full `CodingSession` | ⏳ |
| M5 | ratatui TUI (parity with tau's Textual TUI) | ⏳ |
| M6 | Benchmarks: rho vs tau (cold start, replay, streaming, memory) | ⏳ |
| M7 | WASM extensions | ⏳ |

## Development

```bash
just test        # cargo test --workspace
just lint        # clippy -D warnings + fmt --check
just crosscheck  # run identical sessions through tau and rho, diff the bytes
```

Conventions live in [AGENTS.md](AGENTS.md). The one rule that matters most:
**fixtures are read-only** — if a golden test diffs, the code is wrong, not the
fixture.

## License

MIT, like tau before it and Pi before that.
