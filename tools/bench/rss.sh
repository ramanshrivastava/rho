#!/usr/bin/env bash
#
# M6 family (d): peak resident-set-size over a scripted 500-turn FakeProvider
# session, for rho and tau. Each program drives the same shape of work (500
# assistant turns, transcript accumulating in memory, no network) and prints
# only its final message count; `/usr/bin/time -l` reports the peak RSS.
#
# We invoke the Rust example binary and the tau **venv python directly** (not
# via `uv run`) so the measured footprint is the actual worker process, not a
# launcher. On Darwin `/usr/bin/time -l` reports "maximum resident set size" in
# bytes. Allocator caveats are discussed honestly in dev-notes/benchmarks.md.
#
# Usage: tools/bench/rss.sh [turns]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TAU_CHECKOUT="${TAU_CHECKOUT:-/Users/ramanshrivastava/code/oss-gold/tau}"
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/tools/bench/results}"
RHO_EXAMPLE="$REPO_ROOT/target/release/examples/rss_session"
TAU_PY="$TAU_CHECKOUT/.venv/bin/python"
TAU_SCRIPT="$REPO_ROOT/tools/bench/tau_rss_session.py"
TURNS="${1:-500}"

[[ -x "$RHO_EXAMPLE" ]] || { echo "error: $RHO_EXAMPLE missing — run 'cargo build --release --example rss_session -p rho-agent'" >&2; exit 1; }
[[ -x "$TAU_PY" ]] || { echo "error: tau venv python missing at $TAU_PY — run 'uv sync --project $TAU_CHECKOUT'" >&2; exit 1; }

mkdir -p "$RESULTS_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# macOS `/usr/bin/time -l` writes "<bytes>  maximum resident set size" to stderr.
max_rss_bytes() { # $1=path to a time -l stderr capture
  awk '/maximum resident set size/ {print $1}' "$1"
}

echo ">> RSS: rho example ($TURNS turns)"
/usr/bin/time -l "$RHO_EXAMPLE" "$TURNS" >"$WORK/rho.out" 2>"$WORK/rho.time"
RHO_MSG="$(cat "$WORK/rho.out")"
RHO_RSS="$(max_rss_bytes "$WORK/rho.time")"

echo ">> RSS: tau python ($TURNS turns)"
/usr/bin/time -l "$TAU_PY" "$TAU_SCRIPT" "$TURNS" >"$WORK/tau.out" 2>"$WORK/tau.time"
TAU_MSG="$(cat "$WORK/tau.out")"
TAU_RSS="$(max_rss_bytes "$WORK/tau.time")"

echo "   rho: $RHO_MSG  peak_rss=$RHO_RSS bytes"
echo "   tau: $TAU_MSG  peak_rss=$TAU_RSS bytes"

python3 - "$RESULTS_DIR/memory_rss.json" "$TURNS" "$RHO_RSS" "$TAU_RSS" "$RHO_MSG" "$TAU_MSG" <<'PY'
import json, sys
out, turns, rho_rss, tau_rss, rho_msg, tau_msg = sys.argv[1:7]
def rec(impl, rss, note):
    b = int(rss)
    return {"family": "memory_rss", "impl": impl, "turns": int(turns),
            "peak_rss_bytes": b, "peak_rss_mib": round(b / (1024*1024), 2),
            "note": note}
records = [rec("rho", rho_rss, rho_msg), rec("tau", tau_rss, tau_msg)]
json.dump(records, open(out, "w"), indent=2)
print(f"wrote {out}", file=sys.stderr)
PY

echo ">> RSS done — results in $RESULTS_DIR/memory_rss.json"
