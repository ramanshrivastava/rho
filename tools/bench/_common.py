"""Shared helpers for the tau-side M6 benchmark timers.

These scripts are the Python counterparts to rho's Criterion benches. They use
`time.perf_counter`, run N warmup + M measured iterations, and emit records in
the normalized JSON shape consumed by `gen_report.py`. Keeping the stats tiny
and dependency-free (no numpy) matches tau's own test posture.
"""

from __future__ import annotations

import json
import math
import statistics
import sys
from pathlib import Path

# Repo layout: tools/bench/_common.py -> repo root is two levels up.
REPO_ROOT = Path(__file__).resolve().parents[2]
SYNTHETIC_DIR = REPO_ROOT / "fixtures" / "sessions" / "synthetic"


def summarize(times_s: list[float]) -> dict[str, float]:
    """Reduce a list of per-iteration durations (seconds) to summary stats."""
    mean = statistics.fmean(times_s)
    stddev = statistics.stdev(times_s) if len(times_s) > 1 else 0.0
    return {
        "mean_ms": mean * 1e3,
        "stddev_ms": stddev * 1e3,
        "min_ms": min(times_s) * 1e3,
        "iterations": len(times_s),
    }


def emit(records: list[dict], out: str | None) -> None:
    """Write records as a JSON list to `out` (or stdout when None/`-`)."""
    payload = json.dumps(records, indent=2)
    if out and out != "-":
        Path(out).write_text(payload + "\n", encoding="utf-8")
        print(f"wrote {len(records)} records -> {out}", file=sys.stderr)
    else:
        print(payload)


def per_sec(count: int, mean_ms: float) -> float:
    """Throughput helper: items processed per wall-clock second."""
    if mean_ms <= 0:
        return math.inf
    return count / (mean_ms / 1e3)
