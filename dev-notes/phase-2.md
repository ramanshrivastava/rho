---
title: "Phase 2: The Loop, the Harness, and Async Generators in Rust"
---

Phase 2 makes `rho-agent` *run*. M1 gave us the wire types; M2 adds the behavior
that produces and consumes them: the provider seam, the `FakeProvider` test
double, the `run_agent_loop` state machine, the stateful `AgentHarness`, and the
session-state replay + storage layer. The correctness bar is the same as M1 and
then some: not just that each type round-trips byte-for-byte, but that the
**sequence** of events the loop emits is byte-identical to what tau emits for the
same scripted provider run. The `fixtures/event-streams/` goldens are that oracle,
and all six pass byte-for-byte.

This note records the handful of places where a faithful port required a real
translation decision — async generators, polled cancellation, the shared message
list, tool-error isolation, and the two M1 defaults M2 had to restore.

## Async generators → `async-stream`

tau's loop is an `async def … yield` generator: it `await`s the provider stream
and tool futures, and `yield`s `AgentEvent`s in between. Rust has no native async
generators, so the loop is written with the [`async-stream`] `stream!` macro,
whose `yield expr;` + `.await` desugar to exactly that cooperative state machine.
The returned value is an `impl Stream<Item = AgentEvent>`.

Two structural consequences:

- **No `yield from`.** tau factors the per-turn work into helper sub-generators
  (`_assistant_events`, `_execute_tool_call`) and drives them with
  `async for event in _helper(): yield event`. `stream!` can't delegate to another
  generator, so those helpers are **inlined** into the one `stream!` body. The
  event ordering is the whole contract, and keeping it in a single linear body is
  both necessary (no delegation) and clearer than reconstructing tau's nesting.
  `_run_tool` stays a plain `async fn` — it *buffers* progress updates and returns
  them, it never yields, so it composes fine.
- **One long function.** The inlining makes `run_agent_loop` exceed clippy's
  `too_many_lines`. That's an explicit, localized `#[allow]`: the body is a 1:1
  transliteration of tau's single generator, and splitting it would fragment the
  event ordering that is the reason the function exists. The strict lint bar holds
  everywhere else.

## Polled cancellation, not `tokio::CancellationToken`

