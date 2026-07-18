#!/usr/bin/env bash
#
# M6 family (d): peak resident-set-size over scripted FakeProvider sessions, for
# rho and tau. Each program drives N assistant turns (transcript accumulating in
# memory, no network) and prints only its final message count; `/usr/bin/time
# -l` reports peak RSS.
#
# We sweep several turn counts — a near-empty baseline plus the spec's 500-turn
# session plus a larger point — because the interesting result is the *shape*,
# not one number: rho's baseline is a fraction of CPython's, but the FakeProvider
# test double is where their memory models diverge sharply (see benchmarks.md).
#
# We invoke the Rust example binary and the tau **venv python directly** (not via
# `uv run`) so the footprint is the actual worker process, not a launcher. On
# Darwin `/usr/bin/time -l` reports "maximum resident set size" in bytes.
#
# Usage: tools/bench/rss.sh [turn_counts...]   (default: 1 500 2000)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TAU_CHECKOUT="${TAU_CHECKOUT:-/Users/ramanshrivastava/code/oss-gold/tau}"
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/tools/bench/results}"
RHO_EXAMPLE="$REPO_ROOT/target/release/examples/rss_session"
TAU_PY="$TAU_CHECKOUT/.venv/bin/python"
TAU_SCRIPT="$REPO_ROOT/tools/bench/tau_rss_session.py"
TURN_COUNTS=("$@")
[[ ${#TURN_COUNTS[@]} -eq 0 ]] && TURN_COUNTS=(1 500 2000)

[[ -x "$RHO_EXAMPLE" ]] || { echo "error: $RHO_EXAMPLE missing — run 'cargo build --release --example rss_session -p rho-agent'" >&2; exit 1; }
[[ -x "$TAU_PY" ]] || { echo "error: tau venv python missing at $TAU_PY — run 'uv sync --project $TAU_CHECKOUT'" >&2; exit 1; }

mkdir -p "$RESULTS_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# macOS `/usr/bin/time -l` writes "<bytes>  maximum resident set size" to stderr.
max_rss_bytes() { awk '/maximum resident set size/ {print $1}' "$1"; }

: >"$WORK/records.jsonl"
for turns in "${TURN_COUNTS[@]}"; do
  echo ">> RSS: $turns turns"
  /usr/bin/time -l "$RHO_EXAMPLE" "$turns" >"$WORK/rho.out" 2>"$WORK/rho.time"
  /usr/bin/time -l "$TAU_PY" "$TAU_SCRIPT" "$turns" >"$WORK/tau.out" 2>"$WORK/tau.time"
  rho_rss="$(max_rss_bytes "$WORK/rho.time")"; tau_rss="$(max_rss_bytes "$WORK/tau.time")"
  rho_msg="$(cat "$WORK/rho.out")"; tau_msg="$(cat "$WORK/tau.out")"
  echo "   rho: $rho_msg  peak=$rho_rss B    tau: $tau_msg  peak=$tau_rss B"
  python3 - "$turns" "$rho_rss" "$tau_rss" "$rho_msg" "$tau_msg" >>"$WORK/records.jsonl" <<'PY'
import json, sys
turns, rho_rss, tau_rss, rho_msg, tau_msg = sys.argv[1:6]
def rec(impl, rss, note):
    b = int(rss)
    return {"family": "memory_rss", "impl": impl, "turns": int(turns),
            "peak_rss_bytes": b, "peak_rss_mib": round(b / (1024*1024), 2), "note": note}
print(json.dumps(rec("rho", rho_rss, rho_msg)))
print(json.dumps(rec("tau", tau_rss, tau_msg)))
PY
done

python3 - "$RESULTS_DIR/memory_rss.json" "$WORK/records.jsonl" <<'PY'
import json, sys
out, src = sys.argv[1:3]
records = [json.loads(l) for l in open(src) if l.strip()]
json.dump(records, open(out, "w"), indent=2)
print(f"wrote {out} ({len(records)} records)", file=sys.stderr)
PY

echo ">> RSS done — results in $RESULTS_DIR/memory_rss.json"
