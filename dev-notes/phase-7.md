# Phase 7 — the extension system (the finale)

M7 is the last milestone: rho grows the ability to load third-party **extensions**
and drive interactive **OAuth login** from the TUI. It is also the one milestone
whose parity target is a *capability surface* rather than a byte stream — an
extension can't change rho's wire format, so "parity" here means reproducing
tau's extension **semantics** (what hooks fire, in what order, with what
fail-safe behavior), not matching bytes. The implementation technique is rho's
own: where tau loads Python modules into its own process, rho loads sandboxed
**WebAssembly components** through `wasmtime`.

Read `dev-notes/m7-extension-design.md` first — it is the Phase 0 study this
journal assumes.

## Phase 0: the codex-rs study (why we kept WASM)

The user required a study of OpenAI's production Rust agent `codex-rs` *before*
any design, to catch the case where a battle-tested Rust agent's approach argues
against the locked wasmtime-component plan. The full comparison is in
`m7-extension-design.md`; the conclusion, in one paragraph:

codex-rs uses **zero WASM**. Its extensibility is **process-based MCP
subprocesses** (stdio/HTTP, one child per server) behind a uniform tool trait,
OS-level sandboxes (seatbelt / landlock+seccomp) for untrusted shell commands,
and a **V8 isolate** (not WASM) for in-process model-authored code. The single
most transferable lesson is architectural, not technological: codex treats
"subprocess vs in-process" as a *swappable transport behind a uniform tool/RPC
boundary* (`TransportRecipe`, `StdioServerLauncher`), never as an architecture
fork. But its MCP-subprocess backbone solves a problem **tau does not have** —
connecting to external, untrusted, remote tool servers. tau's extensions are
in-process, trusted, first-party hook callbacks; that is *exactly* the niche
codex's own analysis says WASM components fit best. So importing MCP subprocesses
into rho would be scope creep beyond the parity target, not parity. We kept the
locked WASM approach and adopted two non-structural refinements codex validates
(below). This was flagged to main before implementation.

## Architecture: the transport-neutral host seam

The codex lesson shows up as the crate split:

```text
rho-coding::extensions::ExtensionRuntime   <- tau runtime.py orchestration
        |  (depends only on the trait)
        v
rho-ext-host::ExtensionHost (trait)        <- the seam
        |                         \
        v                          v
   NoopExtensionHost          WasmExtensionHost   (feature "wasmtime")
   (always linked)            (component runtime)
```

- **`ExtensionRuntime`** (in `rho-coding`) holds the parity-critical logic that
  has nothing to do with WASM: hook chaining and fail-safe semantics, first-wins
  registration, the Pi turn-index adaptation and `agent_event` wildcard fan-out,
  diagnostics, reload. It is a straight port of `tau_coding/extensions/runtime.py`
  — and, crucially, it is testable against a `FakeExtensionHost` with no wasm
  toolchain at all, which is how the hook-parity suite runs.
- **`ExtensionHost`** (trait, in `rho-ext-host`) is the seam. `CodingSession`
  depends only on it, never on wasmtime. This keeps the wasmtime dependency
  **optional** — the default `cargo build` links `NoopExtensionHost` and never
  compiles wasmtime — and it means a future process/MCP transport could slot in
  beside WASM without touching the session (codex's `TransportRecipe` seam,
  imported as a host abstraction).
- **`WasmExtensionHost`** (feature `wasmtime`, in `rho-ext-host`) is the real
  component runtime: `Config::async_support(true)`, `bindgen!` over the
  `rho:extension` WIT world, `func_wrap_async` host imports backed by a
  `HostBridge`, a per-extension `Store`, a capability sandbox that grants **no**
  ambient WASI FS/network, and component re-instantiation for `/reload`.

Two deliberate ABI choices make the WASM boundary feasible while preserving tau's
semantics, both echoing tau's own deviations from Pi:

1. **Free-form JSON crosses as canonical JSON text.** Tool arguments,
   custom-message details, and the transcript have no WIT type; a text ABI is
   simple and stable. (tau's analogous move: renderers return markup strings, not
   widgets.)
2. **The guest declares subscriptions during `init`** by calling a host
   `subscribe(event)` import (mirroring `tau.on(event, handler)`), so a hook is
   dispatched only to extensions that subscribed — a faithful port of tau's
   `_handlers_for`, not a call-everything-then-no-op scan.

## Discovery: simpler than tau, on purpose

A compiled WASM component is a single self-contained file, so tau's entire
Python-package apparatus in `loader.py` — submodule search locations,
`sys.modules` namespacing, relative imports, `src/`-layout `pyproject.toml`
manifests — has **no rho counterpart**. rho discovery (`rho-ext-host::discovery`)
keeps what still matters: directory precedence (project-first when opted in, then
`~/.rho/extensions`), the `.`/`_`-prefix skip, deterministic sorted order,
explicit `-x` paths (file or dir) that load even with directory discovery off,
and first-loaded-wins de-dup by resolved path and name. The ~15 tau discovery
tests that exercise package/manifest mechanics are N/A by construction; the rest
are ported.

