"""Crosscheck driver — TAU side only (rho side plugs in at M4a).

Runs a handful of scripted fake-provider sessions through tau's real print-mode
serialization path — the agent loop feeding ``JsonEventRenderer`` — which emits
exactly the JSON that ``tau -p --output json`` writes (one event object per line).
Each stream is passed through :mod:`normalizer` and stored under
``tools/crosscheck/expected/<name>.jsonl``.

At M4a, rho will run the *same* scripted sessions through its own ``rho -p``,
normalize with the identical rules, and assert equality against these files. That
plug-in point is marked ``TODO(M4a)`` below.

Determinism note: tau stamps random uuids/timestamps, but the normalizer collapses
those to position tokens, so this driver is reproducible without monkeypatching.
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extract-fixtures"))

from normalizer import normalize_stream  # noqa: E402

from tau_agent import (  # noqa: E402
    AgentHarness,
    AgentHarnessConfig,
    AgentToolResult,
    AssistantMessage,
    TextContent,
    ToolCall,
)
from tau_agent.tools import AgentTool  # noqa: E402
from tau_ai import FakeProvider  # noqa: E402
from tau_ai.events import (  # noqa: E402
    AssistantDoneEvent,
    AssistantStartEvent,
    TextDeltaEvent,
    ToolCallEndEvent,
)


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


async def _main() -> None:
    out_dir = Path(__file__).resolve().parent / "expected"
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, spec in _scenarios().items():
        raw_lines = await _run(name, spec)
        normalized = normalize_stream(raw_lines)
        (out_dir / f"{name}.jsonl").write_text("\n".join(normalized) + "\n",
                                               encoding="utf-8")
        print(f"crosscheck[tau]: {name} -> {len(normalized)} events")

    # TODO(M4a): run the identical scenarios through `rho -p --output json`,
    # normalize with tools/crosscheck/normalizer (ported to Rust or shelled out),
    # and assert equality against tools/crosscheck/expected/*.jsonl.


if __name__ == "__main__":
    asyncio.run(_main())
