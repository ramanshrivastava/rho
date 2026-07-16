"""Extract scripted agent-loop event streams (fixtures/event-streams/).

Each scenario drives tau's *real* ``AgentHarness`` + ``run_agent_loop`` with a
``FakeProvider`` replaying a fixed script, and captures three artifacts:

  * ``agent-events.jsonl``     — the ``AgentEvent`` sequence, serialized exactly as
                                 ``tau -p --output json`` emits (``model_dump_json``
                                 with ``by_alias=True, exclude_none=True``).
  * ``assistant-events.jsonl`` — the provider-layer ``AssistantMessageEvent``
                                 sequence the fake replayed, in order.
  * ``script.json``            — the fake-provider script + run parameters so rho
                                 can replay byte-identical inputs.

Scenarios: text-only turn, multi-tool-call turn, thinking blocks, error stop,
steering injection between turns, and a max_turns cutoff.
"""

from __future__ import annotations

import asyncio
import json

from _common import compact, patch_determinism, write_text

patch_determinism()

from tau_agent import (  # noqa: E402
    AgentHarness,
    AgentHarnessConfig,
    AgentToolResult,
    AssistantMessage,
    TextContent,
    ThinkingContent,
    ToolCall,
)
from tau_agent.events import TurnEndEvent  # noqa: E402
from tau_agent.tools import AgentTool  # noqa: E402
from tau_ai import FakeProvider  # noqa: E402
from tau_ai.events import (  # noqa: E402
    AssistantDoneEvent,
    AssistantErrorEvent,
    AssistantStartEvent,
    TextDeltaEvent,
    TextEndEvent,
    TextStartEvent,
    ThinkingDeltaEvent,
    ThinkingEndEvent,
    ThinkingStartEvent,
    ToolCallEndEvent,
    ToolCallStartEvent,
)


def _echo_tool() -> AgentTool:
    async def execute(tool_call_id, arguments, signal=None, on_update=None):
        del tool_call_id, signal, on_update
        return AgentToolResult(
            content=[TextContent(text=json.dumps(arguments, sort_keys=True))],
            details={"echoed": True},
        )

    return AgentTool(
        name="echo", label="echo", description="Echo arguments back.",
        parameters={"type": "object", "properties": {}}, execute_fn=execute,
    )


def _text_stream(text: str) -> list:
    empty = AssistantMessage(model="fake")
    full = AssistantMessage(model="fake", content=[TextContent(text=text)])
    return [
        AssistantStartEvent(partial=empty),
        TextStartEvent(content_index=0, partial=empty),
        TextDeltaEvent(content_index=0, delta=text, partial=full),
        TextEndEvent(content_index=0, content=text, partial=full),
        AssistantDoneEvent(reason="stop", message=full),
    ]


