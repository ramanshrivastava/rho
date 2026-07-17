---
title: "Phase 4a: Coding Tools, difflib, and the First Runnable Agent"
---

Phase 4a is the milestone where `rho` becomes a program you can *run*. M0–M3
built the wire types, the loop/harness, session storage, and the six providers,
but nothing tied them into a coding assistant. M4a lands the first vertical
slice of `rho-coding`: the four built-in tools (`read`/`write`/`edit`/`bash`),
the deterministic system-prompt assembly, the three print-mode renderers
(`text`/`json`/`transcript`), a harness-driven `run_print_mode`, and a real
`rho` binary. After this, `cargo run -p rho -- --fake -p "…"` drives the agent
end-to-end offline, and `rho -p` against `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
talks to a live model.

The correctness bars this milestone adds:

- **System-prompt golden** — rho's `build_system_prompt` for the default tool set
  is byte-identical to tau's (`fixtures/system-prompt/default_coding_tools.txt`,
  extracted by `extract_system_prompt.py`).
- **Crosscheck v1** — three scripted fake-provider sessions replayed through
  rho's `JsonEventRenderer` path match tau's normalized event streams
  byte-for-byte (`tests/crosscheck.rs` ⇔ `tools/crosscheck/`).
- **Ported behavior tests** — `test_coding_tools.py`, `test_rendering.py`, and
  `test_system_prompt.py` are ported to Rust; the truncation boundaries, edit
  match/no-match/duplicate semantics, image MIME detection, bash cancellation
  (process-group kill), and renderer output shapes all reproduce tau.

## Scope: a harness slice, not a `CodingSession`

tau's `run_print_mode` builds a full `CodingSession` — session persistence,
slash/terminal commands, project-context (`AGENTS.md`) discovery, skills,
extensions, the provider catalog, and OAuth. All of that is **M4b**. M4a's
`run_print_mode` is deliberately a thin slice: it builds the coding tools + the
system prompt, drives an `AgentHarness` with one prompt, and renders the harness
event stream. The event stream it emits is therefore *exactly* the harness
stream — which is the whole reason the crosscheck oracle (generated from tau's
`AgentHarness`, not its CLI) is the right target. When M4b wraps a real
`CodingSession` around this, the extra session-owned events (`entry_appended`,
`agent_settled`, compaction, …) join the stream; the `CodingSessionEvent` union
and the renderers already accommodate them (they are serialize-only for now).

## Subprocess + process groups: tokio vs asyncio

tau's `bash` tool runs a shell with `asyncio.create_subprocess_shell(...,
start_new_session=True)` and, on timeout/cancel, `os.killpg(pid, SIGKILL)` to
reap the shell **and** its pipeline/compound children. Porting this to tokio hit
three constraints and one classic deadlock:

1. **No `unsafe`** (workspace forbids it), so `libc::killpg` is out. `nix`'s
   `killpg` is a safe wrapper — that's the one unix dependency this milestone
   adds. The child is placed in its own process group with the *safe* std
   `CommandExt::process_group(0)` (a new pgid == the child pid), so
   `killpg(child_pid, SIGKILL)` kills the group. This is `setpgid`, not tau's
   `setsid` (`start_new_session`); the difference (session leadership) is
   irrelevant to `killpg`, which only needs the group.

2. **fd-level stdout/stderr merge.** tau's `stderr=STDOUT` dups fd 2 onto fd 1 so
   the combined output preserves real interleaving order. std/tokio can't merge
   two `Stdio`s, so rho gives the child a single `os_pipe` as *both* stdout and
   stderr (one writer, `try_clone`d). This reproduces the byte-exact merge
   without `unsafe`.

3. **The os_pipe deadlock.** `Stdio::from(writer)` moves the write ends into the
   `Command`, and `spawn` only *dups* them into the child — the parent's copies
   stay open inside the `Command` until it is dropped. Until every writer fd is
   closed the reader never sees EOF, so the blocking `read_to_end` hangs forever.
   The fix is a single `drop(cmd)` right after `spawn` (every bash test hung
   until this landed). The read itself runs on `spawn_blocking`; a `tokio::select!`
   races it against the timeout and a 50 ms cancellation poll, mirroring tau's
   `asyncio.wait(..., FIRST_COMPLETED)`.

Exit-code parity: tau reports `-SIGKILL` (i.e. `-9`) as the `returncode` of a
killed process; rho reproduces this via `ExitStatusExt::signal()` when
`code()` is `None`. The truncation full-output spill keeps tau's `tau-bash-*.log`
prefix so a rho bash result is indistinguishable from tau's. The non-unix path
(no process group, no `killpg`) is stubbed — development targets unix, and M5's
TUI work will revisit Windows.

One documented non-port: the task brief mentions "incremental output streaming
via `on_update`", but tau's actual `tools.py` bash executor buffers the whole
output through `_communicate_with_cancellation` and never calls `on_update`. rho
matches tau's real behavior (no streaming), which is what parity requires.

## difflib, ported 1:1

`edit`'s result `details` carry an ndiff-style `diff` and a unified `patch`
string, produced by CPython's `difflib.ndiff` / `unified_diff`. There is no
shortcut to byte-parity here: `ndiff` runs `SequenceMatcher` at the line level,
then for replaced blocks recurses to a *character-level* `_fancy_replace` that
emits the `? ` guide lines with `^`/`-`/`+` markers. So `tools/difflib.rs` is a
faithful transliteration of the slice CPython's `difflib.py` uses:
`SequenceMatcher` (with `autojunk`, `find_longest_match`, `get_matching_blocks`,
`get_opcodes`, `get_grouped_opcodes`, and the three `ratio`s), plus `unified_diff`
and the `Differ` machinery (`_fancy_replace`, `_plain_replace`, `_fancy_helper`,
`_qformat`, `_keep_original_ws`). It is generic over a hashable element type so
the same code serves the line diff (`elements = lines`) and the intraline char
diff (`elements = chars`, junk = space/tab). The port is pinned by unit tests
whose expected strings were extracted from tau via `uv run` (simple/insert/delete
plus a `_fancy_replace` case with the `? ` guide lines). The terse
`alo/ahi/blo/bhi` names and `for i in range(...)` index loops mirror CPython
exactly, so a few pedantic clippy lints (`similar_names`, `needless_range_loop`)
are allowed module-wide rather than "idiomatized" into a divergent shape.

Matching semantics (`apply_edits_to_normalized_content`) are the byte-critical,
user-visible part and are ported precisely: LF normalization for matching, BOM
preservation, unique + non-overlapping validation *before* any write (so a failed
edit leaves the file untouched), reverse-order application, and the exact error
strings (`Could not find edits[1] in …`, `Found N occurrences …`, etc.).

## System-prompt golden methodology

The system prompt is deterministic given a tool set, date, and cwd. Only the
trailing `Current working directory:` line depends on cwd (the tool snippets and
guidelines are static metadata), so the golden pins a fixed cwd
(`/tmp/rho-fixture-cwd`) and date (`2026-06-17`). `extract_system_prompt.py`
writes tau's exact output; `tests/system_prompt_golden.rs` rebuilds it with the
same inputs and asserts equality. This slots into `just refresh-fixtures` via
`run_all.py`, pinned to `fixtures/TAU_REV` like every other fixture — nothing
else was regenerated.

## Crosscheck v1: making a timing-fragile oracle deterministic

The M0 crosscheck skeleton generated its expected streams from tau's real
`AgentHarness` under a **wall clock**. The normalizer collapses volatile ids and
timestamps to `<id:N>` / `<ts:N>` position tokens — but the *number* of distinct
`<ts:N>` tokens depended on whether two wall-clock reads landed in the same
millisecond. Empirically tau reproduced its own tokens (`text`: 1 ts, `tool`: 3,
`multiturn`: 2), because the scripted assistant messages are built once up front
and the first scenario runs microseconds later (collision) while later ones don't.
That is not a property a *Rust* re-implementation can reproduce: rho's timing is
its own.

The fix makes both sides deterministic. `driver.py` now calls
`patch_determinism()` (the same frozen clock the fixture extraction uses), so
every timestamp is `1_700_000_000_123` ms and normalizes to `<ts:0>` in every
scenario. rho's `tests/crosscheck.rs` uses `FixedClock::fixture()` (the identical
value) for the harness and stamps its scripted assistant messages with the same
constant. The streams are then language-independent, and the crosscheck validates
what it should: event **structure, order, content, and ids** — not wall-clock
collisions. Only `tool.jsonl` / `multiturn.jsonl` changed (their extra ts tokens
collapsed to `<ts:0>`); `text.jsonl` was already single-token.

The rho side runs through the *same* serialization as `rho -p --output-format
json` (the `JsonEventRenderer` == `serde_json::to_string` per `CodingSessionEvent`)
and reuses a Rust port of `normalizer.py`. It is a CI-runnable test needing no
`uv`/tau (the expected files are committed); `just crosscheck` additionally
regenerates the tau side to confirm the oracle has not drifted.

## The renderer test seam

tau's renderers write to typer/`rich` module globals, tested with pytest's
`capsys`. rho gives each renderer an injectable `Sink` (`Box<dyn Write + Send>`,
defaulting to real stdout/stderr) so the ported `test_rendering.py` cases can
capture output into a shared buffer — the same observable behavior, made testable
without a global-capture hack. The transcript renderer writes plain (un-styled)
stderr, matching what `rich` emits to a non-TTY (which is what the tau tests
observe). Custom-message rendering (extensions) is the one transcript branch left
for M4b.

## Deferred to M4b

`CodingSession` and everything it owns: session persistence + `--session`,
slash/terminal commands (`/system`, `!`/`!!`, `/skill:…`), project-context
discovery, skills loading, extensions/`StderrUiBridge`, the provider
config/catalog + `resolve_provider_selection` (M4a uses a minimal env-based
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` path), OAuth, HTML export, and the
`providers`/`setup`/`sessions`/`export` subcommands. Interactive TUI is M5. The
`rho` binary stubs the no-prompt path with an explanatory message.
