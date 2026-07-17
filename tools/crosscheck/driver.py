"""Crosscheck driver — the TAU side of the tau/rho differential harness.

**v1 (M4a)** runs scripted fake-provider sessions through tau's bare
``AgentHarness`` + ``JsonEventRenderer`` and writes the normalized event streams
to ``tools/crosscheck/expected/<name>.jsonl``. rho's ``tests/crosscheck.rs``
reproduces them.

**v2 (M4b)** drives tau's full ``CodingSession`` through richer scenarios
(including compaction and branch), and — because tau's ``patch_determinism``
(counter uuids ``{n:032x}`` + frozen clocks) matches rho's
``SequentialIdGen`` + ``FixedClock::fixture`` *exactly* — the **raw session
JSONL files are byte-identical across the two implementations**. For each v2
scenario the driver emits three artifacts:

* ``tools/crosscheck/sessions/<name>.session.jsonl`` — the raw, byte-identical
  session file (the on-disk *interchange artifact*: rho's ``crosscheck_v2.rs``
  asserts its own writer reproduces these bytes, and both directions of the
  resume-swap load this same file).
* ``tools/crosscheck/expected/v2/<name>.events.jsonl`` — the normalized
  ``CodingSession`` event stream.
* ``tools/crosscheck/expected/v2/<name>.state.jsonl`` — the ``(role, text)`` of
  the transcript replayed from a fresh reload of the session file (the
  resume-swap oracle; ``resume_swap.py`` asserts tau reproduces it from the
  committed — i.e. rho-written — file).

Determinism note: tau stamps random uuids/timestamps; ``patch_determinism``
freezes them, and the event-stream normalizer additionally collapses any
residual volatile values to position tokens.
"""

from __future__ import annotations

import asyncio
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extract-fixtures"))

from _common import patch_determinism  # noqa: E402
from normalizer import normalize_stream  # noqa: E402

from tau_agent import (  # noqa: E402
    AgentHarness,
    AgentHarnessConfig,
    AgentToolResult,
    AssistantMessage,
    TextContent,
    ToolCall,
)
from tau_agent.messages import message_text  # noqa: E402
from tau_agent.session.jsonl import entry_from_json_line  # noqa: E402
from tau_agent.tools import AgentTool  # noqa: E402
from tau_ai import FakeProvider  # noqa: E402
from tau_ai.events import (  # noqa: E402
    AssistantDoneEvent,
    AssistantStartEvent,
    TextDeltaEvent,
    ToolCallEndEvent,
)
from tau_coding.session import (  # noqa: E402
    CodingSession,
    CodingSessionConfig,
    jsonl_session_storage,
)

# A fixed, non-existent cwd so `session_info.cwd` is identical on both sides
# (the one otherwise environment-dependent field in the session file).
FIXED_CWD = "/rho-crosscheck-cwd"


def _echo_tool() -> AgentTool:
    async def execute(tool_call_id, arguments, signal=None, on_update=None):
        del tool_call_id, signal, on_update
        return AgentToolResult(content=[TextContent(text="ok")], details={})

    return AgentTool(name="echo", label="echo", description="echo",
                     parameters={"type": "object", "properties": {}},
                     execute_fn=execute)


def _text_stream(text: str) -> list:
    full = AssistantMessage(model="fake", content=[TextContent(text=text)])
    return [
        AssistantStartEvent(partial=AssistantMessage(model="fake")),
        TextDeltaEvent(content_index=0, delta=text, partial=full),
        AssistantDoneEvent(reason="stop", message=full),
    ]


def _scenarios() -> dict:
    call = ToolCall(id="call-1", name="echo", arguments={"n": 1})
    tool_msg = AssistantMessage(model="fake", content=[call])
    return {
        "text": {"prompt": "hi", "tools": [], "streams": [_text_stream("hello")]},
        "tool": {"prompt": "use echo", "tools": ["echo"], "streams": [
            [AssistantStartEvent(partial=AssistantMessage(model="fake")),
             ToolCallEndEvent(content_index=0, tool_call=call, partial=tool_msg),
             AssistantDoneEvent(reason="toolUse", message=tool_msg)],
            _text_stream("done"),
        ]},
        "multiturn": {"prompt": "count", "tools": [], "streams": [
            _text_stream("one"), _text_stream("two"),
        ], "follow_up": "again"},
    }


async def _run(name: str, spec: dict) -> list[str]:
    provider = FakeProvider(spec["streams"])
    tools = [_echo_tool()] if spec["tools"] else []
    harness = AgentHarness(AgentHarnessConfig(
        provider=provider, model="fake", system="crosscheck", tools=tools))
    if spec.get("follow_up"):
        harness.follow_up(spec["follow_up"])
    # This mirrors JsonEventRenderer.render: model_dump_json(by_alias, exclude_none).
    return [
        event.model_dump_json(by_alias=True, exclude_none=True)
        async for event in harness.prompt(spec["prompt"])
    ]


