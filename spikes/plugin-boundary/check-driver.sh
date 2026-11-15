#!/bin/sh
# Builds the guest with tasks of its own (`guest-async`) for wasm32-wasip2
# and runs what `host-driver` holds one long-lived future per guest to
# (`host-driver/tests/driver.rs`). `--nocapture` prints what a call costs.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")"
cargo build -p guest-async --target wasm32-wasip2
cargo test -p host-driver -- --nocapture --test-threads=1
