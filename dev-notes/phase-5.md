---
title: "Phase 5: ratatui TUI — port of tau's Textual UI"
---

# Phase 5 — ratatui TUI (port of tau `tau_coding/tui/`)

## Intro

M0–M4b landed everything *except* the interactive UI: the full CLI, sessions
(byte-interchangeable with tau on disk), all six providers, commands, skills, and
export. Phase 5 ports tau's Textual TUI to **ratatui**, targeting visual + behavioral
parity. The original `m5-tui` worker stalled on 2026-07-17 ~21:24 UTC when it hit the
session usage limit mid-T3; this phase was resumed on the `m5-tui` branch, salvaging
the worker's uncommitted headless ports and completing the visual layer.

## The core parity principle: retained-mode (Textual) → immediate-mode (ratatui)

tau's UI is **retained-mode**: a persistent tree of Textual widgets that mutate in
place. rho's UI is **immediate-mode**: a pure [`TuiState`]
(`crates/rho-tui/src/state.rs`) that the frontend rebuilds every frame from, mutated
only by [`TuiEventAdapter`] (`adapter.rs`) in response to session events.

What must match tau exactly: **content, layout regions, keybindings, and colors per
theme.** What is ratatui-idiomatic and need NOT match literally: the internal widget
types, the render call graph, and ownership (ratatui has no retained widget tree).
Each place where parity could not be made identical is recorded below with the reason.

The transcript block formatters in `state.rs` (`format_tool_call_block`,
`format_tool_result_block`, spinner, elapsed, python-repr fallback) are byte-identical
to tau, so the transcript text reads the same in both.

## Scope ledger (brief items 1–8 → status)

| # | tau source | rho target | Status |
|---|---|---|---|
| 1 | `state.py` (547) | `TuiState` (`state.rs`) | ✅ Done (committed, tested) |
| 2 | `adapter.py` | `TuiEventAdapter` (`adapter.rs`) — the parity-critical seam | ✅ Done (committed, tested) |
| 3 | `widgets.py` (1744) + `app.py` (5819) | `widgets/{transcript,status,footer,sidebar,composer,style}.rs` + `app.rs` (event loop + render + keybindings) | ✅ Done |
| 4 | modals (in `app.py`) | `modals.rs` overlay enum (`Modal` + `ModalOutcome`) | ✅ Done (in-scope modals; login/ext → M7 `Notice`) |
| 5 | `autocomplete.py` (511) | `build_completion_state` (`autocomplete.rs`) + popup render (`composer.rs`) | ✅ Done (logic + rendering) |
| 6 | `config.py` themes + `terminal_title.py` | `theme.rs` + `terminal_title.rs` | ✅ Done (committed, tested) |
| 7 | binary wiring | `rho` no-`-p` → TUI; `app.py` BINDINGS via `matches_binding`; resume flags | ✅ Done (`crates/rho/src/main.rs::run_tui_entry`) |
| 8 | `cli.py` TUI flags (deferred from M4b) | `--extension`/`-x` parse + M7 notice; `--resume`/`--new-session` `BadParameter` | ✅ Done |

Definition-of-Done items: port `test_tui_adapter.py` + state tests (✅ done),
`test_tui_autocomplete.py` (✅ done), `test_tui_config.py`/`test_tui_components.py`
represented by `theme.rs` + widget unit tests (✅), insta snapshot suite over the
`TestBackend` for every widget + modal, transcript driven through the real adapter
(✅ `tests/snapshots.rs`), parity checklist (below), `cargo test --workspace` +
`clippy -D warnings` + `fmt` clean (✅), `rho-coding` gains no ratatui dep (✅ — the
dependency edge is `rho-tui → rho-coding`, enforced by Cargo).

### Immediate-mode journal: the async borrow seam

The load-bearing design decision in `app.rs`. `CodingSession::prompt()` returns a
`Stream<Item = CodingSessionEvent>` that borrows `&mut session` for its entire
lifetime. That makes two things impossible while a turn streams: calling a second
`&mut session` method (e.g. cycling the model), and calling `&self` control methods
like `cancel()` on the *same* borrowed session. tau never hits this — Textual runs
the prompt as an async worker and the GIL serializes access. rho makes the seam
explicit two ways:

1. **`HarnessControl`** (`rho-agent`): a cheap, cloneable handle over the harness's
   `Arc`-backed shared state (cancel signal, running flag, steering/follow-up
   queues, clock). `App` clones one before the turn, so `cancel` / `steer` /
   `follow_up` work concurrently with the borrowing stream. No ratatui dependency
   crosses into `rho-coding`/`rho-agent`; this is a general control surface.
2. **`ChromeSnapshot`**: session-derived render facts (status line + sidebar) are
   captured *before* the turn. During the turn `render()` reads the snapshot, and
   the turn loop splits disjoint `&mut` field borrows off `App` (scoped in a block
   so they drop before the post-turn `refresh_chrome()`), so the render never
   borrows the session the stream holds. Chrome is refreshed after the turn ends —
   a one-frame staleness in the token counter during a run, matching tau closely
   enough (tau refreshes chrome on events, not on the 0.15 s animation tick).

`render()` is a free function over an explicit `RenderCtx<'_>` of borrows (never the
session) precisely so the same code path serves both the idle loop and the
turn loop.

## Deferred / honest ledger

(Filled in as teammates report. Each entry: what, why deferred, where it lands.)

- **Extension screens → M7.** Modal overlay for extension-provided screens is stubbed
  with a clear "extensions land in M7" error. `TuiState` carries the
  `custom_renderer`/`tool_call_renderer`/`tool_result_renderer` resolvers for
  structural parity but never installs them in M5; `resolve_*` falls back to generic
  text, exactly as tau does before its extension runtime connects.
- **Autocomplete rendering snapshots.** `tests/autocomplete.rs` covers all trigger /
  selection logic (29 tests) but defers the two `render_completion_suggestions` cases
  to the widget snapshot suite (item 3/7).
- **tau → rho rebrand divergence.** `terminal_title.rs` marks the title with `ρ` and
  gates on `RHO_TERMINAL_TITLE` (tau uses `τ` / `TAU_TERMINAL_TITLE`), matching rho's
  product identity. Intentional divergence.
- **5 pre-existing `clippy -D warnings` lints** in salvaged modules
  (`state.rs:554,750,758`, `terminal_title.rs:48`, `theme.rs:607`) — let-else ×2,
  identical match arms ×2, boolean simplify ×1, f64 cast ×1. To be resolved at the
  M5 DoD gate before the PR opens.

## Parity checklist

To be checked against tau's source for each theme before PR; flag anything needing
live-terminal verification by the human.

- **Layout regions:** transcript / input composer / autocomplete popup / status bar /
  footer-hints — _[ ]_
- **Keybindings (all `app.py` BINDINGS):** cancel/steer, follow-up queueing,
  `/commands`, ctrl-keys (command-palette, session-picker, model/thinking cycle &
  toggles, copy, quit) — _[ ]_
- **Colors per theme** (`tau-dark` / `tau-light` / `high-contrast`): parsed from tau's
  exact hex strings into ratatui `Style` — _[ ]_
- **Modal flows:** session picker, tree picker, login provider/method, branch-summary
  instructions, command output, model/thinking pickers, quit-confirm — _[ ]_

## Review round (bots)

_To be filled at PR: each Codex + CodeRabbit thread with FIX-with-SHA or
REBUT-with-tau-evidence. Byte-compat with tau is the arbiter._
