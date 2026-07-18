"""M6 family (d), tau side: a scripted 500-turn FakeProvider session.

Drives `AgentHarness` through a fixed number of turns against the in-process
`FakeProvider` (no network, no tools), accumulating the full transcript in
memory — the counterpart to rho's `examples/rss_session.rs`. It prints nothing
but the final transcript size; peak RSS is captured by the caller running it
under `/usr/bin/time -l` (see `tools/bench/rss.sh`).

Turn count is overridable via argv[1] (default 500).
"""

from __future__ import annotations

import sys

import anyio

from tau_agent import AgentHarness, AgentHarnessConfig
from tau_agent.messages import AssistantMessage
from tau_agent.provider_events import AssistantDoneEvent, AssistantStartEvent
from tau_ai import FakeProvider


def scripted_turn(text: str) -> list:
    """One assistant turn: start then a done carrying a short reply."""
    message = AssistantMessage(content=text)
    message.stop_reason = "stop"
    return [
        AssistantStartEvent(partial=AssistantMessage(model="fake")),
        AssistantDoneEvent(reason="stop", message=message),
    ]


async def run(turns: int) -> None:
    streams = [scripted_turn(f"reply {i}") for i in range(turns)]
    harness = AgentHarness(
        AgentHarnessConfig(
            provider=FakeProvider(streams), model="fake", system="You are Tau."
        )
    )
    # Turn 1 is a fresh prompt; the rest continue the same transcript, so the
    # message list accumulates across all provider calls.
    async for _ev in harness.prompt("go"):
        pass
    for _ in range(1, turns):
        async for _ev in harness.continue_():
            pass
    print(f"turns={turns} messages={len(harness.messages)}")


def main() -> None:
    turns = int(sys.argv[1]) if len(sys.argv) > 1 else 500
    anyio.run(run, turns)


if __name__ == "__main__":
    main()
