#!/usr/bin/env bash
# Build every example rho extension guest to a wasm32-wasip2 component.
#
# The `wasm32-wasip2` Rust target must be installed:
#     rustup target add wasm32-wasip2
#
# Each guest is its own detached cargo workspace (so `cargo build --workspace`
# at the repo root never tries to build them for the host), hence the explicit
# --manifest-path per crate. Output lands at:
#     examples/extensions/<name>/target/wasm32-wasip2/release/<name>.wasm
#
# The rho-ext-host integration tests build these on demand the same way; this
# script is for building them by hand.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for guest in hello_tool permission_gate sandbox_probe; do
    echo ">> building ${guest}"
    cargo build --release --target wasm32-wasip2 \
        --manifest-path "${here}/${guest}/Cargo.toml"
done

echo "done: components under examples/extensions/<name>/target/wasm32-wasip2/release/"
