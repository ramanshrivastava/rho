# rho — contributor & agent guide

`rho` is a full-parity Rust port of [`tau`](https://github.com/huggingface/tau), a
minimalist Pi-style coding-agent harness written in Python. The north star is
**byte-for-byte compatibility** with tau's JSONL wire and session format: a rho
session file must be indistinguishable from a tau session file, and vice-versa.

This file is identical to `CLAUDE.md`. Read it before touching anything.

## Workspace layout

| Crate            | Ports tau package        | Role |
|------------------|--------------------------|------|
| `rho-agent`      | `tau_agent`              | messages, events, provider trait, tools, loop, harness, session |
| `rho-ai`         | `tau_ai`                 | six provider adapters, http/sse, retry, canonicalization |
| `rho-coding`     | `tau_coding`             | coding tools, `CodingSession`, commands, skills, catalog, oauth, export, rendering |
| `rho-tui`        | `tau_coding.tui`         | ratatui app |
| `rho-ext-host`   | `tau_coding.extensions`  | wasmtime host (stub until M7) |
| `rho-ext-api`    | `tau_coding.extensions.api` | extension guest API (stub until M7) |
| `rho` (binary)   | `tau_coding.cli`         | the `rho` command |

## Layering rules (hard constraints)

- `rho-agent` MUST NOT depend on `rho-ai` or `rho-coding`. It is the provider- and
  UI-neutral core.
- `rho-ai` depends only on `rho-agent`.
- `rho-coding` depends on `rho-ai` + `rho-agent`.
- `rho-tui` and the `rho` binary sit on top of `rho-coding`.

Cargo's acyclic dependency graph *enforces* this at compile time — which is the
point. tau carried a documented `tau_agent` ↔ `tau_ai` import cycle (the canonical
event classes live in `tau_agent` and are re-exported from `tau_ai`); expressing
the split as separate crates makes the cycle impossible to reintroduce.

## Fixture policy (read this twice)

`fixtures/` is the **correctness oracle**, extracted *by tau's own serialization
code* and pinned to a specific tau revision in `fixtures/TAU_REV`.

- `fixtures/` is **read-only**. Do not hand-edit a fixture. Ever.
- Regenerate only via `just refresh-fixtures`, and only alongside a deliberate
  `fixtures/TAU_REV` bump (with a note on what tau change motivated it).
- **If a golden test diffs, the CODE is wrong, not the fixture.** The fixture is
  what tau emits; rho's job is to match it. Treat a diff as a bug in rho until
  proven otherwise.

Fixture directories:

- `wire/` — one file per message/entry/event variant, tau's exact serialized bytes.
- `wire-legacy/` — hand-crafted Tau-v1 JSONL + `.expected.json` (post-migration).
- `event-streams/` — scripted agent-loop runs (agent + assistant event sequences +
  the fake-provider script to replay).
- `sessions/` — real session JSONL (linear/branched/compaction/kitchen-sink/legacy)
  plus `synthetic/` benchmark trees (1k/10k/100k; 100k gzipped).
- `sse/` — per-provider request payload + raw SSE body + canonical event sequence.
- `export/` — an HTML session export golden.

## Byte-compat policy summary

Everything below is load-bearing; the M0 extraction confirmed each against tau:

- Transcript messages/events serialize with **camelCase** aliases; session-entry
  top-level fields are **snake_case**. Both casings appear on the *same* JSONL line
  (a `MessageEntry` has snake_case `id`/`parent_id`/`type` wrapping a camelCase
  `message`).
- `None` fields are **omitted** from wire output (pydantic `exclude_none`). But
  `exclude_none` does **not** recurse into free-form JSON values — a literal
  `null` inside `arguments`/`details`/`data`/`custom` payloads is preserved.
- Floats never use scientific notation: tau writes `0.00005`, not `5e-05`. Cost
  fields serialize as floats (`0.0`); token counts as ints (`0`).
- Message `timestamp` is int **milliseconds**; session-entry `timestamp`/`createdAt`
  are **floats** (and whole-number floats must stay `1700000000.0`, not `1700000000`).
- The `_to_camel` alias generator title-cases every non-first underscore segment,
  so `cache_write_1h` → `cacheWrite1H`.
- Session storage uses `exclude_none`; `export_session_jsonl` does **not** (it
  writes nulls). Do not conflate the two paths.

## Commands

```sh
just test         # cargo test --workspace
just lint         # cargo clippy --workspace --all-targets -- -D warnings  +  cargo fmt --check
just refresh-fixtures   # re-extract golden fixtures from the pinned tau rev
just crosscheck   # run the tau/rho differential harness (tau side only until M4a)
```

CI (`.github/workflows/ci.yml`) runs fmt check, clippy `-D warnings`, and
`cargo test --workspace` on ubuntu-latest + macos-latest.

## Pull request reviews

Every PR is reviewed by two automated bots: **Codex**
(`chatgpt-codex-connector`) and **CodeRabbit** (`coderabbitai`). They post inline
and summary comments, often flagging real parity bugs.

**Before any merge, every bot comment must be resolved — one of:**

- **Fixed** — address it, then reply on that comment's thread with the fix commit
  SHA.
- **Rebutted** — reply on the thread with concrete evidence for why it does *not*
  apply (e.g. a `grep` over `tau/src` showing tau doesn't do the thing either).
  Byte-compat with tau is the arbiter: matching tau's *actual* behavior beats a
  plausible-sounding suggestion. Where a rebuttal reflects a deliberate divergence,
  also record it in the relevant `dev-notes/phase-N.md`.

A bot being rate-limited or slow is not a waiver: re-check the PR for
newly-posted comments before merging. Do not merge with unresolved bot threads.

## Dev-notes journal

Each milestone gets a teaching journal entry in `dev-notes/phase-N.md` (voiced like
tau's `dev-notes/architecture/phase-*.md`): what was built, why, and any tau
behavior discovered that later milestones must respect. Start there to understand a
subsystem's rationale.
