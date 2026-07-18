"""M6 family (b), tau side: session-replay throughput.

For each synthetic tree, parse every JSONL entry line and replay it into a
`SessionState` — the same parse+replay the rho Criterion bench times. The 100k
trees ship gzipped and are inflated in-process, exactly like the Rust bench.

Run via uv against the pinned tau revision, e.g.:

    uv run --project <tau> python tools/bench/tau_session_replay.py \
        --scale 1.0 --out tools/bench/results/tau_session_replay.json

(`--scale` multiplies the per-size iteration counts; use `<1` for a quick smoke.)

Emits records: {family, impl, dataset, n_entries, mean_ms, entries_per_sec, ...}.
"""

from __future__ import annotations

import argparse
import gzip
import sys
from time import perf_counter

from _common import SYNTHETIC_DIR, emit, per_sec, summarize

from tau_agent.session.jsonl import entries_from_json_lines
from tau_agent.session.memory import SessionState

FAMILIES = ["linear", "deep-branch", "compaction-heavy"]
# (warmup, measured) per size. tau's pydantic parse is ~9s for a 100k tree, so
# large trees get few iterations — enough for a stable mean without a 10-minute
# sweep. rho's Criterion side self-tunes sample counts the same way.
SIZE_ITERS = {"1k": (3, 30), "10k": (2, 10), "100k": (1, 3)}
# Excluded, matching the rho bench: compaction replay is O(n²) in BOTH tau and
# rho (measured tau 10k replay ≈ 7 s), so the 100k cell takes minutes per
# iteration and adds no signal beyond the 1k→10k trend. Intentional, not silent.
SKIP = {("compaction-heavy", "100k")}


def read_dataset(name: str) -> list[str]:
    """Return the non-empty JSONL lines for a synthetic dataset, gunzipping 100k."""
    plain = SYNTHETIC_DIR / f"{name}.jsonl"
    if plain.exists():
        text = plain.read_text(encoding="utf-8")
    else:
        gz = SYNTHETIC_DIR / f"{name}.jsonl.gz"
        text = gzip.decompress(gz.read_bytes()).decode("utf-8")
    return [line for line in text.splitlines() if line]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--scale",
        type=float,
        default=1.0,
        help="multiply per-size iteration counts (use <1 for a quick smoke run)",
    )
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    records: list[dict] = []
    for family in FAMILIES:
        for size, (warmup, iterations) in SIZE_ITERS.items():
            if (family, size) in SKIP:
                continue
            warmup = max(1, round(warmup * args.scale))
            iterations = max(1, round(iterations * args.scale))
            name = f"{family}-{size}"
            lines = read_dataset(name)
            n_entries = len(lines)

            def load_once(lines: list[str] = lines) -> int:
                entries = entries_from_json_lines(lines)  # parse
                state = SessionState.from_entries(entries)  # replay
                return len(state.messages)

            for _ in range(warmup):
                load_once()

            times: list[float] = []
            for _ in range(iterations):
                t0 = perf_counter()
                load_once()
                times.append(perf_counter() - t0)
            print(f"  tau session_replay {name}: {n_entries} entries", file=sys.stderr)

            stats = summarize(times)
            records.append(
                {
                    "family": "session_replay",
                    "impl": "tau",
                    "dataset": name,
                    "n_entries": n_entries,
                    "entries_per_sec": per_sec(n_entries, stats["mean_ms"]),
                    **stats,
                }
            )
    emit(records, args.out)


if __name__ == "__main__":
    main()