tau's `CancellationToken` is a `Protocol` with one synchronous predicate,
`is_cancelled()`. The loop and the fake *poll* it at defined points (before a tool
runs; inside the fake's replay loop). It is deliberately **not** an awaitable
signal. rho mirrors this exactly: `trait CancellationToken { fn is_cancelled(&self)
-> bool }`, with `SimpleCancellationToken(Arc<AtomicBool>)` as the shared-flag impl
(`cancel()` flips it, clones share it). We do **not** use `tokio_util`'s awaitable
`CancellationToken`: the whole loop is a single cooperative task, every tau
cancellation check is a boolean read, and introducing an await point where tau has
a poll would change the interleaving. Polled predicate in, polled predicate out.

## `ModelProvider`: a sync fn returning a `BoxStream`

tau's `stream_response` is a **synchronous** method that returns an
`AsyncIterator`. It does its bookkeeping (record the call, snapshot the messages)
synchronously, then hands back a lazy stream. rho keeps that shape precisely:

```rust
fn stream_response(&self, model: &str, system: &str,
                   messages: &[AgentMessage], tools: &[AgentTool],
                   signal: Option<Arc<dyn CancellationToken>>)
    -> BoxStream<'static, AssistantMessageEvent>;
```

The returned stream is `'static`, which forces the design honesty that tau's
`FakeProvider` already has: a provider that needs the messages/tools must
**snapshot** (clone) them in the sync body, because the borrowed slices don't
outlive the call. `FakeProvider` records `messages.to_vec()` exactly where tau does
`list(messages)`, so `provider.calls[i]` (rho: `fake.calls()[i]`) sees the
transcript as it stood at each turn — the property the loop test
`provider.calls[1] == messages[:3]` pins.

Why a boxed stream rather than an associated `type Stream`? Because
`ModelProvider` must be object-safe (`Arc<dyn ModelProvider>` is threaded through
the harness and, later, the coding layer), and an associated-type stream would
make it not `dyn`-compatible. `BoxStream` is the object-safe rendering of "returns
some async iterator."

## The shared message list → `Arc<Mutex<Vec<AgentMessage>>>`

tau threads the *same* `list[AgentMessage]` object through the loop (which appends
to it), the provider (which snapshots it per call), and the caller (which reads the
appended messages after the run). That shared-mutable list is the one piece of the
port that doesn't have a clean owned-value analogue: a returned `Stream` can't also
hand back an owned `Vec`, and a `&mut Vec` borrow would fight the harness's need to
touch its other fields during iteration.

The rendering is `Arc<Mutex<Vec<AgentMessage>>>`, shared by the loop and the
harness. The loop appends live; the provider snapshot is a `lock().clone()`; the
harness reads it after the run — no reconstruction. The one discipline: **never
hold the lock across an `.await`** (always snapshot-then-release), which the loop
respects at every point. This is the most literal possible translation of tau's
semantics, and it makes the ported `test_agent_loop` assertions (`assert messages
== …` after the run) translate directly to `*messages.lock() == …`.

`AgentEndEvent.messages` still carries only `new_messages` (the run's additions),
matching tau; it's a separate owned `Vec`, not the shared handle.

## Errors are data: `Result`, not exceptions; and no panic-catching

tau's loop is an isolation boundary: `except Exception` around tool execution
turns *any* failure into an `is_error` tool result, and a provider
`AssistantErrorEvent` becomes a `stop_reason="error"` assistant message. Nothing
propagates out of the generator. Two translations:

- The loop's stream item type is `AgentEvent`, **not** `Result<AgentEvent, _>`. A
  provider error is a normal event (`AssistantMessageEvent::Error`) that the loop
  turns into a terminal error message. Byte-identical to tau's `stream.py`.
- A tool's executor returns `Result<AgentToolResult, ToolError>`. `Err(e)` becomes
  `error_result(e)` with `is_error = true` — the data rendering of tau's
  `except Exception: _error_result(str(exc))`. What rho does **not** do is catch
  Rust *panics*: in Python an `Exception` is routine control flow, but a Rust panic
  signals a bug, not a tool-level failure, so we let it abort rather than silently
  bury it in a tool result. (tau also re-raises `asyncio.CancelledError`; rho has
  no analogue, because cancellation is polled, never thrown.)

## Tools run sequentially — the `execution_mode` red herring

`AgentTool` carries `execution_mode: "sequential" | "parallel"` (default
`parallel`), and the milestone brief flagged a possible `JoinSet` parallel path.
Reading `tau_agent/loop.py` settles it: the loop executes a turn's tool calls with
a plain `for call in calls` — **strictly sequential, regardless of
`execution_mode`**. The field is read by the provider/coding layers (M3/M4) for
payload building, not by the loop. So there is no `JoinSet`; rho ports the `for`
loop verbatim, and carries `ToolExecutionMode` on the tool for the later layers.
Behavioral parity meant *not* adding the parallelism the field seems to promise.

## `terminate` is another dead field (like `execution_mode`)

`AgentToolResult` carries `terminate: Option<bool>` (tau `tools.py:27`), and a
reviewer suggested the loop should end the run when a tool returns
`terminate: true`. It shouldn't — because **tau's loop never reads it**:

```
$ grep -rn '\.terminate' tau/src        # → no matches
$ grep -rn 'terminate'    tau/src        # → only the field definition at tools.py:27
```

Honoring it in rho would be a behavioral *divergence* from tau, not a fix — the
same situation as `execution_mode`. rho carries the field for wire parity (a
tool result with `terminate` must round-trip byte-identically), to be honored
if and when tau's loop honors it. Until then, a `terminate` result flows through
the loop exactly like any other: its content/details become the tool-result
message, and the loop continues.

## `current_signal` is installed eagerly, not on first poll (deliberate)

tau sets `self._current_signal` *inside* the async generator body (`_run`),
which runs only on the first poll of the returned iterator. So a `cancel()`
issued after `prompt()` but *before* the first event is pulled is lost in tau
(the signal isn't installed yet). rho installs the signal **eagerly** in
`run()`, before returning the stream, so that same early `cancel()` is honored.
This is a deliberate, arguably-safer divergence — the window is tiny and only
matters for a caller that cancels a run it hasn't started consuming — but it *is*
a divergence, noted here so a future byte-diff investigator doesn't mistake it
for a bug. (The event *stream* is unaffected; this only changes whether a
pre-consumption `cancel()` takes effect.)

## Cleanup on drop: an RAII guard, not just a generator `finally`

tau's harness cleanup lives in a `finally` (`harness.py:185-190`): on generator
completion *or* `aclose()` it resets `running`, clears `current_signal`, and —
if cancelled — repairs interrupted tool calls. Python's async generators run that
`finally` when the generator is closed, including when a consumer abandons the
iterator. Rust's `async-stream` does **not**: if the consumer drops the stream at
a `yield`, the post-loop code never runs. Left unfixed, a UI that abandons a run
mid-flight would leave `running` stuck `true` forever — every later `prompt()`
rejected as already-running — with a stale signal and no interrupted-tool repair.

The fix is an RAII guard (`RunCleanup`) captured by the stream: its `Drop` runs
the cleanup when the stream is dropped (abandoned), and an explicit `.run()` at
normal exhaustion keeps the timing identical to tau (cleanup at completion), with
a `done` flag making the eventual drop a no-op. So the cleanup fires exactly once,
on whichever of {exhaustion, drop} comes first — the Rust rendering of "finally
runs on close." Two regression tests pin it: dropping mid-run resets `running`
(and a fresh `prompt()` is accepted), and cancel-then-drop appends the
`"Tool call interrupted by user"` repair.

## Progress updates are buffered, then replayed

tau's `_run_tool` gives the tool a synchronous `on_update` callback that appends
deep copies to a list *while `accepting`*, runs the tool, flips `accepting` off in
a `finally`, and returns the buffered list — which `_execute_tool_call` then
replays as `tool_execution_update` events **after** the tool completes. So updates
are not live-streamed; they're collected and emitted in order post-hoc. rho
reproduces this exactly: the callback is `Arc<dyn Fn(AgentToolResult)>` guarding an
`Arc<Mutex<Vec<…>>>` behind an `AtomicBool accepting`; the loop drains the buffer
and yields the update events after `run_tool` returns. The sequence is identical;
only the (unobservable) delivery timing differs.

## The harness: shared state without borrow gymnastics

`AgentHarness` stores its transcript, listeners, queues, running-flag and
current-signal each behind `Arc<Mutex<…>>`/`Arc<Atomic…>`. This isn't
defensive over-sharing — it's what lets `prompt()`/`continue_()` return an owned
`'static` event stream (the run captures `Arc` clones, not a `&self` borrow) while
listeners, fired *during* iteration, can still call back into the harness
(`steer`, `follow_up`, `unsubscribe`) exactly as tau's closures do. `subscribe`
returns a boxed unsubscribe closure keyed by a monotonic id (tau returns a
callable that `list.remove`s the listener); `notify` fires listeners off a
snapshot so a listener may (un)subscribe mid-notify without deadlocking. The
overlap guard (`prompt` while running) is a synchronous check returning
`Err(HarnessError::AlreadyRunning)` where tau raises `RuntimeError` — same timing
(before the stream is created), Rust-idiomatic surface.

Interrupted-tool repair (`_append_interrupted_tool_results`) is a direct port: any
assistant `tool_call` id without a matching `toolResult` gets a synthetic
`is_error` result with the exact text `"Tool call interrupted by user"`, appended
on the next `prompt`/`continue_` and in the run's cancellation `finally`.

## Session state: replay, don't mutate

`SessionState::from_entries` folds the append-only log into messages + metadata; it
never mutates a live object. Compaction is a **replacement during replay** (the
summary user-message stands in for the entries it replaces, in their original
position, with the append-fallback when none are present), and a branch summary
appends a framed user message. Because state is a pure function of the log, replay
is deterministic. The leaf-scoped variant (`from_entries_at_leaf`) restricts replay
to one root-to-leaf path reconstructed by walking `parent_id` pointers
(`tree::path_to_entry`), with tau's cycle/missing-entry detection. `SessionStorage`
is an `#[async_trait]` (object-safe) port of tau's `Protocol`;
`JsonlSessionStorage` appends `exclude_none` lines via M1's `entry_to_json_line`,
and a golden test confirms re-appending every fixture entry reproduces the fixture
bytes exactly.

## Two M1 defaults M2 had to restore

Porting tau's *tests* (which construct messages the way tau's own code does)
surfaced two places M1 modeled as required that tau actually **defaults** — so tau
accepts inputs M1's rho rejected. Both are the same lesson as M1's "validators run
on every deserialization," one level deeper:

1. **Message `timestamp`.** tau: `Field(default_factory=current_timestamp_ms)`. A
   message with no `timestamp` is valid (gets the current time). M1's fixtures
   always carried a timestamp, so it was modeled required; M2 adds
   `#[serde(default = "current_timestamp_ms")]`. tau's legacy migration and its
   constructors both emit timestamp-less messages, so this is required to load
   them.
2. **Content-block `type`.** tau: `Literal["toolCall"] = "toolCall"` (and the same
   for `text`/`thinking`/`image`). `AssistantContent` is a *plain* union (only
   `AgentMessage` is `Field(discriminator="role")`), so a tool-call dict with no
   `type` validates as `ToolCall` via the default. The legacy migration appends raw
   `tool_calls` dicts that lack `type`, relying on exactly this. M2 adds
   `#[serde(default)]` to the four plain-union blocks' `MustBe!` discriminators
   (which still *reject* a wrong value when present). The `role` discriminators stay
   required — tau's role union requires its tag.

Neither changes a single byte of output: every field is always serialized, so the
defaults only widen input acceptance. All M1 goldens stay green.

## What was ported, and what was skipped

**Ported behavior:** the provider seam + `SimpleCancellationToken`;
`FakeProvider`; `run_agent_loop` (all of steering/follow-up draining, `max_turns`
with the exact `"Agent stopped after max_turns={n}"` text, before/after-tool hooks,
tool-exception isolation, the defensive no-assistant path); `AgentHarness` (subscribe/
unsubscribe, prompt/prompt_message/continue_/steer/follow_up/cancel, both queue
modes, interrupted-tool repair); `SessionState` replay, `path_to_entry`,
`SessionStorage`/`JsonlSessionStorage`; and `Clock`/`IdGen` injection.

**Ported tests:** `test_agent_loop.py` (9 cases, incl. a rho-specific tool-error
case), `test_agent_harness.py` (7), the harness/fake-driven half of
`test_pi_event_protocol.py` (2), and `test_session.py` (11, incl. a storage byte
golden). Plus the six `event-streams/` goldens driving the real harness end-to-end.

**Skipped, with reasons:**

- `test_pi_event_protocol.py::test_tau_agent_does_not_import_tau_ai` and
  `::test_tau_ai_reexports_canonical_event_classes` — these guard the Python import
  cycle. Cargo's acyclic crate graph makes `rho-agent` structurally incapable of
  depending on `rho-ai`, so the property holds by construction (see phase-1). The
  wire shapes they touch are covered by the event-stream goldens.
- Async event listeners. tau's `_notify` awaits a listener that returns an
  awaitable; no ported test or fixture uses one, so rho's listeners are sync
  (`Fn(&AgentEvent)`) for M2. If a later layer needs async listeners, widen the
  `EventListener` type then.

## Where `FakeProvider` lives, and why

tau ships `FakeProvider` in `tau_ai`. rho can't: the M2 loop/harness goldens live
in `rho-agent`, and `rho-agent` must not depend on `rho-ai`. So the fake is a
first-class `rho-agent` util behind a default-on `fake` feature — usable by this
crate's tests now and by `rho-ai`/`rho-coding` tests later, without inverting the
dependency graph. It's the crate split doing its job: the placement that tau could
only document, Cargo enforces.
