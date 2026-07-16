---
title: "Phase 1: The Wire Types, Byte for Byte"
---

Phase 1 ports every wire type in `tau_agent` into `crates/rho-agent` and holds
each one to a single, unforgiving standard: for every golden fixture, `parse →
re-serialize` must be **byte-identical** to what tau emitted. The M0 fixtures are
the oracle; if a golden test diffs, the rho code is wrong, never the fixture.

The port is small in line count and large in fiddliness. tau's format is a dozen
quiet decisions — which key casing, which fields are omitted, how a float prints —
that individually look trivial and collectively *are* the compatibility contract.
This note records the ones that bit, and the serde idioms that reproduce Pydantic
without a bespoke serializer.

## The central problem: reproducing a Pydantic `WireModel`

tau's `WireModel` is a `pydantic.BaseModel` with this config:

```python
model_config = ConfigDict(
    extra="forbid",              # reject unknown fields
    serialize_by_alias=True,     # emit aliases, not field names
    alias_generator=_to_camel,   # snake_case -> camelCase
)
# and, at dump time: model_dump_json(by_alias=True, exclude_none=True)
```

Each of those maps onto a serde attribute:

| tau (Pydantic)                 | rho (serde)                                             |
|--------------------------------|--------------------------------------------------------|
| `alias_generator=_to_camel`    | `#[serde(rename_all = "camelCase")]`                    |
| `extra="forbid"`               | `#[serde(deny_unknown_fields)]`                         |
| `exclude_none` (dump time)     | `#[serde(skip_serializing_if = "Option::is_none")]`     |
| discriminated union on a tag   | `#[serde(untagged)]` enum + `monostate::MustBe!(tag)`   |
| free-form `JSONValue`          | `serde_json::Value` / `Map` (with `preserve_order`)     |

The interesting decisions are where that table is *not* a clean one-liner.

## Why untagged unions + monostate, not `#[serde(tag = ...)]`

The obvious way to model a discriminated union in serde is internally-tagged:
`#[serde(tag = "role")]`. It even produces the right output for most messages,
because their discriminator is the first field. But it fails for **session
entries**, whose wire shape is:

```json
{"id":"e1","parent_id":null,"timestamp":1731234567.0,"type":"message","message":{…}}
```

The `type` discriminator is *fourth*, after `id`/`parent_id`/`timestamp`. serde's
internally-tagged representation always hoists the tag to the front, so it would
emit `{"type":"message","id":…}` — a byte mismatch. Internally-tagged is
structurally incapable of the position tau requires.

The fix, applied uniformly across the whole crate so there is one idiom to learn:

- Model each union as `#[serde(untagged)]`. An untagged enum serializes a variant
  struct in its **declared field order** — so the discriminator lands wherever we
  put the field.
- Give each variant a discriminator field of type `monostate::MustBe!("message")`.
  `monostate` is a zero-size type that serializes as exactly that string literal
  and deserializes **only** when the input matches. That turns "try each variant
  until one fits" (untagged's default, order-dependent and fuzzy) into exact
  discrimination: for any input, exactly one variant's `MustBe!` matches.

So `SessionEntry` variants declare `id, parent_id, timestamp, type, …` in that
order, with `type: MustBe!("message")` sitting in its true fourth position, and
the bytes come out right. Variant order in the enum then only affects *speed*
(untagged tries them top-to-bottom), never correctness — so unions are ordered
most-frequent-first (`message`/`text` lead).

A pleasant surprise: `deny_unknown_fields` **is** honored through the untagged
buffering (verified by `messages::tests::unknown_fields_are_rejected` — an unknown
key is rejected both on a leaf struct and through the `AgentMessage` union). So
`extra="forbid"` parity holds even inside the untagged machinery.

## Casing: two casings on one line, and the digit trap

A `MessageEntry` line carries **both** casings at once: snake_case at the entry
level (`parent_id`, `replaces_entry_ids`) wrapping a camelCase `message`
(`toolCallId`, `cacheRead`). serde handles this because casing is per-struct:
entry structs carry no `rename_all` (field names are already the wire names),
while message/event structs carry `#[serde(rename_all = "camelCase")]`.

The trap is that tau's `_to_camel` and serde's `camelCase` **disagree on digits**.
`_to_camel` title-cases every non-first underscore segment:

