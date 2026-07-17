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
| 1 | `state.py` (547) | `TuiState` (`state.rs`, 877 ln) | ✅ Salvaged (committed) |
| 2 | `adapter.py` | `TuiEventAdapter` (`adapter.rs`, 153 ln) — the parity-critical seam | ✅ Salvaged (committed) |
| 3 | `widgets.py` (1744) + `app.py` (5819) | ratatui widgets (`widgets.rs`) + app/event-loop (`app.rs`) | 🟡 widgets: in progress (`m5-widgets`); app: pending (`m5-app`) |
| 4 | modals (in `app.py`) | overlay enum (`modals.rs`) | 🟡 in progress (`m5-modals`) |
| 5 | `autocomplete.py` (511) | `build_completion_state` (`autocomplete.rs`, 650 ln) | ✅ Salvaged (logic); rendering snapshots → item 7 |
| 6 | `config.py` themes + `terminal_title.py` | `theme.rs` (778) + `terminal_title.rs` (309) | ✅ Salvaged (committed) |
| 7 | binary wiring | `rho` no-`-p` → TUI; all `app.py` BINDINGS; resume flags | ⏳ pending (`m5-wire`) |
| 8 | `cli.py` TUI flags (deferred from M4b) | `--extension`/`-x` parse + M7 notice; `--resume`/`--new-session` `BadParameter` | ⏳ pending (`m5-wire`) |

Definition-of-Done items: port `test_tui_adapter.py` + state tests (✅ done, 39
passing), insta snapshot suite (⏳ in progress per-widget/modal; full-app FakeProvider
drive pending), parity checklist (below), `cargo test --workspace` + `clippy -D
warnings` + `fmt` clean, `rho-coding` gains no ratatui dep.

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
