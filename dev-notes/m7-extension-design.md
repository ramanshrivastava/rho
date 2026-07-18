# M7 extension design: locked WASM-component plan vs codex-rs's approach

**Phase 0 (user-required) comparison note.** Read this before the implementation
journal (`phase-7.md`). It records the codex-rs study, weighs the locked
wasmtime-component plan against OpenAI's production Rust agent, and states which
patterns rho adopts, defers, or rejects — and *why the locked approach is kept*.

Sources:
- `../codex/codex-rs` (OpenAI Codex, READ-ONLY) — the implementation-technique reference.
- `../tau/src/tau_coding/extensions/{api,runtime,loader}.py` — the **parity target**.
  tau's extension *semantics* are what rho must reproduce; implementation
  technique is ours to choose.

---

## 1. What codex-rs actually does (study summary)

codex-rs is **overwhelmingly process/subprocess-based**, and there is **no
WASM/wasmtime anywhere** in the codebase.

- **Extension boundary is a protocol (MCP / JSON-RPC over a byte stream), not an
  ABI.** Everything third-party speaks MCP: local tool servers, remote tool
  servers, plugins, even codex-as-a-tool (`codex mcp-server`). The client stack
  wraps the upstream `rmcp` SDK (`rmcp-client`), aggregates servers in
  `McpConnectionManager` (one client per server), and adapts each remote
  `rmcp::model::Tool` into codex's own `ToolDefinition`.
- **Process placement is a swappable strategy behind that boundary.** The
  `TransportRecipe` enum is `{ Stdio, StreamableHttp, InProcess(DuplexStream) }`
  and the `StdioServerLauncher` trait is `{ Local(child proc), Executor(sandbox
  host) }`. Subprocess-vs-in-process is a *per-server transport decision*, not an
  architecture fork. The **one** in-process case is the host-owned, trusted,
  first-party "codex apps" server (a Tokio `DuplexStream`) — chosen to avoid
  subprocess overhead **while keeping the same JSON-RPC boundary**.
- **Uniform tool contract.** MCP, dynamic, extension, and built-in tools all
  implement the *same* `ToolExecutor`/`ToolSpec` trait and live in one
  `ToolRegistry` (name→runtime, duplicate-name protected). They differ only in
  `handle()`. `ToolExposure::Deferred` + a `tool_search` tool keeps large
  external tool sets out of the prompt until needed.
- **Sandbox by argv-wrapping.** Untrusted model shell commands run as a
  subprocess wrapped in an OS sandbox (macOS Seatbelt / Linux landlock+seccomp via
  a dedicated `codex-linux-sandbox` helper / Windows restricted token). FS/network
  access is a `SandboxPolicy`; risky ops go through a graduated `AskForApproval`
  (`Granular` + an execpolicy rule DSL + MCP elicitation).
- **In-process code execution uses V8/deno_core, not WASM.** The `code-mode`
  crate runs model-authored JS in a V8 isolate with a cancellation handle and
  capability minimization (no `console` on `globalThis`). This is the exact niche
  a WASM plan targets — and codex chose V8.

Philosophy in one line: **default to subprocess isolation for anything untrusted;
reserve in-process for first-party trusted code; unify everything behind one
tool/RPC boundary so placement is interchangeable.**

---

## 2. The decisive difference: tau has no MCP layer

The locked plan targets **tau's extension semantics**, and tau's model is *not*
codex's model. A tau extension is:

- **In-process** — a Python module with a `setup(tau)` entry point, loaded into
  the host process (`loader.py` `spec_from_file_location` + `exec_module`).
- **Trusted-ish and first-party-shaped** — it registers tools/commands/renderers
  and subscribes to hooks via a direct API object (`ExtensionAPI`), then runs as
  *host code* on the host's event loop. There is no wire protocol, no
  subprocess, no MCP.
- **Capability-limited by construction, not by OS sandbox** — a renderer returns
  a **markup string**, never a widget (tau's deliberate deviation from Pi's
  widget-returning components); hooks return small typed result dataclasses
  (`ToolCallHookResult`, `InputHookResult`, …); tool *execution* is brokered by
  the host tool executor seam (`runtime.py::_wrap_tool`).
- **Dispatched strictly sequentially per extension** (`_handlers_for` yields in
  load order; each hook awaited before the next), with per-extension failure
  isolation and a `/reload` generation-staleness guard (`ExtensionGeneration`).

That is **precisely the "in-process, trusted, first-party, capability-sandboxed"
niche** the codex study concludes WASM components fit best. codex's
MCP-subprocess backbone exists to solve a problem tau does not have: connecting
to *external, untrusted, third-party or remote* tool servers. tau has no such
surface, so importing an MCP subprocess architecture into rho would be **scope
creep beyond tau parity, not parity work.**

---

## 3. Locked plan vs codex — dimension by dimension

