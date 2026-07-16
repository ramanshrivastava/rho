#!/usr/bin/env bash
# Crosscheck runner (M0 skeleton: TAU side only).
#
# Runs scripted fake-provider sessions through tau's print-mode JSON serialization
# and writes normalized golden streams to tools/crosscheck/expected/.
#
# At M4a the rho side plugs in here: run the same scenarios through `rho -p
# --output json`, normalize, and diff against expected/. See driver.py TODO(M4a).
set -euo pipefail

TAU="${TAU_CHECKOUT:-/Users/ramanshrivastava/code/oss-gold/tau}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "crosscheck: TAU side @ ${TAU}"
uv run --project "${TAU}" python "${HERE}/driver.py"

# TODO(M4a): rho side
#   for name in text tool multiturn; do
#     rho -p --output json < scenarios/$name ... | normalizer > got/$name.jsonl
#     diff -u expected/$name.jsonl got/$name.jsonl
#   done
echo "crosscheck: rho side not wired yet (M4a)"
