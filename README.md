<p align="center">
  <img src="docs/assets/rho-header.svg" alt="rho — a Rust coding agent, oxidized. π → τ → ρ" width="100%" />
</p>

<p align="center">
  <strong>rho</strong> — a Rust coding agent, oxidized.
</p>

<p align="center">
  <a href="https://pi.dev/">Pi</a> (TypeScript)
  &nbsp;→&nbsp;
  <a href="https://github.com/huggingface/tau">tau</a> (Python)
  &nbsp;→&nbsp;
  <strong>ρ rho</strong> (Rust) — the same minimalist coding agent, compiled.
</p>

<p align="center">
  <a href="https://crates.io/crates/rho-code"><img src="https://img.shields.io/crates/v/rho-code?logo=rust&label=crates.io&color=B3391F" alt="crates.io" /></a>
  <a href="https://github.com/ramanshrivastava/rho/releases/latest"><img src="https://img.shields.io/github/v/release/ramanshrivastava/rho?logo=github&color=B3391F" alt="GitHub release" /></a>
  <a href="https://github.com/ramanshrivastava/rho/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/ramanshrivastava/rho/ci.yml?branch=main&logo=github&label=CI" alt="CI status" /></a>
  <a href="#lineage--license"><img src="https://img.shields.io/crates/l/rho-code?color=B3391F" alt="MIT license" /></a>
</p>

<!-- Hero: the running TUI splash, copied from the rho-tui snapshot test.
     A real screenshot or animated GIF of the live TUI can replace this block later. -->

```text
                                        ρ
                             ρω   ·   r h o   ·   रो

               π Pi·TypeScript   →   τ tau·Python   →   ρ rho·Rust

                          a Rust coding agent, oxidized
                     ├─ ~302× faster cold start than τ
                     ├─ ~21× lighter
                     └─ ~40× faster stream canonicalization

 / commands  ·  Ctrl+P model  ·  Ctrl+R sessions  ·  !cmd shell  ·  Ctrl+D quit

           · did you know — ρ reads and writes τ's exact session files
```