```python
def _to_camel(name):
    parts = name.split("_")
    return parts[0] + "".join(p.title() for p in parts[1:])
# "cache_write_1h" -> "cache" + "Write" + "1h".title() -> "cacheWrite1H"
```

`"1h".title()` is `"1H"` (Python title-cases the first *letter*, which is `h`).
serde's `camelCase` upper-cases the first *character* of each segment — for `"1h"`
that's the digit `1`, a no-op — yielding `cacheWrite1h`. So serde silently gets
`cache_write_1h` **wrong**. Every multi-segment field was checked against the
fixtures; this is the only one that diverges, and it carries an explicit
`#[serde(rename = "cacheWrite1H")]`. (See `usage/usage_full.json`, which pins the
capital `H`.)

## `exclude_none` vs. free-form JSON: the non-recursion rule

`exclude_none` omits `None` fields — but only *typed* ones, and it does **not**
recurse into free-form JSON payloads. A literal `null` inside `arguments`,
`details`, or `data` is preserved. Two different mechanisms in rho:

- Typed optionals get `skip_serializing_if = "Option::is_none"`, so `None` is
  omitted at the top level (e.g. an absent `details` disappears entirely).
- Free-form payloads are `serde_json::Value` / `Map`, which serialize `null`
  verbatim wherever it sits *inside* the value.

The distinction is visible in one fixture pair: a top-level `details: null`
vanishes, but `arguments: {"nested":{"b":null}}` keeps its `null`
(`content/tool_call_nested_args.json`). Modeling both as `Option<Value>` /
`Value` gets this for free — an absent/`null` top-level field decodes to `None`
and is skipped; a `null` nested inside a present `Value` is retained.

One subtlety worth flagging: `details` distinguishes **absent** from **`{}`**.
`AgentToolResult` fixtures include both a present empty object (`"details":{}`,
in `tool_execution_end`) and an omitted one. `Option<Value>` captures exactly
that: `Some(Value::Object({}))` round-trips as `{}`; `None` is skipped.

## Floats: the formatting we feared, but didn't need to fix

The plan warned that Python's `json.dumps` renders small floats in scientific
notation (`5e-05`) whereas Pydantic does not, and that if serde_json's `ryu`
output diverged we'd need a custom serializer. It doesn't. A probe
(`float_probe`, run before any modeling) confirmed serde_json matches Pydantic on
every value the fixtures pin:

- `0.0` → `"0.0"` (whole floats keep the `.0` — critical for entry timestamps
  like `1731234567.0`, which must **not** collapse to an integer)
- `0.00005` → `"0.00005"` (no scientific notation — `usage/usage_full.json`)
- `3.75`, `0.00325`, `0.0001` → verbatim

So: **cost fields and entry timestamps are `f64`** (serde_json prints them tau's
way), **token counts and message timestamps are `i64`** (ints, no decimal point).
No custom float serializer was needed. Message `timestamp` is int **milliseconds**;
entry `timestamp`/`created_at` are float **seconds** — different types on purpose.

## Legacy migration: a transform on the raw `Value`, before typed decode

Because our models are `deny_unknown_fields`, they would reject old Tau-v1 lines
outright (string `content`, a `tool` role, sibling `tool_calls`, `data` payloads).
So migration runs **before** typed decoding, as a transform on the raw
`serde_json::Value` — mirroring tau, which migrates decoded dicts before
validation. `session::jsonl::migrate_*` ports `_migrate_message` faithfully:

- **user → custom**: a `custom_type`/`customType` on a user message flips the role
  to `custom`, folding the snake key into `customType` and defaulting `display`.
- **assistant**: string `content` becomes a `[{"type":"text",…}]` block list;
  sibling `tool_calls`/`toolCalls` are appended as blocks; `usage.cost == null`
  becomes `{}` (so it decodes to the all-zero default cost).
- **tool → toolResult**: renames `name`/`tool_call_id`, maps `ok` to `isError`,
  normalizes string content, and folds legacy `data` into `details`.

Two ordering details that matter for byte output:

1. The `{**data, **details}` merge is order-sensitive — data keys first, then
   details override. `serde_json::Map` with the workspace's `preserve_order`
   feature is an `IndexMap`, so inserting data then details reproduces Python's
   dict-merge order exactly (`tool_ok_data_details` → `{"path":…,"extra":1}`).
