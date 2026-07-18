"""M6 family (c), tau side: SSE canonicalization overhead.

Feeds a response-start, N text deltas, and a terminal response-end through
`canonicalize_provider_stream` and drains every canonical event — the analogue
of rho's `StreamAccumulator` micro-bench. Provider events are pre-built once
(their construction is provider-parse work, not canonicalization) and re-yielded
each measured iteration, so the timing isolates the per-token bookkeeping +
per-event snapshot cost.

The async pipeline is driven inside a single `anyio.run` so event-loop startup
is amortized across all iterations rather than charged to each one.

Emits records: {family, impl, n_deltas, mean_ms, ns_per_delta, deltas_per_sec,
events_per_sec, events, ...}.
"""

from __future__ import annotations

import argparse
from time import perf_counter

import anyio
from _common import emit, per_sec, summarize

from tau_agent.messages import AssistantMessage
from tau_ai._provider_events import (
    ProviderResponseEndEvent,
    ProviderResponseStartEvent,
    ProviderTextDeltaEvent,
)
from tau_ai.stream import canonicalize_provider_stream

SIZES = [100, 1_000, 10_000]


def build_chunks(n: int) -> list:
    return (
        [ProviderResponseStartEvent(model="bench-model")]
        + [ProviderTextDeltaEvent(delta="tok ") for _ in range(n)]
        + [
            ProviderResponseEndEvent(
                message=AssistantMessage(content=""), finish_reason="stop"
            )
        ]
    )


async def drain(chunks: list) -> int:
    async def source():
        for chunk in chunks:
            yield chunk

    count = 0
    async for _ev in canonicalize_provider_stream(
        source(), api="anthropic-messages", provider="anthropic", model="bench-model"
    ):
        count += 1
    return count


async def run(records: list[dict], iterations: int, warmup: int) -> None:
    for n in SIZES:
        chunks = build_chunks(n)
        events = await drain(chunks)  # also serves as a warmup + event count

        for _ in range(warmup):
            await drain(chunks)

        times: list[float] = []
        for _ in range(iterations):
            t0 = perf_counter()
            await drain(chunks)
            times.append(perf_counter() - t0)

        stats = summarize(times)
        records.append(
            {
                "family": "sse_canonicalize",
                "impl": "tau",
                "n_deltas": n,
                "events": events,
                "ns_per_delta": stats["mean_ms"] * 1e6 / n,
                "deltas_per_sec": per_sec(n, stats["mean_ms"]),
                "events_per_sec": per_sec(events, stats["mean_ms"]),
                **stats,
            }
        )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--iterations", type=int, default=50)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    records: list[dict] = []
    anyio.run(run, records, args.iterations, args.warmup)
    emit(records, args.out)


if __name__ == "__main__":
    main()