**rho is a coding agent that lives in your terminal**, written in Rust. It is a
full-parity port of [tau](https://github.com/huggingface/tau), the Python
teaching implementation of [Pi](https://pi.dev/)'s minimalist coding-agent
architecture — and it reads and writes **tau's exact wire format**, byte for
byte, so a session started in tau resumes in rho and vice versa.

The lineage is the name. Pi is **π**. tau is **τ = 2π** ("twotimespi"). rho is
**ρ** — the Greek *r*, for Rust; in physics ρ is *density*: the same agent,
compiled.

## Install

The installed command is always **`rho`**; `rho-code` is only the package name
(the bare `rho` crate name is squatted).

```bash
# Homebrew (macOS / Linux)
brew install ramanshrivastava/tap/rho
```

```bash
# crates.io — builds and installs a binary named `rho`
cargo install rho-code
```

```bash
# Prebuilt binary via the release installer (macOS / Linux)
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ramanshrivastava/rho/releases/download/v0.1.0/rho-code-installer.sh | sh
```

```bash
# Latest HEAD, straight from git
cargo install --git https://github.com/ramanshrivastava/rho rho-code
```

Prebuilt binaries for macOS (arm64/x86_64) and Linux (x86_64) are attached to
every [GitHub Release](https://github.com/ramanshrivastava/rho/releases).

## Headline numbers

Three implementations of the same agent, one per runtime model — **pi**
(TypeScript/Node), **tau** (Python), **rho** (Rust) — measured on one machine.
rho wins decisively where a *native binary* wins:

| Metric | rho | tau | pi |
|---|--:|--:|--:|
| Cold start — `--version`, direct entry | **6.5 ms** | 1970.8 ms | 2176.6 ms |
| Peak RSS — baseline, 1 turn | **1.98 MiB** | 41.47 MiB | 79.98 MiB |
| SSE canonicalization — 10k deltas, per-delta | **2160 ns** | 86815 ns | — |

One M4 Pro laptop, means shown — absolute numbers move ±10–30% run to run, so
the *ratios* are the durable result. pi has no isolated canonicalization pass,
so that row is rho-vs-tau only (`—`). The full four-family study, methodology,
and the honest caveats — where a warmed JIT *beats* rho on the JSONL hot loop,
and a FakeProvider O(n²) memory artifact — live in
[dev-notes/benchmarks.md](dev-notes/benchmarks.md).

## Features

- **Six providers, no vendor SDKs.** `anthropic`, `openai-compatible`, `codex`,
  `google`, `mistral`, and the scripted `fake`, all spoken over raw HTTP/SSE.
- **Subscription OAuth sign-in** with `/login [provider]`: OpenAI Codex (ChatGPT)
  and Anthropic (Claude Pro/Max) via browser authorization-code + PKCE, and
  GitHub Copilot via device-code. Credentials persist to
  `~/.rho/credentials.json` (mode `0600`, tau's exact on-disk format), refresh
  automatically, and are removed with `/logout`.
- **Byte-compatible sessions.** rho reads and writes tau's exact JSONL wire and
  session format — sessions cross between the two runtimes unchanged.
- **A ratatui TUI at parity with tau's Textual TUI:** the welcome splash,
  bottom-anchored transcript, blinking cursor, scrollback, model/session
  pickers, slash-commands, and a shell escape.
- **Sandboxed WASM extensions** via a wasmtime host + guest API — off by
  default; build with `--features wasmtime`.

## Byte-compatible, not just similar

This is the soul of the project: rho reads and writes **tau's exact wire
format**, and that is enforced, not aspirational.

The [fixtures/](fixtures/) directory holds golden files extracted *by tau's own
serialization code*, pinned to a specific tau revision in
[`fixtures/TAU_REV`](fixtures/TAU_REV). Every wire type must round-trip
**byte-identically** against those fixtures in CI. The fixtures are the
**correctness oracle** — read-only by policy: if a golden test diffs, the code
is wrong, not the fixture.

Byte-parity is unforgiving in the details, and reproducing it is most of the
work: transcript messages serialize with camelCase aliases while session-entry
fields are snake_case (both on the *same* JSONL line); `None` fields are omitted
except inside free-form JSON payloads; floats never use scientific notation
(`0.00005`, not `5e-05`); message timestamps are integer milliseconds while
session timestamps are whole-number floats. Delivered milestone by milestone
(M0–M7), the port is complete: wire types, agent loop, session state, all six
providers, the coding tools, the full CLI, and the ratatui TUI are byte-golden
against tau today.

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

`rho-agent` must not depend on `rho-ai` or `rho-coding` — the acyclic graph makes
the layering impossible to violate at compile time. (tau carried a documented
`tau_agent` ↔ `tau_ai` import cycle; the crate split makes it unreconstructable.)

<details>
<summary><strong>Milestones M0–M7</strong> — the port shipped gated on golden-fixture parity (all complete)</summary>

<br />

| Milestone | Scope | Status |
|---|---|---|
| [M0](dev-notes/phase-0.md) | Workspace scaffold + golden fixtures extracted from tau | ✅ |
| [M1](dev-notes/phase-1.md) | Wire types with byte-identical serde | ✅ |
| [M2](dev-notes/phase-2.md) | Agent loop, harness, session tree, fake provider | ✅ |
| [M3](dev-notes/phase-3.md) | All six providers (anthropic, openai-compatible, codex, google, mistral, fake) | ✅ |
| [M4](dev-notes/phase-4a.md) | Coding tools, print-mode CLI, full `CodingSession` | ✅ |
| [M5](dev-notes/phase-5.md) | ratatui TUI (parity with tau's Textual TUI) | ✅ |
| [M6](dev-notes/phase-6.md) | Benchmarks: rho vs tau vs pi (cold start, replay, streaming, memory) | ✅ |
| [M7](dev-notes/phase-7.md) | WASM extensions (wasmtime host + guest API) — off by default; build with `--features wasmtime` | ✅ |

Each milestone ships with a [dev-notes](dev-notes/) journal entry explaining
which Rust idioms replaced which Python patterns, and why.

</details>

## Development

```bash
just test        # cargo test --workspace
just lint        # clippy -D warnings + fmt --check
just crosscheck  # run identical sessions through tau and rho, diff the bytes
```

Conventions live in [AGENTS.md](AGENTS.md). The one rule that matters most:
**fixtures are read-only** — if a golden test diffs, the code is wrong, not the
fixture.

## Lineage & license

rho stands on [tau](https://github.com/huggingface/tau) (Python), which is a
teaching port of [Pi](https://pi.dev/) (TypeScript). Same architecture, three
runtimes — π → τ → ρ.

MIT, like tau before it and Pi before that.
