#!/usr/bin/env bash
#
# M6 family (a): cold-start + end-to-end print latency (hyperfine).
#
# Measures process-spawn -> exit for a single non-interactive print-mode turn,
# comparing the compiled `rho` binary against `tau` launched via `uv run`. Both
# talk to the same in-process mock provider (tools/mock-provider) replaying a
# fixed OpenAI-compatible SSE body, so the only moving parts are process startup
# and the streaming client. Variants:
#   * 0ms      — whole body returned at once (isolates spawn + parse)
#   * 20ms/chunk — body chunked with per-chunk latency (models streaming; a
#                  fixed network cost both implementations pay identically)
#   * version  — `--version` only, the purest interpreter-vs-binary startup gap.
#
# Configs are written into a throwaway dir (RHO_HOME / a temp HOME for tau) so
# the user's real ~/.rho and ~/.tau are never touched. hyperfine JSON lands in
# tools/bench/results/. Honest framing lives in dev-notes/benchmarks.md: this is
# where a compiled binary wins outright, and also where — under any real network
# latency — that win is dwarfed by the provider round-trip.
#
# Usage: tools/bench/cold_start.sh [--runs N] [--warmup N]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TAU_CHECKOUT="${TAU_CHECKOUT:-$REPO_ROOT/../tau}"
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/tools/bench/results}"
RHO_BIN="$REPO_ROOT/target/release/rho"
MOCK_BIN="$REPO_ROOT/target/release/mock-provider"
FIXTURE="$REPO_ROOT/fixtures/sse/openai_compatible/text.sse"
PORT="${BENCH_PORT:-18086}"
RUNS="${RUNS:-15}"
WARMUP="${WARMUP:-3}"
MODEL="gpt-x"

# Parse a couple of flags.
while [[ $# -gt 0 ]]; do
  case "$1" in
    --runs) RUNS="$2"; shift 2 ;;
    --warmup) WARMUP="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

for tool in hyperfine uv; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool not found on PATH" >&2; exit 1; }
done
[[ -x "$RHO_BIN" ]] || { echo "error: $RHO_BIN missing — run 'cargo build --release'" >&2; exit 1; }
[[ -x "$MOCK_BIN" ]] || { echo "error: $MOCK_BIN missing — run 'cargo build --release'" >&2; exit 1; }

mkdir -p "$RESULTS_DIR"
WORK="$(mktemp -d)"
RHO_HOME="$WORK/rho-home"
TAU_HOME="$WORK/tau-home"
UV_CACHE="$(uv cache dir)"
mkdir -p "$RHO_HOME" "$TAU_HOME"

MOCK_PID=""
cleanup() {
  [[ -n "$MOCK_PID" ]] && kill "$MOCK_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

start_mock() { # $1=latency_ms $2=chunk_size(0=whole)
  local latency="$1" chunk="$2" extra=()
  [[ "$chunk" -gt 0 ]] && extra=(--chunk-size "$chunk")
  "$MOCK_BIN" --body "$FIXTURE" --addr "127.0.0.1:$PORT" \
    --latency-ms "$latency" "${extra[@]}" >"$WORK/mock.log" 2>&1 &
  MOCK_PID=$!
  sleep 1
}
stop_mock() { [[ -n "$MOCK_PID" ]] && kill "$MOCK_PID" 2>/dev/null || true; MOCK_PID=""; }

# One-time provider registration into the throwaway config dirs.
echo ">> registering providers in $WORK"
env RHO_HOME="$RHO_HOME" "$RHO_BIN" setup --provider bench \
  --base-url "http://127.0.0.1:$PORT/v1" --api-key-env OPENAI_API_KEY \
  --model "$MODEL" --set-default >/dev/null
# tau requires setup options BEFORE the `setup` positional (variadic-arg quirk).
env HOME="$TAU_HOME" UV_CACHE_DIR="$UV_CACHE" uv run --project "$TAU_CHECKOUT" tau \
  --provider openai --model "$MODEL" --base-url "http://127.0.0.1:$PORT/v1" \
  --api-key-env OPENAI_API_KEY setup >/dev/null 2>&1 </dev/null

RHO_PRINT="env RHO_HOME=$RHO_HOME OPENAI_API_KEY=dummy $RHO_BIN -p 'Say hello' --provider bench -o text"
TAU_PRINT="env HOME=$TAU_HOME UV_CACHE_DIR=$UV_CACHE OPENAI_API_KEY=dummy uv run --project $TAU_CHECKOUT tau -p 'Say hello' --provider openai --model $MODEL -o text"

run_variant() { # $1=label $2=latency_ms $3=chunk_size
  local label="$1"
  echo ">> cold-start variant: $label (latency=${2}ms chunk=${3})"
  start_mock "$2" "$3"
  hyperfine --warmup "$WARMUP" --runs "$RUNS" --shell=default \
    --export-json "$RESULTS_DIR/cold_start_${label}.json" \
    -n "rho ($label)" "$RHO_PRINT" \
    -n "tau ($label)" "$TAU_PRINT"
  stop_mock
}

run_variant "0ms" 0 0
run_variant "20ms-chunk" 20 16

# --version: pure startup, no provider needed.
echo ">> cold-start variant: version (--version only)"
hyperfine --warmup "$WARMUP" --runs "$RUNS" --shell=default \
  --export-json "$RESULTS_DIR/cold_start_version.json" \
  -n "rho (version)" "$RHO_BIN --version" \
  -n "tau (version)" "env HOME=$TAU_HOME UV_CACHE_DIR=$UV_CACHE uv run --project $TAU_CHECKOUT tau --version"

echo ">> cold-start done — results in $RESULTS_DIR"
