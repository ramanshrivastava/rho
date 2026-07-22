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

This compatibility was delivered **milestone by milestone** and the port is now
complete (M0–M7 — see [Status](#status)). The wire types, agent loop, session
state, all six providers, the coding tools, the full CLI, and the ratatui TUI
are byte-golden against tau in CI: a session started in tau resumes end-to-end
in the `rho` binary and vice versa, today.

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

Ported milestone by milestone, gated on golden-fixture parity with tau. **All
milestones are complete** — each links to its dev-notes journal entry:

| Milestone | Scope | Status |
|---|---|---|
| [M0](dev-notes/phase-0.md) | Workspace scaffold + golden fixtures extracted from tau | ✅ |
| [M1](dev-notes/phase-1.md) | Wire types with byte-identical serde | ✅ |
| [M2](dev-notes/phase-2.md) | Agent loop, harness, session tree, fake provider | ✅ |
| [M3](dev-notes/phase-3.md) | All six providers (anthropic, openai-compatible, codex, google, mistral, fake) | ✅ |
| [M4](dev-notes/phase-4a.md) | Coding tools, print-mode CLI, full `CodingSession` | ✅ |
| [M5](dev-notes/phase-5.md) | ratatui TUI (parity with tau's Textual TUI) | ✅ |
| [M6](dev-notes/phase-6.md) | Benchmarks: rho vs tau vs pi (cold start, replay, streaming, memory) | ✅ |
| [M7](dev-notes/phase-7.md) | WASM extensions (wasmtime host + guest API) | ✅ |

## Providers & sign-in

rho talks to six providers over raw HTTP/SSE (no vendor SDKs): `anthropic`,
`openai-compatible`, `codex`, `google`, `mistral`, and the scripted `fake`. Set
an API key the usual way, or sign in with an existing **subscription** through
OAuth from inside the TUI with `/login [provider]`:

- **OpenAI Codex** (ChatGPT subscription) — browser authorization-code + PKCE
- **Anthropic** (Claude Pro/Max) — browser authorization-code + PKCE
- **GitHub Copilot** — device-code flow (enter the code at github.com)

Credentials are stored in `~/.rho/credentials.json` (mode `0600`, same on-disk
format tau writes), refreshed automatically, and removed with `/logout`.

## Benchmarks

Three implementations of the same agent, one per runtime model — **pi**
(TypeScript/Node), **tau** (Python), **rho** (Rust) — measured on one machine.
rho wins decisively where a *native binary* wins; the full tables, methodology,
and honest caveats (including where a warmed JIT beats compiled Rust) are in
[dev-notes/benchmarks.md](dev-notes/benchmarks.md).

| Metric | rho | tau | pi |
|---|--:|--:|--:|
| Cold start (`--version`, direct entry) | **6.5 ms** | 1971 ms | 2177 ms |
| Peak RSS, baseline (1 turn) | **1.98 MiB** | 41.47 MiB | 79.98 MiB |
| SSE canonicalization (per-delta) | **40× faster than τ** | 1× | — |

*A native binary vs two interpreter runtimes is the whole story on cold start
and baseline memory — a ~300× and ~20× gap that holds against Node as firmly as
against CPython.*

## Install

> Available from **v0.1.0**.

```bash
# Homebrew (macOS / Linux)
brew install ramanshrivastava/tap/rho

# Cargo — the crate is `rho-code`; it installs a binary named `rho`
cargo install rho-code

# Cargo, straight from git
cargo install --git https://github.com/ramanshrivastava/rho rho-code
```

Prebuilt binaries for macOS and Linux are attached to each
[GitHub Release](https://github.com/ramanshrivastava/rho/releases). (The bare
`rho` name on crates.io is squatted, hence the `rho-code` crate — the installed
command is still `rho`.)

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
