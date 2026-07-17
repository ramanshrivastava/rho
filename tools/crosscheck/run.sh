#!/usr/bin/env bash
# Crosscheck runner: tau/rho differential harness.
#
# 1. Regenerates the tau side (the oracle) with a frozen clock, so the normalized
#    expected streams are deterministic across runs and languages.
# 2. Runs the rho side, which replays the identical scripted fake-provider
#    sessions through the same serialization path as `rho -p --output-format
#    json` (the JsonEventRenderer), normalizes with the ported rules, and asserts
#    byte-equality against tools/crosscheck/expected/*.jsonl.
#
# The rho side is a CI-runnable test that does NOT need uv/tau (the expected
# files are committed); this script additionally refreshes the tau side to
# confirm the oracle has not drifted.
set -euo pipefail

TAU="${TAU_CHECKOUT:-/Users/ramanshrivastava/code/oss-gold/tau}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"

echo "crosscheck: TAU side @ ${TAU}"
uv run --project "${TAU}" python "${HERE}/driver.py"

echo "crosscheck: RHO side (cargo test -p rho-coding --test crosscheck)"
( cd "${ROOT}" && cargo test -p rho-coding --test crosscheck -- --nocapture )

echo "crosscheck: OK — rho matches tau on all scenarios"
