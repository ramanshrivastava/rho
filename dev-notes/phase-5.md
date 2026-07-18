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

### Adversarial review round (team lead)

A second, adversarial pass found issues the bot pass and the first fix commit had
not yet covered (the reviewer read a pre-`6dd76e1` tree for some, but three were
genuinely new). `HarnessControl` (rho-agent) was independently verified clean
(purely additive, token identity preserved) and stays as-is.

**Fixed (criticals):**
- **C1 — empty code-fence panic** (`transcript.rs::render_fenced_body`): an empty
  ` ```\n``` ` matched the opening line's own newline as the closing fence, so the
  code slice `text[fence_line_end+1..closing]` was a reversed range → panic on any
  body (tool/skill/user/error). Now searches for the closing fence from
  `fence_line_end+1` via `get(..).find`, so `closing >= code_start` always.
  Regression test with an empty fence in a tool result.
- **C2 — terminal never restored on panic** (`app.rs`): added a chained panic hook
  **and** a `TerminalGuard` RAII drop, so any unwind (e.g. in render) restores raw
  mode / alt screen / mouse capture before the process dies. Test: an RAII guard's
  `Drop` runs during `catch_unwind`.
- **C3 / F-Stale / F-Seed** were already fixed in `6dd76e1` (transcript
  bottom-follow scroll + overflow snapshot; per-turn `HarnessControl`, no cached
  field; `load_messages`-based seeding) — re-verified on HEAD.

**Fixed (importants):** `!`/`!!` → `run_terminal_command` (F-Bang); Shift+Tab no
longer collides with plain Tab so the thinking-cycle binding lives (F-Bindings, with
regression test); **quit and command-palette stay live during a running turn**
(F-Quit) — `handle_running_key` returns a `Quit` outcome that cancels the run and
exits; code-block truncation by display width (F-Width).

