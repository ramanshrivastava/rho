#!/usr/bin/env bash
# Crosscheck runner: tau/rho differential harness.
#
# v1 (bare AgentHarness event streams) + v2 (full CodingSession: byte-identical
# session files, event streams, and bidirectional resume-swap).
#
# 1. Regenerates the tau side (the oracle) with frozen clocks/uuids, so the
#    session files + normalized streams are deterministic and language-agnostic.
# 2. Runs the rho side, which drives the identical scenarios through
#    CodingSession, byte-diffs the session files and normalized event streams
#    against tools/crosscheck/{sessions,expected}, and asserts the tau->rho
#    resume-swap.
# 3. Runs the rho->tau resume-swap: tau resumes each committed (rho-byte-
#    identical) session file and must replay to the same state.
#
# The rho side is CI-runnable WITHOUT uv/tau (expected + session files are
# committed); this script additionally refreshes the tau side and runs the
# uv-only rho->tau resume-swap to confirm the oracle has not drifted.
set -euo pipefail

TAU="${TAU_CHECKOUT:-/Users/ramanshrivastava/code/oss-gold/tau}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"

echo "crosscheck: TAU side @ ${TAU} (v1 event streams + v2 sessions/events/state)"
uv run --project "${TAU}" python "${HERE}/driver.py"

echo "crosscheck: RHO side v1 (cargo test -p rho-coding --test crosscheck)"
( cd "${ROOT}" && cargo test -p rho-coding --test crosscheck -- --nocapture )

echo "crosscheck: RHO side v2 (session files + event streams + tau->rho resume-swap)"
( cd "${ROOT}" && cargo test -p rho-coding --test crosscheck_v2 crosscheck_v2_all_scenarios -- --nocapture )

echo "crosscheck: resume-swap rho->tau (tau resumes rho-written session files)"
TAU_CHECKOUT="${TAU}" uv run --project "${TAU}" python "${HERE}/resume_swap.py"

echo "crosscheck: OK — sessions are byte-interchangeable and resumable both ways"
