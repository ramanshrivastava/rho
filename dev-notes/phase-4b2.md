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

<!-- Finalized during integration; see the commit series. -->

## Deferred / honest ledger

<!-- Finalized during integration. -->
