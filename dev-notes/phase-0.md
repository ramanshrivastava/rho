---
title: "Phase 0: Scaffold and the Fixture Oracle"
---

Phase 0 does not port a single line of tau's behavior. It builds the workspace
that the port will grow inside, and — far more importantly — it freezes tau's
observable output into a corpus of golden fixtures that every later milestone will
be graded against.

The bet is simple: **if you are going to reimplement a serializer in another
language, extract the reference outputs first, from the reference implementation's
own code, before you write the replacement.** Otherwise you spend the whole port
arguing with yourself about what "correct" means. With fixtures, "correct" is a
file on disk.

## What M0 built

Two things.

1. A Cargo workspace of seven crates, each compiling with a doc comment describing
   its future role and nothing else:

   ```text
   crates/rho-agent      messages, events, provider trait, tools, loop, harness, session
   crates/rho-ai         provider adapters, http/sse, retry, canonicalization
   crates/rho-coding     coding tools, CodingSession, commands, skills, export, rendering
   crates/rho-tui        ratatui app
   crates/rho-ext-host   wasmtime host (empty stub)
   crates/rho-ext-api    extension guest API (empty stub)
   crates/rho            the `rho` binary (clap skeleton: prints version, -p is a stub)
   ```

2. A fixture-extraction toolchain under `tools/extract-fixtures/` that runs against
   a pinned tau checkout and writes 173 golden files into `fixtures/`, plus a
   crosscheck skeleton under `tools/crosscheck/`.

## Why fixtures-first

tau's wire format is not big, but it is *fiddly* — a dozen small decisions that are
individually easy to get subtly wrong and collectively define byte-compatibility.
The only authority on those decisions is tau's own serialization code. So the
extraction scripts do not hand-write expected JSON; they import tau and call the
exact functions tau uses in production:

- `WireModel.model_dump_json(by_alias=True, exclude_none=True)` for messages,
  events, usage, and tool results.
- `entry_to_json_line` (tau's `JsonlSessionStorage` append path) for session
  entries.
- `render_session_html` for the HTML export.
- Real provider adapters driven against canned SSE bodies through an
  `httpx.MockTransport`, capturing the request payload, the raw SSE, and the
  canonical event sequence tau produces.

A fixture that a human typed is a fixture that encodes a human's *belief* about
tau. A fixture tau printed is ground truth.

### Determinism

Golden files are worthless if `just refresh-fixtures` produces different bytes each
run. tau stamps three kinds of non-determinism: millisecond message timestamps,
float session-entry timestamps + random UUID entry ids, and an HTML "generated at"
instant. `_common.patch_determinism()` freezes all of them at their definition
sites — patching the *modules* (`messages.time`, `entries.time`, `entries.uuid4`,
`session_export.datetime`) rather than the pydantic `default_factory` callables,
because pydantic has already captured those factory references and re-looks-up
their module globals at call time. We ran the whole extraction twice and diffed:
byte-identical.

## Why the crate split (and how it fixes a real tau wart)

tau has a documented circularity: the canonical Pi event classes (`TextDeltaEvent`
and friends) live in `tau_agent.provider_events`, but they are the vocabulary the
`tau_ai` provider layer speaks, so `tau_ai` re-exports them — and a test literally
asserts `tau_ai` does not `import tau_ai` back into `tau_agent` to keep the cycle
from closing. It is held apart by discipline.

In rho the same split is a Cargo dependency edge: `rho-ai` depends on `rho-agent`,
never the reverse. Cargo rejects a dependency cycle at build time, so the layering
tau maintains by convention, rho maintains by construction. The event types will
live in `rho-agent` and `rho-ai` will simply `pub use` them.

## Discoveries from extraction (that later milestones must respect)

The extraction surfaced several byte-level facts. Each is now pinned by a fixture;
each is a place the Rust serializer can go wrong.

- **Mixed casing on one line.** Transcript models serialize with a camelCase alias
  generator; session entries do not (their fields stay snake_case). A
  `MessageEntry` therefore writes snake_case `id`/`parent_id`/`type` wrapping a
  camelCase `message` object. Both casings, same line. See
  `fixtures/wire/entries/message_entry.json`.

- **`exclude_none` does not recurse.** `None` model *fields* are dropped, but a
  literal `null` living inside a free-form JSON value — tool-call `arguments`,
  `details`, `data`, custom payloads — is preserved. `arguments:{"b":null}` stays.
  The Rust port must apply skip-if-none to struct fields only, never to
  `serde_json::Value` interiors. See `wire/content/tool_call_nested_args.json`.

- **No scientific notation for floats.** tau writes `0.00005`; Python's own
  `json.dumps` would write `5e-05`. This bit the extractor's first self-check.
  serde_json's default float formatting matches tau here, but it is worth an
  explicit fixture (`wire/usage/usage_full.json`) so a future formatting change
  can't slip through.

- **`cacheWrite1H`.** tau's `_to_camel` title-cases every non-leading underscore
  segment, and `"1h".title() == "1H"`, so `cache_write_1h` aliases to
  `cacheWrite1H`. A naive camelizer that only upper-cases the first letter of
  alphabetic segments gets this wrong.

- **Int-ms vs float timestamps.** Message `timestamp` is an integer count of
  milliseconds; entry `timestamp`/`createdAt` are floats. And a whole-number float
  must serialize as `1700000000.0`, not collapse to `1700000000` — otherwise it
  would round-trip as an int and mis-type on the next read.

- **Two JSONL write paths that disagree on nulls.** `entry_to_json_line` (session
  storage) uses `exclude_none`; `export_session_jsonl` uses a plain
  `model_dump_json` that *writes* nulls. rho must keep these as two distinct paths,
  not one shared "serialize an entry" helper.

- **Legacy migration is a persistence-boundary concern.** `_migrate_message`
  rewrites Tau-v1 shapes (a `role:"tool"` message → `toolResult`; string assistant
  content + `tool_calls` → content blocks; `usage.cost: null` → `{}`; a `user`
  message carrying `custom_type` → a `custom` message) only when reading persisted
  JSONL, never in the live model constructors. `fixtures/wire-legacy/` pins every
  branch with an input line and its post-migration `.expected.json`.

- **Provider request shape is a fixture too.** The captured OpenAI-compatible
  request already tells us tau sends `stream_options:{include_usage:true}` and
  `store:false`; the Codex request uses `input`/`instructions` with
  `input_text` blocks. Those live under `fixtures/sse/*/*.request.json` for M3.

## The crosscheck skeleton

`tools/crosscheck/` runs scripted fake-provider sessions through tau's print-mode
JSON serialization and normalizes the stream — UUIDs and timestamps collapse to
position tokens (`<id:0>`, `<ts:0>`) via a standalone `normalizer.py` — so two
agents that differ only in random values compare equal. Today it stores the tau
side under `expected/`. The rho side plugs in at M4a, running the identical
scenarios through `rho -p` and diffing. The normalizer is deliberately tau-free so
it can be reused (or ported) verbatim.

## Status

`cargo build`, `cargo test`, `cargo clippy -D warnings`, and `cargo fmt --check`
are all green on an empty workspace. 173 fixtures are extracted and reproducible.
Nothing does anything yet — which is exactly the point of a phase whose job was to
make "does it match tau?" a question with a file for an answer.
