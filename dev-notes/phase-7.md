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

```
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

<!-- TODO(integration): fill in per-cluster outcomes, the final ExtensionRuntime
     public API, the scope ledger (every brief item → landed / deferral-rationale),
     the DoD checklist, and the /login manual checklist once the clusters merge. -->

## Scope ledger

<!-- TODO(integration): one row per brief item. -->