**Ledgered (deliberate scope for M5):**
- **Session-picker during a running turn:** tau *notifies* ("can't switch while
  running") rather than opening it; rho currently ignores the key mid-turn. The
  live bindings that matter for not-stranding-the-user (quit, palette) are wired;
  the mid-run session-picker notification is deferred (cosmetic).
- **Chrome frozen during a turn:** intentional immediate-mode seam trade-off (the
  status token counter is one turn stale during a run; refreshed at settle) — see
  the borrow-seam journal above.
- **Minors:** modal Enter-on-empty-filter, session-picker row format, `format_g`
  `%g` on absurd magnitudes, and the tau-parity file-reference traversal rebuttal
  are all documented above / in the salvage-audit ledger.

### Workspace-test flake investigation (resolved: environmental, not a test bug)

During the M5 PR work two `cargo test --workspace` runs showed a single transient
`test result: FAILED` (≈2 of ~10 early runs), with no capturable failing-test name.
Investigated to a firm conclusion before the merge gate:

- **Not reproducible.** 45 consecutive clean `--workspace` runs on the M5 HEAD
  (`c48ac2c`), including 5 under full 14-core CPU saturation. At the apparent early
  rate (~1/8) the chance of 45 clean runs is `(7/8)^45 ≈ 0.25%` — i.e. that rate was
  not real.
- **Isolated crates are rock-stable**: `rho-tui` 8/8, `rho-coding` 4/4.
- **CI is clean on every commit** (test on ubuntu + macos, every push).
- **Root cause = concurrent working-tree mutation, not a test.** Both early
  transients coincided with a background subagent operating on the same checkout —
  the transcript-fix agent explicitly `git stash`/`git checkout`-ed `app.rs` *while*
  a `cargo test` was in flight. A source mutation mid-run (triggering a rebuild or a
  file-reading test seeing an inconsistent tree) is an artifact of the parallel
  multi-agent workflow, not a defect in any test.

No M5 test is implicated and no reproducible pre-existing test failure was found, so
there is nothing to fix and no actionable flake to file. Tracked here; if CI ever
surfaces a named failure it should be revisited with that name.

- **Named failure surfaced (2026-07): `tools::tests::bash_tool_timeout_kills_shell_children`.**
  A timing-margin test (`duration < 0.5s` bound after a 0.01s timeout) that fails only
  under full `cargo test --workspace` CPU contention; passes 3/3 in isolation. Filed as
  [#11](https://github.com/ramanshrivastava/rho/issues/11) (widen the margin / serialize
  the process-group-kill timing tests); not fixed in the login-required PR, whose diff
  does not touch `tools`.

_Original template retained below._

_To be filled at PR: each Codex + CodeRabbit thread with FIX-with-SHA or
REBUT-with-tau-evidence. Byte-compat with tau is the arbiter._

## M5.5 — TUI polish milestone (owner-sanctioned look/feel divergence)

Owner directive (recorded in project memory): the TUI's strict-parity rule is
**relaxed for look / feel / performance** — the TUI may deliberately diverge from
tau there. **Wire / session / CLI parity stays LOCKED.** Three workstreams,
branch `feat/tui-polish`. Goldens/crosscheck untouched; full workspace + clippy
`-D warnings` + fmt green throughout. The TUI was also driven live under
`rho --fake` in a PTY (splash + rho theme + sidebar/status/footer all render;
prompt submit + clean Ctrl+D quit confirmed) — the `[HUMAN]` live-drive item
above is now machine-smoke-tested for launch/render/input.

### 1. Parity gaps — audited rho-tui against tau `tui/` directly

(A background audit subagent returned prompt-injected content — a fake
`_ext_ai_health_check` payload — and was disregarded; the audit was done by hand
against the tau source.)

**Closed (real behavioral gaps, daily-use):**
- **Hidden-thinking placeholder.** `build_transcript_lines` never honored
  `state.show_thinking`, so Ctrl+T did nothing to the transcript. Now, when
  thinking is hidden, a run of consecutive `thinking` items collapses to a single
  `_HIDDEN_THINKING_PLACEHOLDER` block — exactly tau's `TranscriptView`
  render (`widgets.py:653-669`, placeholder text `widgets.py:200`). Regression
  test `hidden_thinking_collapses_to_single_placeholder`.
- **Up-arrow recalls the last prompt.** Up on an EMPTY composer now recalls the
  most recent submission (tau `action_recall_previous_prompt`, `app.py:4077`),
  before falling through to cursor-up. Test
  `up_recalls_previous_prompt_only_into_empty_composer`.
- **Command-output modal scroll clamp.** The scrollable `$ cmd` output now clamps
  its offset to the last page (tracks the rendered viewport height) instead of
  drifting into blank space on over-scroll. Test
  `command_output_scroll_clamps_to_last_page`.

**Ledgered (deliberate, with rationale):**
- **Edit-queued-message on Up while running** (tau `action_edit_queued_message`,
  `app.py:4091`): pops the latest queued follow-up/steering message back into the
  composer. Deferred — it needs `pop_latest_follow_up` / `pop_latest_steering` on
  `HarnessControl` (they exist on `AgentHarness` but not the cloneable control
  handle the TUI uses mid-turn), i.e. an `rho-agent` change outside this
  milestone's `rho-tui` + TUI-wiring + theme-plumbing scope boundary. The common
  case (recall a submitted prompt) is covered above.
- **Session-picker / model-cycle mid-run notification.** tau *notifies* ("Tau is
  already working. Press Escape to cancel.") when these keys are pressed during a
  run; rho ignores them mid-turn. Deferred (cosmetic) — rho has no transient-toast
  primitive and the immediate-mode borrow seam makes opening a modal mid-turn
  awkward; the not-stranding-the-user bindings (quit, command palette) are live.
- **Session-picker row format.** rho shows `{id}  {title}  {model}`; tau shows
  `{updated_at} - {model} - {title}` (`app.py:5193`). Kept rho's format
  deliberately — rho surfaces the session **id** because resume-from-picker is
  itself deferred and the picker's notice tells the user to run
  `rho --resume <id>`, so the id is the actionable field. Look/feel, sanctioned.

### 2. Performance pass (measured)

- **`TranscriptCache`** (`widgets/transcript.rs`): a fingerprint-keyed memo over
  `build_transcript_lines`. The transcript was rebuilt from scratch every frame
  (each 150 ms spinner tick during a run, each keystroke while composing) —
  O(transcript) markdown parse + word wrap. The fingerprint hashes every input
  that affects the render (per-item role/text/result/update/always-show, the
  `show_*` toggles, `assistant_buffer`, the active `tool_spinner`, theme name,
  width), so an unchanged frame reuses the prior render and there is no manual
  invalidation to drift. Held in a `RefCell` so the immutable-borrow render path
  refreshes it in place.
- **Stream-delta coalescing** (`app.rs::run_turn`): the turn loop drains every
  already-ready stream event (`now_or_never`) before the next draw, so a burst of
  token deltas is one redraw, not one per delta.

Measured (`cargo bench -p rho-tui --bench transcript_render`, criterion medians):

| transcript size        | rebuild (before) | cache hit (after) | speedup |
|------------------------|------------------|-------------------|---------|
| 50 turns (150 items)   | 4.16 ms          | 0.21 ms           | ~20×    |
| 200 turns (600 items)  | 16.4 ms          | 1.07 ms           | ~15×    |
| 500 turns (1500 items) | 43.5 ms          | 2.67 ms           | ~16×    |

The cache-hit cost is now dominated by the `Vec<Line>` clone handed to the
`Paragraph` (a zero-copy render path is a possible future follow-up); the
markdown parse/wrap is skipped entirely on unchanged frames. CI compiles the new
bench but never runs it (same policy as the M6 benches).

### 3. rho identity (sanctioned divergence — rho looks like rho, not tau)

All divergences below are **look/feel only**; none touch a wire/session/CLI byte
or a golden. rho's `tui.json` is rho-local (tau never reads it), so extending its
theme vocabulary is not a parity surface.

- **`rho` theme, now the default.** `TuiThemeName::Rho` + `rho_theme()`: rust-oxide
  accents (ρ `#b3391f`) over warm neutral parchment text on a warm near-black
  ground — the graph-paper spirit adapted to a dark terminal. `"rho"` leads
  `BUILTIN_TUI_THEME_NAMES` in both `rho-tui` and `rho-coding` (kept equal by
  `theme_names_match_rho_coding`); it is the default for a fresh session and for a
  `tui.json` with no `theme` field. The three `tau-*` themes stay fully
  selectable (`/theme`, the theme picker).
- **Greek prompt-prefix spinner.** While a turn runs, the composer prefix cycles
  the π → τ → ρ lineage (`RHO_SPINNER_FRAMES`) instead of tau's braille. This is
  chrome and is deliberately **distinct** from the transcript tool spinner
  (`state::TOOL_SPINNER_FRAMES`), which stays byte-identical to tau — guarded by a
  test asserting the two differ.
- **Welcome splash.** A fresh (empty) transcript shows a centered ρ mark, the
  π → τ → ρ lineage, and "a Rust port of tau" instead of a blank pane
  (snapshot `rho_splash`). tau shows a blank transcript.
- **`--help` lineage.** The binary `long_about` states the π → τ → ρ lineage and
  the tau-compat guarantee.

The idle prompt glyph `ρ` and the `ρ`/`RHO_TERMINAL_TITLE` terminal title were
already rho's (prior commits); this milestone extends the identity to the theme,
the running spinner, the empty state, and the help text.

## M5.6 — TUI delight pass v2 (owner-sanctioned, actively art-directed)

Task #45. Everything below is **look/feel only** and confined to `rho-tui` +
the binary's TUI wiring; no wire/session/CLI byte, golden, or crosscheck is
touched (`cargo test --workspace` + `clippy -D warnings` + `fmt` all green).
The owner explicitly sanctioned rho's TUI diverging from tau in look/feel and
performance for personality; the pieces:

- **First-message latency fix (a real bug).** `App::submit_prompt` now renders
  the user's message **optimistically** (`TuiState::add_optimistic_user_echo`)
  the instant Enter is pressed, before `session.prompt()`'s stream echoes it
  back. On the *first* message that stream does durable-session create +
  `ensure_session_indexed` + turn assembly *before* emitting the user echo, so
  the message appeared to lag. The adapter reconciles the real user `MessageEnd`
  against the pending `optimistic_echo` marker (exact-text match) so nothing
  double-renders. Because `CodingSession::prompt` runs `input` hooks + `/skill:`
  / `/template` expansion **before** emitting the durable user message (Codex PR
  #19 P1), the echo is **self-correcting**: a mismatch (the durable text was
  transformed) *withdraws* the stale raw item (`reconcile_optimistic_user` drops
  the tracked `optimistic_range`) and renders the real message in its place; a
  turn that ends with no user `MessageEnd` at all (a hook *handled* the prompt,
  no agent run) withdraws the orphan at `finish_turn` (`drop_optimistic_echo`).
  Covered by `optimistic_echo_reconciles_without_double_render`,
  `optimistic_echo_transform_withdraws_raw_and_renders_real`, and
  `optimistic_echo_withdrawn_when_turn_handles_without_user_message`.
- **Full-pane splash background (the "half-screen theme" bug).** `render_splash`
  now paints the theme background across the **entire** transcript pane (a
  `Block` fill) before centering its content, killing the black seam above the
  block. Guarded by `splash_fills_entire_pane_with_theme_background` (asserts
  every cell's bg == the theme bg at 4 terminal sizes).
- **The working-state signature.** A new pure motion module (`motion.rs`) with
  two reusable, deterministic primitives driven by the 150 ms activity frame
  (so every animated frame is snapshot-testable at a fixed frame):
  - **throb** — a sine brightness pulse along a rust-oxide ramp (dim terracotta ↔
    `#b3391f` ↔ hot gold): heated iron cooling and reheating, *not* a spinner.
  - **shimmer** — a travelling cosine light-sweep band (a port of Codex's
    `shimmer.rs`, half-width ~5, ~2 s period) blended over the oxide ramp rather
    than grey→white.

  Applied: the composer-prefix **ρ throbs** while running (fixing the
  static-while-running parity gap — the π→τ→ρ frame cycle moved to the splash);
  a shimmering **forge-verb** rotates one-per-turn (`Forging`, `Oxidizing`,
  `Tempering`, … `Reticulating`) on the status row while running, reading
  `Tempering…  ·  2m 14s  ·  esc to interrupt` (Codex-format elapsed timer); and
  the composer **cursor breathes** a quiet oxide throb while idle. All degrade to
  a static dim ρ + plain verb + default cursor under non-truecolor or reduced
  motion (`MotionCaps::from_env`: `COLORTERM`, `NO_COLOR`, `TERM=dumb`,
  `RHO_REDUCED_MOTION`).
- **Idle animation heartbeat.** The idle event loop gains a ~150 ms ticker that
  advances the frame counter **only** on a motion-capable terminal (a 1-hour
  dormant interval otherwise), so the splash lineage, breathing cursor, and
  rotating placeholder animate while idle without ever waking a plain terminal.
- **Heritage splash + bench brag + welcome tips.** The splash centerpiece is the
  animated π→τ→ρ lineage with language labels (`π Pi·TypeScript → τ tau·Python →
  ρ rho·Rust`); the active glyph oxidizes/brightens marching π→τ→ρ (settles on ρ
  under no-motion). Beneath the ρ mark, the **name across scripts** — `ρο · ロー ·
  रो` (Greek · Japanese-katakana · Hindi-Devanagari) — on a cool→hot oxide
  gradient with a shared gentle throb (owner request). The full-pane bg
  regression test skips wide-glyph (CJK) continuation cells, which ratatui resets
  and the wide glyph visually covers; it asserts the theme bg on the full-height
  edge columns instead (never continuation cells) so an unpainted-row seam is
  still caught. A **benchmark brag** line pulls REAL numbers from the
  committed `dev-notes/benchmarks.json` (baked in via `include_str!`, parsed at
  runtime, degrades to no line on malformed data): `ρ · ~302× faster cold start
  than τ · ~21× lighter` (cold-start = `version-direct` variant tau/rho; memory =
  `memory_rss` turns=1 tau/rho). Plus a one-line pitch ("a Rust coding agent,
  oxidized"), a hints row (`/ commands · Ctrl+P model · Ctrl+R sessions · !cmd
  shell · Ctrl+D quit`), and a rotating "· did you know" heritage fact. A
  rotating composer placeholder ("Explain this repo", "Add a test…", "Fix this
  stack trace") cycles while idle.
- **Note on the signature's layout.** The throbbing ρ lives in the composer
  prefix (its natural home, by the input) and the forge-verb + timer + interrupt
  hint on the status row directly below, so the eye reads `ρ / Tempering… · 2m
  14s · esc to interrupt` top-to-bottom rather than as one literal row. This is
  the one deliberate deviation from the spec's single-line mock, chosen so the
  animated glyph stays adjacent to the input and the status row keeps a natural
  home for the verb/timer/hint.

Snapshots: `rho_splash` (regenerated), `working_status_line` (new). The prior
`RHO_SPINNER_FRAMES` (π→τ→ρ composer cycle) is retired; the composer prefix is
now always the ρ glyph, animated by color, and the lineage cycle is the splash's.