def _v2_scenarios() -> dict:
    call = ToolCall(id="call-1", name="echo", arguments={"n": 1})
    tool_msg = AssistantMessage(model="fake", content=[call])
    tool_stream = [
        AssistantStartEvent(partial=AssistantMessage(model="fake")),
        ToolCallEndEvent(content_index=0, tool_call=call, partial=tool_msg),
        AssistantDoneEvent(reason="toolUse", message=tool_msg),
    ]
    return {
        # Simple text turn.
        "text": {"tools": False, "streams": [_text_stream("hello")],
                 "ops": [("prompt", "hi")]},
        # A tool call + a follow-up text turn.
        "tool": {"tools": True, "streams": [tool_stream, _text_stream("done")],
                 "ops": [("prompt", "use echo")]},
        # A turn, then a manual compaction (2nd stream is the summary model call);
        # exercises the CompactionEntry + replaces_entry_ids on disk.
        "compaction": {"tools": False,
                       "streams": [_text_stream("did the work"),
                                   _text_stream("## Summary\nprior work done")],
                       "ops": [("prompt", "work"), ("compact",)]},
        # Two turns, then branch back to the first assistant reply (summarize=False);
        # exercises a mid-history branch leaf + truncated resume.
        "branch": {"tools": False,
                   "streams": [_text_stream("first reply"), _text_stream("second reply")],
                   "ops": [("prompt", "first"), ("prompt", "second"),
                           ("branch", "first_assistant")]},
    }


def _select_entry(session_path: Path, selector: str) -> str:
    """Resolve a branch target (e.g. the first assistant message entry id)."""
    entries = [
        entry_from_json_line(line)
        for line in session_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    messages = [e for e in entries if e.type == "message"]
    if selector == "first_assistant":
        for entry in messages:
            if entry.message.role == "assistant":
                return entry.id
    raise ValueError(f"unknown selector: {selector}")


async def _load_v2_session(session_path: Path, tools: list) -> CodingSession:
    return await CodingSession.load(CodingSessionConfig(
        provider=FakeProvider([]), model="fake", cwd=Path(FIXED_CWD),
        storage=jsonl_session_storage(session_path), provider_name="fake", tools=tools))


async def _run_v2(name: str, spec: dict, sessions_dir: Path, v2_dir: Path) -> None:
    # Reset the entry-id counter to 0 for each scenario, so every session file is
    # independent (ids 0..N). rho drives each scenario with a fresh
    # `SequentialIdGen`, so this keeps the raw files byte-identical per scenario.
    patch_determinism()
    tmp = Path(tempfile.mkdtemp())
    session_path = tmp / "s.jsonl"
    tools = [_echo_tool()] if spec["tools"] else []
    provider = FakeProvider(spec["streams"])
    session = await CodingSession.load(CodingSessionConfig(
        provider=provider, model="fake", cwd=Path(FIXED_CWD),
        storage=jsonl_session_storage(session_path), provider_name="fake", tools=tools))

    events: list[str] = []
    for op in spec["ops"]:
        if op[0] == "prompt":
            async for event in session.prompt(op[1]):
                events.append(event.model_dump_json(by_alias=True, exclude_none=True))
        elif op[0] == "compact":
            await session.compact()
        elif op[0] == "branch":
            await session.branch_to_entry(_select_entry(session_path, op[1]), summarize=False)

    # Raw byte-identical session file (the interchange artifact).
    (sessions_dir / f"{name}.session.jsonl").write_text(
        session_path.read_text(encoding="utf-8"), encoding="utf-8")
    # Normalized event stream.
    normalized = normalize_stream(events)
    (v2_dir / f"{name}.events.jsonl").write_text(
        "\n".join(normalized) + ("\n" if normalized else ""), encoding="utf-8")
    # Resume-swap oracle: (role, text) of the transcript replayed from a fresh reload.
    reloaded = await _load_v2_session(session_path, tools)
    state = [
        json.dumps({"role": m.role, "text": message_text(m)}, ensure_ascii=False, separators=(",", ":"))
        for m in reloaded.messages
    ]
    (v2_dir / f"{name}.state.jsonl").write_text(
        "\n".join(state) + ("\n" if state else ""), encoding="utf-8")
    print(f"crosscheck[tau/v2]: {name} -> {len(normalized)} events, "
          f"{len(session_path.read_text().splitlines())} entries, {len(state)} replayed msgs")


async def _main() -> None:
    # Freeze tau's message clock so every timestamp is the same fixed value
    # (`1_700_000_000_123` ms). The normalizer then collapses them all to
    # `<ts:0>`, making the crosscheck language-independent: rho reproduces the
    # identical stream with `FixedClock::fixture()`. Without this, the tokens
    # depend on wall-clock millisecond collisions that a Rust re-implementation
    # cannot reproduce. See dev-notes/phase-4a.md.
    patch_determinism()
    out_dir = Path(__file__).resolve().parent / "expected"
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, spec in _scenarios().items():
        raw_lines = await _run(name, spec)
        normalized = normalize_stream(raw_lines)
        (out_dir / f"{name}.jsonl").write_text("\n".join(normalized) + "\n",
                                               encoding="utf-8")
        print(f"crosscheck[tau]: {name} -> {len(normalized)} events")

    # v2: drive tau's full CodingSession; emit the byte-identical session files
    # + normalized event streams + resume-swap state oracles. rho's
    # `tests/crosscheck_v2.rs` reproduces all three; `resume_swap.py` closes the
    # rho->tau direction.
    here = Path(__file__).resolve().parent
    sessions_dir = here / "sessions"
    v2_dir = here / "expected" / "v2"
    sessions_dir.mkdir(parents=True, exist_ok=True)
    v2_dir.mkdir(parents=True, exist_ok=True)
    for name, spec in _v2_scenarios().items():
        await _run_v2(name, spec, sessions_dir, v2_dir)


if __name__ == "__main__":
    asyncio.run(_main())
