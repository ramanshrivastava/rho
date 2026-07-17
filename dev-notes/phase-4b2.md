---
title: "Phase 4b (dispatch 2): provider catalog, commands, skills, templates, OAuth, export, full CLI"
---

M4b-1 landed the `CodingSession` *core* — persistence, compaction, branch
summaries, thinking, and the session-backed `rho -p`. Every provider-catalog
branch collapsed to its `None` default because `provider_settings` /
`runtime_provider_config` were always absent. This dispatch fills that surface
in: the vendored provider catalog, model/provider resolution, the slash-command
registry, skills + prompt templates, the credential store + OAuth machinery, and
HTML/JSONL session export — then wires them through `CodingSession` and the full
print-mode CLI.

## The vendored catalog is the source of truth

`data/catalog.toml` is copied **byte-for-byte** from tau (sha256
`4d882a5d…04a365`) and loaded via `include_str!`. `catalog_loader.rs` parses it
with `toml` (`preserve_order`) into ordered `ProviderCatalogEntry` records —
insertion order matters, so provider/model lists use `indexmap`, never a
`HashMap`. `provider_catalog.rs` exposes the built-in catalog; `provider_config.rs`
(the 2282-LOC heart) is the durable provider model: `ProviderConfig`
(`OpenAICompatible`/`Anthropic`/`OpenAICodex`), scoped-model names, compat
profiles, env-key mapping, thinking-level resolution, `context_windows`, and the
`*_config_from_provider` bridges to the `rho-ai` runtime configs.

**Byte-parity is checked against tau, not asserted by hand.** The ported
`test_provider_config` / `test_provider_catalog` suites (62 cases) reproduce
tau's expected resolutions, and `providers.json` is written with recursively
sorted keys to match tau's `json.dumps(sort_keys=True)`.

## Dependency inversion at the catalog/credential seam

tau imports `FileCredentialStore` and `oauth_registry` *directly* inside
`provider_config` and `provider_runtime`. rho split those into separate clusters
(so `rho-coding` can be built and tested a piece at a time), which would create a
module cycle. The fix is a small `CredentialReader` trait in `provider_config`
(`get` / `get_oauth`) that the config layer depends on abstractly;
`provider_runtime.rs` provides `impl CredentialReader for FileCredentialStore`,
the one place the two layers meet. `get_oauth_provider` is likewise resolved in
`provider_runtime` rather than imported into the config layer.

## `provider_runtime::create_model_provider`

The factory that turns a durable `ProviderConfig` into a live `rho-ai` provider,
replacing M4a's minimal env selection. It routes anthropic / codex /
openai-compatible (including the `google-generative-ai` / `mistral-conversations`
/ `anthropic-messages` sub-dispatch on `config.api`) and, for OAuth-backed
providers, installs the per-request credential resolvers.

Two deliberate deviations from tau, journaled in the module header:

- **Injected clock + HTTP client.** tau's resolvers await an ambient `httpx`
  client and read `time.time()`. rho's `OAuthProvider::refresh` and
  `refresh_openai_codex_token` take an explicit `&dyn OAuthHttpClient` and a
  `now_ms: i64` (so refresh logic is deterministically unit-testable against
  recorded fixtures — see A2). The resolver closures construct a
  `ReqwestOAuthClient` and read the wall clock at call time; these are
  runtime-only values, never persisted.
