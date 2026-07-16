# rho task recipes. Run `just` to list.

# Path to the tau checkout used for fixture extraction and crosscheck.
tau := env_var_or_default("TAU_CHECKOUT", "/Users/ramanshrivastava/code/oss-gold/tau")

default:
    @just --list

# Run the full test suite.
test:
    cargo test --workspace

# Lint: clippy as errors + rustfmt check.
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check

# Auto-format.
fmt:
    cargo fmt --all

# Build everything.
build:
    cargo build --workspace

# Re-extract golden fixtures from the pinned tau revision.
# Fixtures are the correctness oracle — see AGENTS.md fixture policy before running.
refresh-fixtures:
    uv run --project {{tau}} python tools/extract-fixtures/run_all.py

# Differential harness: run scripted sessions through tau (and, from M4a, rho) and
# compare normalized event streams.
crosscheck:
    TAU_CHECKOUT={{tau}} bash tools/crosscheck/run.sh
