---
title: "Phase 4b (dispatch 1): CodingSession core & session persistence"
---

M4a made `rho` a program you can *run* against a bare `AgentHarness`. M4b wraps
that harness in tau's `CodingSession`: the durable, resumable, self-compacting
session environment. This dispatch (1 of 2) lands the **session core** — the
persistence lifecycle, session-owned events, compaction, branch summaries,
thinking plumbing, and the session-backed `rho -p` CLI. Dispatch 2 adds the
provider catalog, slash/terminal commands, skills, OAuth, and HTML export.

## What `CodingSession` owns

`AgentHarness` owns the in-memory agent brain (messages + the loop).
`CodingSession` owns the *environment*: an append-only JSONL transcript, the
default coding tools, the compaction/branch/thinking machinery, and the
session-owned event stream that joins the harness stream.

The port lives in `rho-coding`:

| Module | tau source | Role |
|--------|-----------|------|
| `paths.rs` | `paths.py` | `RhoPaths`: `~/.rho` root, tau's slug + `sha256(cwd)[:6]` layout |
| `session_manager.rs` | `session_manager.py` | `SessionManager`: `index.jsonl`, listing, resume records |
| `resources.rs` / `context.rs` | `resources.py` / `context.py` | resource paths + project-context (`AGENTS.md`) discovery |
| `context_window.rs` | `context_window.py` | token estimator, compaction planning, summarization prompts |
| `branch_summary.rs` | `branch_summary.py` | model-assisted abandoned-branch summaries |
| `thinking.rs` | `thinking.py` | thinking-level validation + cycling |
| `diagnostics.rs` | `diagnostics.py` | agent-call failure JSONL logging |
| `session.rs` | `session.py` (2571 LOC) | the `CodingSession` itself |

## The durable-message boundary

The central invariant (tau `_persist_messages_since`): **`MessageEndEvent` is the
commit point.** As `prompt()`/`continue_()` drive the harness stream, each
completed message is, at its `MessageEnd`, appended as a `MessageEntry` on the
active parent chain, immediately followed by a `LeafEntry` pointing at it. The
leaf lets tree navigation observe the current branch *while a run is still in
flight*. `_refresh_persisted_state` then re-replays the log at the new leaf so
`SessionState` (and the cached context estimate) stay consistent.

Two deferral rules matter for byte-parity:

- **Empty sessions defer their file.** A fresh session's `SessionInfo`,
  `ModelChange`, and `ThinkingLevelChange` entries are held in
  `pending_initial_entries` and flushed only on the first real durable write
  (`_ensure_session_initialized`). So `load()` of a never-used session touches no
  disk — the integration test asserts the transcript file does not exist until
  the first prompt.
- **The system prompt is never persisted.** It is rebuilt at `load()` from cwd +
  tools + discovered context, so a resumed session picks up the *current*
  environment rather than a stale snapshot.

## Determinism: threading Clock + IdGen (no monkeypatch)

tau makes session writes reproducible by monkeypatching `time`/`uuid4` at their
definition sites. rho has no monkeypatch, so `CodingSessionConfig` carries an
`Arc<dyn Clock>` and `Arc<dyn IdGen>` (the same seam M2 gave the harness). Entry
construction goes through the `EntryType::new()` constructors (which stamp a real
uuid/time) and then **overwrites** the public `id`/`timestamp`/`parent_id` fields
with the injected values — the discarded uuid never advances the injected
`SequentialIdGen` counter, so ids come out `0,1,2,…` in construction order,
matching tau's `_CounterUUID`. Message timestamps inside a `MessageEntry` come
from the harness (built `with_clock(config.clock)`), while the prompt
`UserMessage`, interrupted-tool-repair `ToolResultMessage`, and summarizer prompts
are stamped from the same clock. Result: a pinned session byte-matches, and the
integration test asserts the first entry id is `0`×32.

## Session-owned events

`events.rs` gained the three variants M4a deferred — `CompactionStart`,
`CompactionEnd`, `EntryAppended` — plus round-trip `Deserialize`, so the ten
`fixtures/wire/session-events/` goldens (skipped since M0 as coding-layer types)
now pass byte-for-byte in `tests/session_events_golden.rs`. `CompactionEnd`
serializes `result`/`errorMessage` with `exclude_none` and always emits
`aborted`/`willRetry`, matching tau. A `session_own_from!` macro provides
`From<Event> for SessionOwnEvent`/`CodingSessionEvent` so `session.rs` reads
cleanly.