2. tau's `message.pop("tool_calls", message.pop("toolCalls", []))` removes **both**
   keys and prefers the snake variant — the Python default argument is eagerly
   evaluated, popping `toolCalls` as a side effect. `take_tool_calls` reproduces
   that: remove both, prefer `tool_calls`.

Crucially, migration does **not** need to preserve the *entry-level* key order it
produces, because the value is then decoded into a typed model and re-serialized —
the struct field order is what determines the final bytes. Migration only has to
preserve order *inside* free-form values (the data/details merge), which it does.
Migration is also a no-op on already-current (v2) entries, so the same decode path
serves both.

## What was ported, and what was skipped

**Ported wire types** (all byte-golden): content blocks (`TextContent`,
`ThinkingContent`, `ImageContent`, `ToolCall`), `Usage`/`UsageCost`, all seven
`AgentMessage` variants, `AssistantMessageDiagnostic`, the `AgentEvent` union (10
variants), the `AssistantMessageEvent` union (12 variants), `AgentToolResult`,
and all nine `SessionEntry` variants, plus the JSONL codec and Tau-v1 migration.

**Ported tests** (`messages::tests`, from `test_agent_types.py`): the user-message
wire shape, ordered assistant blocks, tool-result canonical output, per-role union
discrimination, unknown-field rejection, and string-content preservation.

**Skipped tests, with reasons** — Pydantic/Python features with no Rust analogue:

- `test_models_reject_unknown_fields` via the Python constructor kwarg — replaced
  by a deserialization-level rejection test (serde has no keyword constructors).
- `test_assistant_message_keeps_ordered_content_blocks` asserts the non-`by_alias`
  `model_dump()` shape (with `thoughtSignature: None` present). rho models **only**
  the exclude-none wire path, so the ported version asserts the wire shape
  (`thoughtSignature` omitted). The Python "convenience" representation (accepting
  a bare string for assistant `content`, `usage=None → Usage()`) is a *constructor*
  behavior; on the wire everything is already normalized, and legacy inputs are
  handled by the migration, so there is nothing to reproduce in the typed model.
- `test_agent_tool_executes_with_pi_arguments` — the `AgentTool` executor is
  behavior, not wire format; it lands in M2.
- All of `test_pi_event_protocol.py` — every case drives an `AgentHarness` +
  `FakeProvider` (M2), or asserts a Python import-cycle property that Cargo's
  acyclic crate graph enforces structurally (`rho-agent` simply cannot depend on
  `rho-ai`). The event *wire shapes* those tests exercise are covered instead by
  the `event-streams/` golden round-trip.

**Out of scope for M1**: `fixtures/wire/session-events/` (10 files). Those are
`tau_coding.events` (`SessionOwnEvent`: `auto_retry_start`, `agent_settled`, …),
which the layering contract maps to `rho-coding` (M4), not `rho-agent`. They are
noted in the golden harness and covered when that crate lands.

## The test harness

`tests/golden_roundtrip.rs` walks every in-scope fixture directory, parses each
file/line into the typed model, re-serializes, and asserts byte equality —
`wire/**` (single objects), `sessions/*.jsonl` (through the migrating decode),
`wire-legacy/*` and `sessions/legacy-v1` (migrate → compare to the `.expected`
golden), and `event-streams/*/{agent,assistant}-events.jsonl` (every streamed
event). A separate `corpus_idempotence` test asserts `serialize ∘ parse` is
stable across a second round-trip over the whole corpus.

Result: **every** non-synthetic fixture round-trips byte-identically. No fixture
required a code compromise, and none could not be matched — there is no blocker to
flag. The one place rho deliberately diverges from tau's Python surface (the
non-`by_alias` `model_dump` convenience shape) is a representation that never
reaches the wire, and is documented above.

## Pydantic features with no Rust analogue (and what replaced them)

- **`@model_validator(mode="before")` content normalization** → not modeled in the
  typed layer; the wire is always normalized, and legacy inputs go through the
  raw-`Value` migration instead.
- **Keyword constructors + `ValidationError`** → serde `Deserialize` with
  `deny_unknown_fields`; validation happens at decode time.
- **`str | int | None` (`code`) / recursive `JSONValue`** → `Option<Value>` and
  `serde_json::Value`, the exact recursive sum type, with `preserve_order` so key
  order survives.
