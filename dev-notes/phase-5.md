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
- **Clippy `-D warnings` on salvaged modules** — the salvage's pre-existing lints
  (plus the new app/modal code) were all resolved at the M5 DoD gate;
  `clippy --workspace --all-targets -D warnings` is clean.

## Salvage audit (GLM-authored scaffold, audited as untrusted)

The `c8ffd7a` scaffold (`state.rs`, `adapter.rs`, `autocomplete.rs`, `theme.rs`,
`terminal_title.rs`, `pystr.rs`) was partially authored by a weaker model (GLM 5.2)
via a different harness. It was re-audited line-by-line against the tau source and
against rho's M1–M4b idioms. Verdict: **high-fidelity overall** — the parity-critical
logic was correct — with a handful of edge-case fixes. Recorded honestly as the
GLM-vs-Opus quality delta:

**Kept as correct (verified faithful):**
- `adapter.rs` — the whole event seam: assistant-buffer flush, `agent_settled`,
  error/abort mapping, the `text or buffer` / empty-string-→`"Error"` truthiness, and
  the `Custom.details` dict-guard all match `adapter.py` exactly. The ported
  `test_tui_adapter.py` assertions are equal-strength (exact expected strings), not
  weakened.
- `state.rs` formatters (`format_tool_call/result_block`, `format_elapsed`,
  `apply_tool_spinner`, `_preview_text`, `read_line_suffix`) — byte-identical,
  including all the Python-truthiness / `int()`-truncation / codepoint-slice traps.
- `autocomplete.rs` — the full dispatch + every helper (sort-key tuples, byte-offset
  `apply()` with no multibyte panic, shell `!`/`!!` handling) is a faithful 1:1 port;
  tests are equal-strength.
- `theme.rs` — all three palettes match `config.py` hex-for-hex across every field
  and role in declared order; keybindings + `to_json` order exact.
- `terminal_title.rs` — sanitize/truncate codepoint logic faithful (τ→ρ rebrand aside).
- `pystr.rs` (rho-tui's copy) — `splitlines` uses the **identical** 10-char CPython
  boundary set as rho-coding's `pub(crate)` `pystr` (a zero-copy `&str` variant).
  rho-coding's is unreachable from rho-tui, so a minimal documented copy is the right
  layering call (same rationale as the accepted split); **not** a divergent impl.

**Rewrote / fixed (GLM defects):**
- `autocomplete.rs` — the salvage used `trim_start_matches`/`trim_end_matches`
  (strip *all* repeated copies) at 7 sites where tau uses `removeprefix`/`removesuffix`
  (strip *one*). Rewrote to `strip_prefix/strip_suffix(...).unwrap_or(...)`. Concrete
  divergence on `/skill:/skill:x`-style input; locked with a regression test.
- `state.rs` — the salvage **hand-rolled its own `python_repr`** for the fallback
  tool-call invocation, diverging from rho-coding's canonical one (missing Python
  float-exponent form `1e+20` and control-char `\xNN`/`\uNNNN` escaping). Deleted the
  copy and reused rho-coding's tested `python_repr` (now re-exported `pub`) — one
  canonical impl, no divergence, per the audit directive.
- Added the missing on-disk `save_tui_settings`/`load_tui_settings` roundtrip test
  (`test_tui_config.py` had 4; rho had 0) plus a keybindings sub-object key-order guard.

**Accepted rare-input deviations (documented, not fixed):**
- `format_g` (bash `timeout` display) diverges from C `%g` only for absurd magnitudes
  (≥1e6 s ≈ 11 days, or <1e-4 s); ordinary seconds match.
- `str.lower()` final-sigma (`Σ→σ` vs Rust `Σ→ς`) and `0x1c`–`0x1f`-as-whitespace
  (`str.isspace` vs Rust `White_Space`) — Unicode/control-char inputs only, pre-sanitized
  in practice.
- Skill-path matching uses lexical normalization, not symlink `resolve()` (documented
  in the code); only bites a symlinked skill path.

## Parity checklist

Checked against tau's source. Items needing **live-terminal** human verification are
flagged `[HUMAN]` (immediate-mode render output can't be fully asserted headlessly).

- **Layout regions:** transcript / queued / input composer / status line / autocomplete
  popup / sidebar / footer-hints — _[x]_ (snapshot-tested; live proportions `[HUMAN]`).
- **Keybindings (all `app.py` BINDINGS):** Enter submit / Shift+Enter newline, steer +
  follow-up queueing, cancel, `/commands` palette, session-picker, model/thinking cycle,
  tool-results/thinking toggles, clear, quit — _[x]_ (dispatch unit-tested via
  `matches_binding`; live key delivery `[HUMAN]`).
- **Colors per theme** (`tau-dark` / `tau-light` / `high-contrast`): hex parsed into
  ratatui `Style` — _[x]_ (hex parity unit-tested; on-screen color `[HUMAN]`).
- **Modal flows:** session / tree / model / scoped / theme pickers, branch-summary
  instructions, command output, M7 notice — _[x]_ (navigation + outcomes unit-tested,
  render snapshot-tested). Login/OAuth/extension modals → M7. No quit-confirm or
  thinking-picker modal (they do not exist in tau's source — confirmed).
- **Live end-to-end drive** (fake + real provider) — _[HUMAN]_ (raw-mode TTY can't run
  headless; `rho --fake` launch + non-tty error paths verified programmatically).

## Review round (bots) — PR #9

First pass: CI green (clippy `-D warnings`, rustfmt, test on ubuntu + macos) and
**21 inline findings** from Codex (4) + CodeRabbit (17). All were legitimate — almost
all in the newly-written `app.rs`, plus 4 in the salvaged `transcript.rs` renderer
(consistent with the untrusted-salvage framing). **20 fixed in `6dd76e1`, 1 rebutted:**

- **Fixed (correctness/robustness):** event-loop + turn-loop busy-loop on stream
  close (break/propagate); transcript bottom-follow scroll; resume seeds via
  `load_messages` (was dropping tool/thinking/summary shapes); `!`/`!!` routed to
  `run_terminal_command`; per-turn `HarnessControl` (dropped the stale cached field);
  `take_run_error` surfaced post-turn; completion-popup height counts rendered rows;
  session-mutation `Result`s surfaced (notice on failure, mutate only on success);
  `matches_binding` BackTab ≠ plain Tab (+ regression test); scoped-model editor opens
  on the full list; git-branch 500 ms timeout reinstated; two-column exact-fit
  (no spurious ellipsis); read-range widened to i128; malformed `tui.json` warns;
  transcript live-update text, `$ ` spacing, patch-marker newline, cell-width code
  truncation.
- **Rebutted (parity):** unbounded file-reference traversal / directory-symlink loop
  — tau's `_iter_file_reference_paths` (autocomplete.py:187–205) is the identical
  unbounded stack-DFS with symlink-following `is_dir()` and post-traversal cap, so rho
  matches it exactly; byte-compat with tau is the arbiter, and skipping symlinks would
  diverge from tau's output.

_Original template retained below._

_To be filled at PR: each Codex + CodeRabbit thread with FIX-with-SHA or
REBUT-with-tau-evidence. Byte-compat with tau is the arbiter._