`prompt()` wraps the harness's `AgentEnd` into `SessionAgentEnd` (adds
`willRetry`) and emits `agent_settled` at the end of every settled turn; the
overflow path emits `compaction_start` → `compaction_end` →
`auto_retry_start` → (retry) → `auto_retry_end`. The observable `rho -p -o json`
stream therefore now ends with `agent_end`/`agent_settled` rather than the bare
harness `agent_end` — which is exactly the extra surface M4a's `phase-4a.md`
predicted would join the stream.

## Compaction

Ported faithfully: `_recent_preserving_compaction_plan` walks the active context
rows backwards accumulating `DEFAULT_COMPACTION_KEEP_RECENT_TOKENS`, then snaps
the boundary to a user-message start (tau `_first_recent_context_index`); the
prefix is summarized (model call, `SUMMARIZATION_SYSTEM_PROMPT`) and written as a
`CompactionEntry` with `replaces_entry_ids`, after which the harness messages are
replaced by the re-replayed state. Auto-compaction runs before and after each
prompt; a post-response context-overflow error triggers the
`compaction → single retry` path. The chars/4 estimator uses `str(dict)` via
`pystr::python_repr` for `arguments`/`input_schema` (plan risk #2), preserving
tau's exact token counts.

## Dispatch-2 collapse points

Because `provider_settings`/`runtime_provider_config` are always `None` in this
dispatch, every provider-catalog branch takes its default: `available_*` returns
the single configured provider/model, `available_thinking_levels` is the full
`THINKING_LEVELS` set, `context_window_tokens` is
`DEFAULT_CONTEXT_WINDOW_TOKENS`, and `_refresh_runtime_provider` /
`_persist_*_choice` are no-ops. Extensions are elided entirely: input hooks pass
through and session-owned events are emitted directly rather than mirrored to an
extension bus (the WASM runtime is M7). `handle_command`, `reload`, `export`,
`set_model`/`set_provider`/scoped-model cycling, skills, and prompt templates are
**dispatch 2** — `expand_prompt_text` is currently the identity and the matching
tau tests are deferred with it.

## The streaming API in Rust

tau's `prompt()` is an `async def … yield`. rho returns
`impl Stream<Item = CodingSessionEvent> + '_` built with `async_stream::stream!`,
capturing `&mut self`. This works because the harness stream is owned/`'static`
(interior mutability behind an `Arc`), so the session can poll it *and* call its
own `&mut self` persistence methods between events without a borrow conflict —
the caller simply cannot touch the session until the stream is dropped, which is
exactly tau's single-consumer contract.

## CLI wiring

`rho -p` now drives `run_session_print_mode`: it builds a `CodingSession`
(JSONL storage when `--session <path>` is given, else an in-memory
`MemorySessionStorage` so a one-shot print leaves no files) and renders the full
`CodingSessionEvent` stream. `--fake` still works, now session-backed. The M4a
bare-harness `run_print_mode` is retained because `tests/crosscheck.rs` (v1)
pins the harness stream directly.

## Tests & oracles

- `tests/session_events_golden.rs` — the ten `wire/session-events` goldens.
- `tests/coding_session.rs` — durable-message persistence + deterministic ids,
  two-prompt parent-chain advance, thinking persistence/replay, new-session
  indexing on first write, and **resume parity for every
  `fixtures/sessions/*.jsonl`** (load through `CodingSession` replays to the same
  transcript a direct `SessionState` reconstruction gives — the rho side of the
  resume-swap check).
- All prior goldens (`golden_roundtrip`, `crosscheck` v1, `system_prompt`) stay
  green.

## Crosscheck v2 — sessions are byte-interchangeable on disk

The milestone's core oracle. Where v1 (`tests/crosscheck.rs`) pins the bare
harness event stream, v2 (`tests/crosscheck_v2.rs`) drives the full
`CodingSession` on **both** sides and proves the two implementations produce
*and* resume the same session files.

The enabling fact: tau's `patch_determinism` (`_CounterUUID` → `{n:032x}`, plus
`_FIXED_MESSAGE_TIME`/`_FIXED_ENTRY_TIME`) is **numerically identical** to rho's
`SequentialIdGen` + `FixedClock::fixture`. With a pinned `cwd`
(`/rho-crosscheck-cwd`, the one otherwise environment-dependent field —
`session_info.cwd`) the raw session JSONL files are **byte-for-byte identical**
across tau and rho. So the interchange artifact is literal: one committed file
per scenario under `tools/crosscheck/sessions/`, which both implementations
write identically and each can resume.