- **`alias_generator`** → `rename_all` plus one explicit `rename` for the digit
  trap.

## Review findings (PR #2) — validators vs. serde defaults

The first cut treated tau's convenience shapes as "constructor-only" and modeled
only the canonical wire. That was **wrong**, and code review caught it. Pydantic
`@model_validator(mode="before")` methods run on **every** deserialization, not
just Python constructors — so they are part of the wire contract, and rho has to
reproduce them or it will fail to load real sessions.

### The `usage: null` lesson (the sharp one)

tau's `AssistantMessage` validator maps `usage is None → Usage()`. On the wire
that means `{"usage": null}` decodes to the default usage. The rho port used
`#[serde(default)]`, and here is the trap: **serde's `default` only fires when the
key is _absent_.** A present `"usage": null` is a value — serde tries to
deserialize `null` into the `Usage` struct, fails, and the whole untagged
`AgentMessage` parse fails → `entry_from_json_line` errors → the entire session
refuses to load. A real session that ever persisted a null usage would be
unreadable.

The fix is two-pronged, because null usage can arrive by two paths:

1. A `deserialize_with = "null_to_default"` shim on the typed `usage` field
   (`Option::<Usage>::deserialize(...).unwrap_or_default()`), so **any** direct
   typed parse — including an assistant message nested inside an event — maps
   null → default, exactly like the validator.
2. The legacy migration also rewrites a present `usage: null` to `{}` before
   typed decode, keeping the entry path robust and explicit.

Lesson for the journal: **`serde(default)` is not Pydantic's `default`.** Pydantic
runs the field default (and before-validators) for both absent *and* null;
`serde(default)` covers only absent. Any Optional-with-default that a producer
might emit as literal `null` needs a `null_to_default` shim, or the parse dies.

### Python truthiness in the migration

Two migration gates used the wrong falsiness test:

- The tool-`error` → content gate used `!err.is_null()`, but tau gates on
  `if error` (Python truthiness). So `"error": ""` (and `0`, `false`, `{}`, `[]`)
  must be **ignored**. Verified against tau: `error:"" + empty content` → tau
  emits `content:[]`; the old rho emitted a spurious text block.
- The `ok` → `isError` map used `as_bool().unwrap_or(true)`, silently treating a
  non-bool `ok` as `true`. tau does `not bool(ok)`, so `"ok": 0` → `isError:true`.

Both now route through a single `python_truthy(&Value)` helper (falsy = null,
false, numeric zero, and every empty container). Four new tau-authored
`wire-legacy` regression pairs pin these: `assistant_usage_null`,
`tool_error_empty_string`, `tool_ok_falsy_zero`, `assistant_string_content_only`
— generated by running the inputs through tau's own `entry_from_json_line`
(the same extractor path as every other fixture), so the goldens stay
tau-authored per repo policy.

### Behavioral parity, not just byte parity

The locked decision from review: rho should **accept** tau's convenience shapes on
the wire, not merely round-trip canonical output byte-for-byte. So a
`string_or_blocks` deserialize shim now normalizes a bare-string `content` into a
single text block (empty string → `[]`) for `AssistantMessage`,
`ToolResultMessage`, and `AgentToolResult` — the three types whose tau validators
do this. `UserMessage`/`CustomMessage` have no such validator, so their string
content is genuinely preserved; the doc comments were corrected to say so
precisely (the earlier "only happens in Python constructors" claim was false).

None of this changes the byte-golden output: canonical fixtures already carry
block lists and object usage, so the shims are pure input-acceptance widening.

### Constructors (unblocking M2)

The wire structs keep their `role`/`type` discriminator private (only the
`monostate::MustBe!` value is valid), which made them unconstructible outside the
crate — a blocker for M2. Every message/event/entry now has a `new(...)`
constructor mirroring tau's keyword constructors: required fields positional,
optionals defaulted, `timestamp` injected from `current_timestamp_ms` and entry
`id` from `new_entry_id` (`uuid4().hex` ⟶ `Uuid::new_v4().simple()`), matching
tau's `default_factory`. `AssistantMessage` also gets fluent `with_*` builders,
since external callers can't use `..Default::default()` struct-update (the private
discriminator blocks the literal) — they use `new()`/`default()` + the `pub`
fields or the builder. A `tests/constructors.rs` integration test builds each
kind from **outside** the crate to prove the surface is usable.