def _scenarios() -> dict:
    # --- text-only turn ------------------------------------------------------
    text_only = {
        "prompt": "say hi", "system": "test", "model": "fake", "tools": [],
        "max_turns": None, "steer_after_turn": None,
        "streams": [_text_stream("hello world")],
    }

    # --- multi-tool-call turn (two calls in one assistant message) ----------
    call_a = ToolCall(id="call-a", name="echo", arguments={"n": 1})
    call_b = ToolCall(id="call-b", name="echo", arguments={"n": 2})
    tools_msg = AssistantMessage(model="fake", content=[call_a, call_b])
    empty = AssistantMessage(model="fake")
    multi_tool = {
        "prompt": "use tools twice", "system": "test", "model": "fake",
        "tools": ["echo"], "max_turns": None, "steer_after_turn": None,
        "streams": [
            [
                AssistantStartEvent(partial=empty),
                ToolCallStartEvent(content_index=0, partial=tools_msg),
                ToolCallEndEvent(content_index=0, tool_call=call_a, partial=tools_msg),
                ToolCallStartEvent(content_index=1, partial=tools_msg),
                ToolCallEndEvent(content_index=1, tool_call=call_b, partial=tools_msg),
                AssistantDoneEvent(reason="toolUse", message=tools_msg),
            ],
            _text_stream("done using tools"),
        ],
    }

    # --- thinking blocks -----------------------------------------------------
    think_msg = AssistantMessage(
        model="fake",
        content=[ThinkingContent(thinking="let me reason"),
                 TextContent(text="the answer")],
    )
    thinking = {
        "prompt": "think first", "system": "test", "model": "fake", "tools": [],
        "max_turns": None, "steer_after_turn": None,
        "streams": [[
            AssistantStartEvent(partial=empty),
            ThinkingStartEvent(content_index=0, partial=empty),
            ThinkingDeltaEvent(content_index=0, delta="let me reason", partial=think_msg),
            ThinkingEndEvent(content_index=0, content="let me reason", partial=think_msg),
            TextStartEvent(content_index=1, partial=think_msg),
            TextDeltaEvent(content_index=1, delta="the answer", partial=think_msg),
            TextEndEvent(content_index=1, content="the answer", partial=think_msg),
            AssistantDoneEvent(reason="stop", message=think_msg),
        ]],
    }

    # --- error stop ----------------------------------------------------------
    err_msg = AssistantMessage(model="fake", stop_reason="error",
                               error_message="model exploded")
    error_stop = {
        "prompt": "trigger error", "system": "test", "model": "fake", "tools": [],
        "max_turns": None, "steer_after_turn": None,
        "streams": [[
            AssistantStartEvent(partial=empty),
            AssistantErrorEvent(reason="error", error=err_msg),
        ]],
    }

    # --- steering injection between turns ------------------------------------
    steering = {
        "prompt": "first request", "system": "test", "model": "fake", "tools": [],
        "max_turns": None, "steer_after_turn": 1, "steer_text": "actually, also this",
        "streams": [_text_stream("first answer"), _text_stream("second answer")],
    }

    # --- max_turns cutoff ----------------------------------------------------
    call_c = ToolCall(id="call-c", name="echo", arguments={})
    tool_msg = AssistantMessage(model="fake", content=[call_c])
    max_turns = {
        "prompt": "loop forever", "system": "test", "model": "fake",
        "tools": ["echo"], "max_turns": 1, "steer_after_turn": None,
        "streams": [[
            AssistantStartEvent(partial=empty),
            ToolCallEndEvent(content_index=0, tool_call=call_c, partial=tool_msg),
            AssistantDoneEvent(reason="toolUse", message=tool_msg),
        ]],
    }

    return {
        "text-only": text_only,
        "multi-tool-call": multi_tool,
        "thinking": thinking,
        "error-stop": error_stop,
        "steering": steering,
        "max-turns": max_turns,
    }


async def _run_scenario(name: str, spec: dict) -> None:
    provider = FakeProvider(spec["streams"])
    tools = [_echo_tool()] if spec["tools"] else []
    harness = AgentHarness(AgentHarnessConfig(
        provider=provider, model=spec["model"], system=spec["system"],
        tools=tools, max_turns=spec["max_turns"],
    ))

    steer_after = spec.get("steer_after_turn")
    if steer_after is not None:
        turns_seen = {"count": 0}
        unsub_holder: dict = {}

        def on_event(event):
            if isinstance(event, TurnEndEvent):
                turns_seen["count"] += 1
                if turns_seen["count"] == steer_after:
                    harness.steer(spec["steer_text"])
                    unsub_holder["unsub"]()

        unsub_holder["unsub"] = harness.subscribe(on_event)

    agent_events = [event async for event in harness.prompt(spec["prompt"])]

    agent_lines = [
        e.model_dump_json(by_alias=True, exclude_none=True) for e in agent_events
    ]
    write_text(f"event-streams/{name}/agent-events.jsonl", "\n".join(agent_lines) + "\n")

    assistant_lines: list[str] = []
    for stream in spec["streams"]:
        for event in stream:
            assistant_lines.append(
                event.model_dump_json(by_alias=True, exclude_none=True))
    write_text(f"event-streams/{name}/assistant-events.jsonl",
               "\n".join(assistant_lines) + "\n")

    script = {
        "prompt": spec["prompt"], "system": spec["system"], "model": spec["model"],
        "tools": spec["tools"], "max_turns": spec["max_turns"],
        "steer_after_turn": steer_after,
        "steer_text": spec.get("steer_text"),
        "streams": [
            [json.loads(e.model_dump_json(by_alias=True, exclude_none=True))
             for e in stream]
            for stream in spec["streams"]
        ],
    }
    write_text(f"event-streams/{name}/script.json", compact(script) + "\n")


async def _main() -> int:
    scenarios = _scenarios()
    for name, spec in scenarios.items():
        await _run_scenario(name, spec)
    return len(scenarios)


def extract() -> int:
    return asyncio.run(_main())


if __name__ == "__main__":
    print(f"event-streams: wrote {extract()} scenarios")