`tools/crosscheck/driver.py` grew a v2 section that drives tau's `CodingSession`
through four scenarios — **text, tool, compaction (manual, with a
`CompactionEntry` + `replaces_entry_ids`), and branch (mid-history
`branch_to_entry`)** — resetting the id counter per scenario, and emits three
artifacts each: the raw `sessions/<name>.session.jsonl`, the normalized
`expected/v2/<name>.events.jsonl` event stream, and the resume `(role, text)`
oracle `expected/v2/<name>.state.jsonl`.

`tests/crosscheck_v2.rs` (CI-runnable, no `uv`) reproduces all three per
scenario: rho's writer must match the session file **byte-for-byte**, its
normalized `CodingSessionEvent` stream must match, and it loads the committed
(tau-written) file and asserts the replayed transcript — the **tau → rho
resume-swap**. The **rho → tau** direction is `tools/crosscheck/resume_swap.py`:
tau loads each committed (rho-byte-identical) session file and must replay to the
same state; it runs under `#[ignore]` (`crosscheck_v2_resume_swap_rho_to_tau`,
shelling `uv`) and in `just crosscheck`. `just crosscheck` now runs the full
pipeline: regenerate tau side → rho v1 → rho v2 (files + streams + tau→rho) →
rho→tau resume-swap.

## Deferred / remaining (honest ledger)

- **The full `test_coding_session.py` port.** ~40 of its 90 cases exercise
  dispatch-2 surface (set_model/switch-provider/scoped models, skills, prompt
  templates, commands, export, reload). Those are deferred with their subsystems;
  the session-core cases are covered by `tests/coding_session.rs`.
- `--resume <id>` (indexed-session resume in print mode) and the
  `providers`/`sessions`/`export` subcommands land with dispatch 2's CLI surface.

## Review round (bot findings)

Codex surfaced two real P1 bugs, both fixed: an explicit root leaf must replay
to the *empty* pre-root context (tau `from_entries(entries, leaf_id=None)`), not
a linear replay of the abandoned log; and `persist_messages_since` must
propagate storage errors (returning `Result`, aborting the turn) rather than
returning a stale count that re-appends an already-durable message. CodeRabbit
added two more fixes — `RhoResourcePaths::default` now honors `$RHO_HOME` via
`RhoPaths::default`, and the `json.dumps` port renders floats through Python's
`float.__repr__` (`1e-7` → `1e-07`).

Three CodeRabbit suggestions were **rebutted as deliberate tau-parity choices**
(byte/behavior parity is the arbiter, AGENTS.md):

- **No `session_id` path validation.** tau's `prepare_session` uses
  `record_id = session_id or uuid4().hex` as a path component with no
  sanitization; session ids are internally generated, never user-supplied.
- **Non-atomic, unlocked index writes.** tau's `_write_index` is a plain
  `path.write_text(content)` — no flock, no temp-rename. rho mirrors it. If tau
  hardens the index writer, rho follows.
- **`will_retry=False` on the overflow `agent_end`.** tau hardcodes it in the
  main loop (`session.py:1506-1507`), before the overflow branch; the pending
  retry is signaled by `auto_retry_start`/`auto_retry_end`, not this field.
- **The silent harness-startup guard** (`let Ok(events) = prompt_message(...)
  else { return }`). `prompt()` checks `harness.is_running()` immediately before
  `prompt_message`, and the only `HarnessError` is `AlreadyRunning`, so the arm
  is unreachable on the prompt path (single-threaded async, no interleaving);
  `continue_`'s guard is reached only from the internal overflow-retry after the
  loop has drained. tau likewise does not emit a special terminal event there —
  `events = self._harness.prompt_message(...)` just proceeds — so rho matches its
  control flow. (Note: the *separate*, real persist-failure path does now
  surface an error — see below.)

Note the persistence-failure path is **not** a rebuttal: `persist_or_log` now
records the error on the session and the print-mode CLI exits non-zero, matching
tau's re-raise (`session.py:1575-1581`).