| Dimension | Locked plan (rho, WASM) | codex-rs | Verdict for rho |
|---|---|---|---|
| Extension unit | WASM component (wasmtime), one instance per extension | stdio/HTTP MCP subprocess; V8 isolate for code-mode | **Keep WASM.** Maps tau's in-process trusted module 1:1; sandboxed without a subprocess-per-extension tax. |
| Boundary | WIT world `rho:extension` (typed guest exports / host imports) | JSON-RPC over byte stream (MCP) | **Keep WIT**, but *treat it as one transport behind a host-side trait* (see §4). |
| Trust model | Capability sandbox: no ambient FS/net; tool exec brokered by host | OS sandbox (seatbelt/landlock) for untrusted subprocesses | **Keep capability sandbox.** WASM's deny-by-default imports give us tau's "no ambient authority" for free, in-process. |
| Tool contract | Extension tools become `AgentTool`s, composed with built-ins | one `ToolExecutor` trait + `ToolRegistry` for all sources | **Adopt the spirit** — rho already has a uniform `AgentTool`; extension tools wrap a host closure that calls the guest. |
| Renderers | markup **strings** across the boundary | n/a (codex renders host-side) | **Keep strings** — this is the load-bearing decision that makes the WASM boundary feasible at all. |
| Hot reload | notify + component re-instantiation (`/reload` parity) | per-server kill/respawn | **Keep**; mirror tau's generation invalidation. |
| Approval/permissions | tau's `tool_call` block/rewrite hook | graduated `AskForApproval` + execpolicy DSL | **Parity = tau's hook.** Note codex's graduated model as a *future* idea; not tau semantics. |
| Discovery/config | `~/.rho/extensions/` + `-x` flag; manifest via `[tool.rho]` in pyproject-style | `[mcp_servers.*]` in config.toml; `codex mcp add/list/...` | **Keep tau's discovery** (`loader.py` parity). |

---

## 4. What we adopt from codex (refinements to the locked plan)

The study does **not** argue for materially changing the locked approach *for
this milestone* (tau parity dictates in-process hooks). But two codex lessons
sharpen the design without changing its shape, and rho will adopt them:

1. **Transport-neutral host seam.** codex's single most transferable lesson is
   that the extension boundary should be an interface with WASM as *one*
   implementation, not the architecture itself. Concretely, `rho-ext-host` will
   express dispatch through an internal `ExtensionHost` trait (load, register,
   run each hook, teardown). The wasmtime-backed `WasmExtensionHost` is the only
   implementation M7 ships, but the `CodingSession` integration depends on the
   trait, not on wasmtime types. Payoff: (a) the `rho-coding` ↔ host coupling
   stays wasmtime-free so **default builds stay lean** (feature-gate `wasmtime`
   in `rho-ext-host`; a no-op host satisfies the trait when the feature is off,
   exactly like tau's `NullUiBridge`); (b) a future process/MCP transport — if
   rho ever grows one — slots in beside WASM without touching the session. This
   is codex's `TransportRecipe`/`StdioServerLauncher` seam, imported as a
   host-abstraction rather than a wire protocol.

2. **Uniform tool contract + per-extension isolation + independent lifecycle.**
   rho already has `AgentTool` (codex's `ToolExecutor` analogue). Extension tools
   become `AgentTool`s whose `execute_fn` is a host closure that marshals args
   into the guest and unmarshals the result — identical downstream to a built-in.
   Per-extension failure isolation and "first registration per name wins" come
   straight from tau `runtime.py` and match codex's duplicate-name protection.

**Explicitly rejected for M7 (with rationale, so a later milestone can revisit):**

- **Process/MCP subprocess servers** — not in tau; would be a new product
  surface, not a port. If rho ever wants external tool servers, the §4.1 seam is
  where they attach. *Deferred, by design, behind the host trait.*
- **V8/deno_core for code execution** — rho has no code-mode surface; tau has no
  code-mode surface. N/A.
- **Graduated approval / execpolicy DSL** — tau's model is the single
  `tool_call` block/rewrite hook. Reproducing codex's richer model would diverge
  from tau. Noted as a future idea only.

---

## 5. Locked decisions carried into implementation (unchanged)

- **wasmtime component model**, `Config::async_support(true)`, `func_wrap_async`
  for host imports the guest awaits. wasmtime 46.x; guests target
  `wasm32-wasip2` (component model). Feature-gated so default `cargo build`
  never compiles wasmtime.
- **WIT world `rho:extension`.** Guest exports: `init` + hook handlers
  (`session-start`/`session-shutdown`, `turn-start`/`turn-end`, `input`,
  `tool-call`, `tool-result`) with tau's Pi-shaped `(event, context)` signature
  and tau's result types (`input-hook-result`, `tool-call-hook-result`,
  `tool-result-hook-result`). Host imports: `register-tool`, `register-command`,
  `add-prompt-guideline`, `register-message-renderer`, `register-key-interceptor`,
  `notify`, session read APIs.
- **Registration only during a dedicated `init` phase**; hooks dispatched
  **strictly sequentially per extension** (tau runtime parity).
- **Capability sandbox**: no WASI ambient FS/net imports granted; tool execution
  brokered by the host.
- **Discovery** from `~/.rho/extensions/` + `-x/--extension` CLI flag (already
  parsed, currently stubbed "M7"); **hot reload** via re-instantiation (`/reload`
  parity), with generation-staleness semantics ported from
  `ExtensionGeneration`.
- **Renderers return markup strings.**
- **Examples ported as Rust guests**: `examples/extensions/hello_tool` (one custom
  tool) and `permission_gate` (a `tool_call` hook blocking dangerous bash),
  ported from tau's `examples/extensions/*.py`.

---

## 6. Flag to main

The user required this study *before* design specifically to catch the case where
codex's approach argues for materially changing the locked WASM plan (e.g.
process-based MCP servers instead of / alongside WASM). **Assessment: it does
not, for this milestone.** codex's MCP-subprocess backbone targets external/
untrusted/remote tool servers — a surface tau does not have — so adopting it here
would be scope creep beyond the parity target, not parity. codex's *own*
recommendation places WASM components in exactly rho's niche: the in-process,
trusted, first-party, capability-sandboxed extension slot. We therefore **keep
the locked WASM approach**, adopting the two non-structural refinements in §4
(transport-neutral `ExtensionHost` trait; uniform tool contract) that make the
design more codex-like *behind* the WIT boundary without abandoning it.

This conclusion is flagged to main before implementation, per the brief.