- **A credential read error reads as absent.** `CredentialReader::get` returns
  `Option`, so a `CredentialStoreError` collapses to `None` (i.e. "no stored
  credential"), matching how a missing store already reads.

## Credentials & OAuth

`credentials.rs` writes `~/.rho/credentials.json` byte-identically to tau — sorted
keys, 2-space indent, `ensure_ascii` escaping, trailing newline, temp-file +
atomic rename, mode `0600`. The OAuth machinery (`oauth*.rs`) ports the provider
registry, PKCE, JWT `account_id` extraction, and token refresh for Anthropic,
ChatGPT Codex, and GitHub Copilot device flow. Token refresh is unit-tested
against a `MockHttpClient` (tau's `MockTransport` analog); the *interactive*
login flows (browser authorize, device-code polling, local callback server) are
behind the manual checklist in `dev-notes/oauth-manual-checklist.md` — logic is
tested, only a live-IdP end-to-end run is manual.

## Skills, prompt templates & `expand_prompt_text`

`skills.rs` / `prompt_templates.rs` build on the M4b-1 `resources.rs`. Session
`load()` now discovers skills (gated on `skills_enabled`) and prompt templates
alongside project context, concatenating diagnostics in tau's
skill → prompt → context order. `expand_prompt_text` ports tau faithfully: a
`/name [args]` prompt-template command wins first (it never errors), then a
`/skill:name` command. tau raises the unknown-skill `ResourceError` (a
`ValueError`, which its CLI turns into an exit-2 `BadParameter`) out of
`prompt()`; rho records the message on the session `run_error` so print mode
exits non-zero — a one-code divergence (1 vs 2) on an error path, journaled here.

`build_skill_index` is ported as a standalone helper but, exactly as in tau, is
**not** wired into the system prompt (tau's prompt uses the separate
`format_skills_for_prompt` block, already ported in M4b-1), so the system-prompt
golden is unchanged.

## Session export

`session_export.rs` renders the transcript to HTML (byte-matching
`fixtures/export/kitchen-sink.html`) and JSONL. The critical subtlety
(AGENTS.md): session *storage* uses `exclude_none`, but `export_session_jsonl`
writes **nulls** — the exporter re-densifies omitted fields in pydantic order
rather than reusing the wire form. Two documented parity gaps, both unreachable
by the golden and the ported tests: a `model_dump_json` fallback for exotic
`MessageEntry` payloads, and `serde_json`'s no-scientific-notation float
formatting inside HTML *JSON-highlight* blocks (the JSONL wire path serializes
struct `f64`s correctly).

## How this was built — parallel cluster ports

The ~6200 LOC were decomposed into independent module clusters, each ported in an
isolated git worktree by a dedicated agent, then merged into `m4b-full-cli` and
re-verified together: (A1) catalog + config, (A2) credentials + OAuth, (A3)
skills + templates, (A4) export — and the interdependent glue (provider_runtime,
the command registry, the session dispatch-2 surface, and the CLI) integrated on
top. The four leaf clusters touch disjoint files, so only `lib.rs` / `Cargo.toml`
needed union-merges.

## CommandSession, the slash-command registry & the CLI

`commands.rs` ports tau's `CommandRegistry` and the 17 print-mode commands behind
a `CommandSession` trait (tau's `Protocol`). `CodingSession::handle_command`
dispatches to a freshly-built default registry — a `/name [args]`
prompt-template command is an *expansion directive* and stays unhandled so it
flows through `prompt()`, exactly as tau does. `impl CommandSession for
CodingSession` is the registry's view of the session; three seams differ from
tau and are journaled at the impl:

- `model()` / `system_prompt()` borrow the harness config (`&AgentHarnessConfig`)
  because the inherent accessors return owned `String`s.
- `context_token_estimate` / `context_usage_breakdown` recompute the estimate on
  `&self` (the `&mut self` accessor caches); they reuse the cache when present.
- `ensure_session_indexed` performs only the *synchronous* index-record create;
  rho's transcript flush is `async`, so it is deferred to the next durable write
  (tau flushes eagerly). `set_model` in the command path drops the rare
  provider-refresh error (the handler pre-validates the model against
  `available_models`, so it cannot fire there); tau would propagate it.

Print mode now routes commands before the agent turn (tau `run_print_mode`):
`parse_terminal_command` → `handle_command` → on a handled command, print its
message (running `/reload` and rendering `format_reload_summary` when requested)
and return; only an unhandled prompt drives the agent. `SessionPrintModeConfig`
carries `provider_settings` + `runtime_provider_config`, so the print session is
catalog-aware — `/session` reports the active provider's context window and the
system/message/tool token breakdown.

The `rho` binary grew the full surface in clap: `-p`/`--provider`/`-m` print
mode with catalog resolution (`resolve_provider_selection` +
`create_model_provider`), plus `sessions`, `providers`, `export`, and `setup`
subcommands mirroring `cli.py`. `providers` lists the vendored catalog with each
provider's credential status (`stored:` / `env:` / `missing`); `export` renders a
session id or `.jsonl` to HTML/JSONL. `--resume` / `--new-session` are accepted
but report that interactive mode is M5 (no-prompt, no-subcommand does the same).

Verified end to end: `rho providers` (full catalog), `rho -m gpt-5.4` (resolves,
then the missing-key error — proving catalog resolution), `rho -m no-such-model`
(rejected with the model list), `rho export <jsonl> --format html`, and
`rho --fake -p "/hotkeys|/session|/quit"` (print-mode commands).

## Deferred / honest ledger

- **`update_check.py` — skipped by design.** It is a network beacon (queries the
  upstream release feed at startup); porting it adds a runtime network call with
  no parity value for the offline oracles. Deferred; the CLI simply never emits
  a startup update/notice line.
- **TUI-only command results are inert in print mode.** The `*_picker_requested`
  flags on `CommandResult` (model/resume/tree/login/logout/theme pickers) are
  carried for parity but, exactly like tau's `run_print_mode`, print mode only
  consumes `message` + `reload_requested`. The pickers land with the M5 TUI.
- **Live-provider paths are constructed, not exercised.** `create_model_provider`
  builds real `rho-ai` providers; the DoD demo uses `--fake` or the catalog
  resolution up to the credential check. OAuth interactive login is behind the
  manual checklist.
- **`session_title()` reads the index record live** (tau
  `CodingSession.session_title`): it resolves `session_id` → `session_manager.get_session`
  → `record.title`, so a named session surfaces tau's `/session` "Session name:"
  line. The `CommandSession::session_title` return type is `Option<String>` (not
  `Option<&str>`), because `get_session` hands back an owned record — the title
  cannot be borrowed from `self`. `/name` still writes the index via
  `touch_session`. (Earlier this was stubbed to `None`; a fake-session unit test
  masked the gap, now covered by a real-session integration test.)
- **A harness provider-swap drops live event subscribers.** `set_model` /
  `set_provider` rebuild the harness from a cloned config (rho's `AgentHarness`
  has no in-place model/provider setter). Harmless today — rho has no extension
  runtime and per-turn event fan-out is created fresh — but a future milestone
  relying on persistent `harness.subscribe` listeners needs a real setter.
- **The `/skill:` unknown-skill exit code** is 1 (via `run_error`), not tau's 2
  (`ResourceError`→`ValueError`→`BadParameter`); see the skills section above.
- **`--fake` and `--session <path>` are rho-only CLI additions.** `--fake`
  selects the deterministic demo provider (no tau equivalent — tau's fake lives
  only in tests); `--session <path>` persists the transcript to an explicit JSONL
  file, *unindexed*, as an escape hatch. The default print path matches tau
  (`run_openai_print_mode`): create + index a session, persist at `record.path`.
- **`--extension` / `-x` and precise `--resume` + `--new-session` errors are M5
  scope.** rho accepts `--resume` / `--new-session` but reports interactive mode
  as M5 rather than reproducing tau's exact `BadParameter` "mutually exclusive"
  specificity; extension flags land with the M7 WASM host. Both are deferred, not
  divergences to keep.
- **Index reads are strict; index writes are best-effort.** A malformed /
  schema-invalid index line makes the *read* APIs (`list_sessions` / `get_session`
  / `latest_session_for_cwd`, and thus `rho sessions` / `rho export`) fail with a
  non-zero exit (tau's propagating `ValidationError`). The write-path `upsert` and
  a few internal best-effort lookups (auto-naming, ensure-indexed — which tau also
  wraps in try/except) tolerate a corrupt index rather than abort a write.

## Review round (bots)

Codex flagged one real P2: the export path serialized entries by round-tripping
through `serde_json::Value`, whose default `f64` writer emits scientific
notation (`0.0000005` → `5e-7`), whereas tau's export uses `json.dumps`
(`float.__repr__` → `5e-07`). Fixed with a `PyFloat` `serde_json::Formatter`
that routes `write_f64` through `pystr::python_float_repr` while delegating
structure to the wrapped `Compact`/`Pretty` base, applied to both the JSONL
(`dump_entry_line`) and HTML (`json_dump`) paths; whole-number floats are
unchanged so the byte-match goldens hold, with a small-float regression test.

Why the fix is **export-only** (a genuine tau quirk, verified against the
interpreter): tau serializes floats *two different ways* depending on the path.
- Storage / wire (`model_dump_json`) goes through **pydantic-core**, whose
  serializer is written in Rust on top of serde — so `UsageCost(input=0.0000005)`
  emits `"input":5e-7`, identical to serde's default.
- Export (`json.dumps`) uses Python's stdlib, i.e. `float.__repr__` → `5e-07`.

So `rho-agent`'s wire codec (`entry_to_json_line`, plain serde) is *already
correct* and must **not** get the `PyFloat` treatment — matching it to `5e-07`
would break byte-parity with pydantic-core. The Python float-repr shape exists
only on `json.dumps` paths, which in this milestone is exactly the two export
serializers. Teaching nugget: "port Python float repr" is not a blanket rule —
it applies where tau reaches for `json.dumps`, not where it reaches for pydantic.

## Review round 2 (adversarial verify)

A second review pass (on top of the Codex float fix) surfaced accepted-scope
carryovers from M4b-1 plus polish. All are fixed with tau evidence and tests:

- **C1 (critical) — print mode now persists + indexes like tau.** The default
  `rho -p` path was building an *in-memory* session with no `session_manager` /
  `session_id`, so `ensure_session_indexed` never ran and a print run never
  appeared in `rho sessions`. `run_session_print_mode` now mirrors
  `cli.run_openai_print_mode`: it `create_session(cwd, model)`s (indexing
  immediately), persists JSONL at `record.path`, and wires `session_id` +
  `session_manager`. `--session <path>` stays as the rho-only unindexed override.
  Note the print flow makes **two** provider calls — the agent turn and the
  one-shot session auto-naming — exactly as tau does.
- **I1 — a corrupt session index is fatal on read.** `read_index` returned a
  best-effort `Vec` that silently dropped unparseable lines; tau's `_read_index`
  lets pydantic's `ValidationError` propagate. `read_index` (and the read APIs
  above it) now return `Result<_, SessionManagerError>`; see the ledger note on
  the strict-read / best-effort-write split.
- **I2 — failed automatic compaction is logged, not swallowed.** `try_auto_compact`
  / `try_overflow_compact` ignored their diagnostic `context`; on failure they now
  `log_exception` and stash `last_diagnostic_log_path`, with tau's per-call-site
  phase (`auto_compact_before_prompt` / `_after_prompt` / `_after_continue` /
  `overflow_compact`), matching `_try_auto_compact` / `_try_overflow_compact`.
- **M1 — empty CLI strings fall back like Python `or`.** `--provider ""` /
  `--model ""` used `Option::unwrap_or`, so an empty string won (→ "Unknown
  provider: " / an empty model). A shared `or_default` helper reproduces tau's
  `value or default` truthiness at `get_provider` and every
  `model or provider.default_model` site.
- **M2 — `setup` gains the explicit `--set-default`.** The tuning flags
  (`--base-url` / `--api-key-env` / `--timeout-seconds` / `--max-retries` /
  `--max-retry-delay-seconds`) already existed; added `--set-default` as the
  `overrides_with` counterpart to `--no-set-default` (tau's `--set-default/
  --no-set-default`, default true).
- **M3 — frontmatter parsing uses `pystr::splitlines`.** `resources.rs` split on
  `'\n'` only; tau uses `str.splitlines()` (every Unicode line boundary, no
  trailing empty). Swapped to the existing `pystr::splitlines` port.
