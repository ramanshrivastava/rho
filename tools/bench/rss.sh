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
TAU_CHECKOUT="${TAU_CHECKOUT:-$REPO_ROOT/../tau}"
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/tools/bench/results}"
RHO_EXAMPLE="$REPO_ROOT/target/release/examples/rss_session"
TAU_PY="$TAU_CHECKOUT/.venv/bin/python"
TAU_SCRIPT="$REPO_ROOT/tools/bench/tau_rss_session.py"
PI_SCRIPT="$REPO_ROOT/tools/bench/pi_rss_session.mjs"
TURN_COUNTS=("$@")
[[ ${#TURN_COUNTS[@]} -eq 0 ]] && TURN_COUNTS=(1 500 2000)

[[ -x "$RHO_EXAMPLE" ]] || { echo "error: $RHO_EXAMPLE missing — run 'cargo build --release --example rss_session -p rho-agent'" >&2; exit 1; }
[[ -x "$TAU_PY" ]] || { echo "error: tau venv python missing at $TAU_PY — run 'uv sync --project $TAU_CHECKOUT'" >&2; exit 1; }

# pi side: drive the INSTALLED pi's own Agent + faux provider in-process (the
# code the measured `pi` binary runs). Resolve its bundled dist dirs from the
# real cli.js entry so we never rebuild or diverge from the shipped version.
# pi is OPTIONAL: if it (or its bundled internals) is absent, the pi RSS row is
# skipped and rho/tau still measure — pi-less regen never aborts.
PI_SHIM="$(command -v pi || true)"
PI_OK=0
if [[ -n "$PI_SHIM" ]] && command -v node >/dev/null 2>&1; then
  PI_ENTRY="$(readlink -f "$PI_SHIM" 2>/dev/null || realpath "$PI_SHIM")"        # .../dist/cli.js
  PI_PKG="$(cd "$(dirname "$PI_ENTRY")/.." && pwd)"                              # pi-coding-agent root
  NODE_REAL="$(readlink -f "$(command -v node)" 2>/dev/null || realpath "$(command -v node)")"
  export PI_AI_DIST="$PI_PKG/node_modules/@earendil-works/pi-ai/dist"
  export PI_AGENT_DIST="$PI_PKG/node_modules/@earendil-works/pi-agent-core/dist"
  [[ -f "$PI_AI_DIST/providers/faux.js" ]] && PI_OK=1
fi
[[ "$PI_OK" == 1 ]] || echo ">> pi/node (or bundled internals) not found — skipping pi RSS rows (rho/tau only)" >&2

mkdir -p "$RESULTS_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Peak RSS via `/usr/bin/time`, normalized to bytes across BSD (macOS) and GNU
# (Linux) — their flags AND units differ:
#   * macOS/BSD:  `-l`  → "<bytes>  maximum resident set size"      (bytes)
#   * GNU/Linux:  `-v`  → "Maximum resident set size (kbytes): N"   (kibibytes)
case "$(uname -s)" in
  Darwin) TIME_FLAG="-l" ;;
  Linux)  TIME_FLAG="-v" ;;
  *) echo "error: unsupported OS $(uname -s) — RSS bench needs BSD or GNU /usr/bin/time" >&2; exit 1 ;;
esac

max_rss_bytes() { # $1 = time stderr capture; echoes bytes (empty on parse failure)
  if [[ "$(uname -s)" == "Darwin" ]]; then
    awk '/maximum resident set size/ {print $1; exit}' "$1"
  else
    awk -F':[[:space:]]*' '/Maximum resident set size/ {print $2 * 1024; exit}' "$1"
  fi
}

: >"$WORK/records.jsonl"
for turns in "${TURN_COUNTS[@]}"; do
  echo ">> RSS: $turns turns"
  /usr/bin/time "$TIME_FLAG" "$RHO_EXAMPLE" "$turns" >"$WORK/rho.out" 2>"$WORK/rho.time"
  /usr/bin/time "$TIME_FLAG" "$TAU_PY" "$TAU_SCRIPT" "$turns" >"$WORK/tau.out" 2>"$WORK/tau.time"
  rho_rss="$(max_rss_bytes "$WORK/rho.time")"; tau_rss="$(max_rss_bytes "$WORK/tau.time")"
  if [[ -z "$rho_rss" || -z "$tau_rss" ]]; then
    echo "error: could not parse peak RSS from /usr/bin/time output" >&2
    cat "$WORK/rho.time" >&2; exit 1
  fi
  rho_msg="$(cat "$WORK/rho.out")"; tau_msg="$(cat "$WORK/tau.out")"
  pi_rss=""; pi_msg=""
  if [[ "$PI_OK" == 1 ]]; then
    /usr/bin/time "$TIME_FLAG" "$NODE_REAL" "$PI_SCRIPT" "$turns" >"$WORK/pi.out" 2>"$WORK/pi.time"
    pi_rss="$(max_rss_bytes "$WORK/pi.time")"; pi_msg="$(cat "$WORK/pi.out")"
    [[ -n "$pi_rss" ]] || { echo "error: could not parse pi peak RSS" >&2; cat "$WORK/pi.time" >&2; exit 1; }
  fi
  echo "   rho: $rho_msg  peak=$rho_rss B    tau: $tau_msg  peak=$tau_rss B    pi: ${pi_msg:-<skipped>}  peak=${pi_rss:-—} B"
  python3 - "$turns" "$rho_rss" "$tau_rss" "$pi_rss" "$rho_msg" "$tau_msg" "$pi_msg" >>"$WORK/records.jsonl" <<'PY'
import json, sys
turns, rho_rss, tau_rss, pi_rss, rho_msg, tau_msg, pi_msg = sys.argv[1:8]
def rec(impl, rss, note):
    b = int(rss)
    return {"family": "memory_rss", "impl": impl, "turns": int(turns),
            "peak_rss_bytes": b, "peak_rss_mib": round(b / (1024*1024), 2), "note": note}
print(json.dumps(rec("rho", rho_rss, rho_msg)))
print(json.dumps(rec("tau", tau_rss, tau_msg)))
if pi_rss:  # pi row only when pi was available
    print(json.dumps(rec("pi", pi_rss, pi_msg)))
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