## Clusters

M7 was built as a spine plus three worktree-isolated clusters forked from it:

- **spine** (this branch): the WIT world, the `ExtensionHost`/`HostBridge`
  traits, host-neutral payload types, discovery, `NoopExtensionHost`, and the
  feature-gated wasm skeleton. The frozen contract every cluster builds against.
- **coding-runtime**: `ExtensionRuntime` + `CodingSession` integration +
  `FakeExtensionHost` + the ported `test_extensions.py` semantics.
- **wasm-host**: `WasmExtensionHost` + the `rho-ext-api` guest authoring crate +
  the `hello_tool` / `permission_gate` Rust guests + host & sandbox-denial tests.
- **tui-login**: the `/login` OAuth wiring, the Login/Method picker modals, and
  the extension UI screens (task #34).

All three clusters merged onto `m7-extensions`, then the parent wired the pieces
together (the wasmtime feature end to end, the live agent-event fan-out) and
verified the whole stack.

## The `ExtensionRuntime` public API (the session's seam)

`new()` (Noop) · `for_session()` (Wasm under the feature, Noop otherwise) ·
`with_host(Arc<dyn ExtensionHost>)` (inject a host / test double) · `set_bridge`
· `rebind` · `load(paths, extra_paths, include_resource_dirs, include_project_dir)`
· `load_discovered(Vec<ExtensionSpec>)` · `reset_for_reload` · `compose_tools`
· `build_command_registry` · `prompt_guidelines` · `run_input_hooks`
· `emit_session_start/shutdown` · `emit_event` · `on_agent_event(&mut, &AgentEvent)`
· `render_custom_message` · `diagnostics` · `extension_names` · `extension_tools`
· `has_extensions`. `CodingSession` exposes `extension_runtime[_mut]()`.

## Definition of done

| DoD item | Status |
|---|---|
| Hook parity tests vs `test_extensions.py` semantics | ✅ 41 `ExtensionRuntime` tests (FakeExtensionHost) |
| Hot-reload integration test | ✅ `hot_reload_reinstantiates` (host) + `reload_picks_up_changes` (runtime) |
| Sandbox-denial test (FS/net fails cleanly) | ✅ `sandbox_denies_filesystem_and_network` |
| Both example guests work in TUI **and** print mode | ✅ `-x` wired into both paths; `wasm_extension.rs` proves the `CodingSession`→WASM path; featured binary smoke-tested in print mode |
| Live agent-event fan-out to extensions | ✅ inline dispatch + `agent_events_reach_..._session_run` |
| `/login` flows compile + mocked-token tests + manual checklist | ✅ modals + wiring + credential-store tests; checklist below |
| `cargo test --workspace` green (goldens/crosscheck intact) | ✅ 40 test groups, system-prompt + crosscheck goldens unperturbed |
| clippy `-D warnings`, fmt | ✅ default + `--features wasmtime` clean |
| wasmtime feature-gated; default build lean | ✅ default `cargo build` compiles **0** wasmtime crates |

## Scope ledger

| Brief item | Outcome |
|---|---|
| `rho-ext-host` (wasmtime host, feature-gated) | ✅ `WasmExtensionHost` (wasmtime 46, component model, async) |
| `rho-ext-api` (guest authoring crate, wit-bindgen) | ✅ ergonomic `Setup`/`Extension`/`export_extension!` over wit-bindgen 0.46 |
| WIT world `rho:extension` (guest exports + host imports) | ✅ `crates/rho-ext-api/wit/rho-extension.wit` |
| Pi-shaped `(event, context)` hooks + tau result types | ✅ input / tool_call / tool_result / lifecycle / agent-event |
| Renderers return markup **strings** | ✅ `render-message` → `option<string>` |
| `async_support(true)` + `func_wrap_async`; init-phase-only registration | ✅ `in_init` flag enforces it |
| Hooks dispatched strictly sequentially per extension | ✅ runtime iterates subscribers in load order |
| Capability sandbox (no ambient FS/net) | ✅ empty `WasiCtx`; sandbox-denial test |
| Discovery from `~/.rho/extensions/` + `-x` | ✅ `discovery.rs`; simpler than tau (one-file component) |
| Hot reload (re-instantiation, `/reload` parity) | ✅ `load` replaces the instance set; reload tests |
| Port `hello_tool` + `permission_gate` as Rust guests | ✅ under `examples/extensions/` (+ `sandbox_probe` fixture) |
| Task #34: interactive `/login` OAuth in the TUI | ✅ pickers + browser/device flows + credential store + provider swap |
| Unstub extension TUI screens (Select/Confirm/Input) | ✅ modals + `ExtensionUiHandle` (see deferral on live wiring) |

## Wired in the review pass (were deferred)

The Codex + CodeRabbit review turned three documented deferrals into shipped
wiring:

- **Input hooks + agent-event fan-out + `session_start`/`session_shutdown` now
  fire in production.** `CodingSession::prompt` runs `input` hooks on the raw
  prompt before expansion (handled → consume; transform → replace); the agent
  loop dispatches each canonical event to subscribers inline; `session_start`
  fires on load, `session_shutdown` on `/reload` and print-mode exit. Print mode
  gets all of this for free (it drives `session.prompt`).
- **A live `SessionContextBridge` is bound.** Extension `context.*` reads
  (`cwd`/`model`/`provider`/`session_id`/`system_prompt`) reflect the current
  session and update in place on `set_model`/`set_provider`, via a shared
  `Arc<Mutex<SessionContext>>` (the sync-mutator-friendly design that sidesteps
  the `CodingSession`-ownership cycle).
- **wasmtime resource limits.** A malicious guest can no longer hang or OOM the
  host: per-call fuel metering (a `loop {}` tool traps — `runaway` fixture test)
  plus a `StoreLimits` memory ceiling, complementing the empty-`WasiCtx`
  capability sandbox.

## Deferrals (with rationale)

- **`transcript`/`is_running` reads + interactive UI dialogs *during hooks*.**
  `context.*` scalar reads are live (above), but the transcript and run-state
  live inside the non-`Arc` `AgentHarness`, so `HostBridge::transcript_json`/
  `is_running` return the empty defaults; and a guest calling `ctx.ui.select/
  confirm/input` mid-hook is not yet routed to the TUI's `ExtensionUiHandle`
  (which exists and is unit-tested). Both need the frontend/harness handle
  threaded through; the two shipped example guests use neither.
- **Extension slash-command *execution*.** Registration, layering onto the
  default registry, and shadow-builtin rejection are ported and tested; the WIT
  has no `call-command` export and rho's `CommandHandler` is a bare `fn` pointer
  that cannot carry per-extension state, so a registered command currently
  reports it is not executable. Adding a `call-command` guest export + a boxed
  handler type is the follow-up.
- **Generation-staleness of a captured API handle across `/reload`.** Largely
  **N/A by construction**: a WASM guest instance is dropped on `teardown`, so
  there is no long-lived host-side API object a guest could misuse across a
  reload (tau's `ExtensionGeneration` guards a Python object that outlives the
  reload; rho has no such object). The observable behavior — reload replaces the
  registration set — is implemented and tested.
- **Python-package/manifest discovery tests** (`test_manifest_*`,
  relative-import, `sys.modules` namespacing, async-`setup` rejection): N/A — a
  WASM component is one self-contained file.
- **Tool `render_call`/`render_result` resolvers**: N/A — rho's M2 `AgentTool`
  carries no render hooks (only the custom-message renderer applies, and it is
  ported).

## `/login` manual verification checklist (task #34 — live, human-run)

The OAuth handshakes open a browser and bind local sockets, so they cannot run in
CI; the non-interactive machinery is unit-tested (see
`dev-notes/oauth-manual-checklist.md`). Build the interactive binary and run in a
scratch dir; watch `~/.rho/credentials.json` (mode `0600`, sorted keys) and
`~/.rho/providers.json`.

1. **Picker flow** — `/login` shows the method picker (Subscription / API key /
   Custom); arrows wrap, Enter selects, Esc/Ctrl+D close.
2. **Anthropic (Claude Pro/Max)** — `/login` → Subscription → Anthropic. Browser
   opens `https://claude.ai/oauth/authorize?…`; callback on `:53692`. Expect a
   modal-shown URL, then "Saved login for Anthropic…", an `anthropic` OAuth
   credential (`account_id` null), and the provider swapped.
3. **OpenAI Codex (ChatGPT)** — `/login` → Subscription → OpenAI Codex. Browser
   opens `https://auth.openai.com/oauth/authorize?…`; callback on `:1455`.
   Another machine: paste the full redirect URL into the modal's `code:` field
   (a `state` mismatch must show "OAuth failed: OAuth state mismatch"). Expect an
   `openai-codex` credential with access/refresh + `account_id`.
4. **GitHub Copilot (device flow)** — `/login` → Subscription → GitHub Copilot.
   Prompts for an Enterprise domain (Enter = github.com), shows the verification
   URL + `XXXX-XXXX` code; authorize in browser; expect a `github-copilot`
   credential and provider swap.
5. **API-key login** — `/login openai` (or via the picker) → masked field →
   paste + Enter → `openai` API-key credential + "Saved login for OpenAI."
6. **Custom provider** — `/login custom` → fill id / base URL / models / key →
   provider added to `providers.json` + catalog, credential stored, swapped.
7. **Logout** — `/logout` lists only providers with stored credentials; select →
   credential removed. `/logout` with none stored shows the "no stored
   credentials" notice.
