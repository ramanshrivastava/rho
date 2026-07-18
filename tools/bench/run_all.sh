#!/usr/bin/env bash
#
# M6 benchmark orchestrator — runs all four families end-to-end and regenerates
# the report. Invoked by `just bench`. Each family writes machine-readable
# results (Criterion under target/criterion, hyperfine + tau + RSS JSON under
# tools/bench/results/); gen_report.py normalizes them into dev-notes/.
#
# Env knobs:
#   TAU_CHECKOUT   path to the pinned tau checkout (default: $REPO_ROOT/../tau,
#                  i.e. a `tau` checkout sitting beside the rho repo)
#   BENCH_SCALE    tau session-replay iteration multiplier (default 1.0)
#   RUNS/WARMUP    hyperfine cold-start counts (default 15 / 3)
#   SKIP_COLD=1    skip the hyperfine cold-start family (needs hyperfine + uv)
#
# This is intentionally serial and single-process: benchmarks must not contend
# for CPU with each other (or with a busy machine) or the numbers are noise.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TAU_CHECKOUT="${TAU_CHECKOUT:-$REPO_ROOT/../tau}"
RESULTS_DIR="$REPO_ROOT/tools/bench/results"
BENCH_SCALE="${BENCH_SCALE:-1.0}"
cd "$REPO_ROOT"
mkdir -p "$RESULTS_DIR"

echo "== [0/5] release build (rho, mock-provider, rss example) =="
cargo build --release --bin rho --bin mock-provider
cargo build --release --example rss_session -p rho-agent

echo "== [1/5] family (b)+(c): rho Criterion benches =="
cargo bench -p rho-agent --bench session_replay
cargo bench -p rho-ai --bench sse_canonicalize

echo "== [2/5] family (b)+(c): tau timers =="
uv run --project "$TAU_CHECKOUT" python tools/bench/tau_session_replay.py \
  --scale "$BENCH_SCALE" --out "$RESULTS_DIR/tau_session_replay.json"
uv run --project "$TAU_CHECKOUT" python tools/bench/tau_canonicalize.py \
  --out "$RESULTS_DIR/tau_canonicalize.json"

echo "== [2b/5] family (b): pi timer (Node, installed binary internals) =="
# Resolve the installed pi's bundled dist from the real cli.js entry so the
# harness imports the shipped session code, never a rebuild. Family (c) has no
# pi counterpart (documented in benchmarks.md: pi snapshots by reference inline,
# with no standalone canonicalization stage to isolate).
PI_ENTRY="$(readlink -f "$(command -v pi)" 2>/dev/null || realpath "$(command -v pi)" 2>/dev/null || true)"
if [[ -n "$PI_ENTRY" ]]; then
  PI_PKG="$(cd "$(dirname "$PI_ENTRY")/.." && pwd)"
  PI_CA_DIST="$PI_PKG/dist" node tools/bench/pi_session_replay.mjs \
    --scale "$BENCH_SCALE" --out "$RESULTS_DIR/pi_session_replay.json"
else
  echo "   pi not found on PATH — skipping pi session-replay timer"
fi

echo "== [3/5] family (a): cold-start (hyperfine) =="
if [[ "${SKIP_COLD:-0}" == "1" ]]; then
  echo "   skipped (SKIP_COLD=1)"
else
  bash tools/bench/cold_start.sh
fi

echo "== [4/5] family (d): memory RSS =="
# No args → rss.sh runs its full default sweep (1 500 2000); the report's
# baseline + crossover rows depend on all three, so `just bench` must not pin it
# to a single turn count.
bash tools/bench/rss.sh

echo "== [5/5] report =="
python3 tools/bench/gen_report.py

echo "== done. See dev-notes/benchmarks.md =="
