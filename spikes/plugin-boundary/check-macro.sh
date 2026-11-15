#!/bin/sh
# Builds the plugin described once in Rust (`guest-macro`, through
# `plugin-macro`) for wasm32-wasip2 and reads it as a host would
# (`host-macro/tests/macro.rs`). `--nocapture` prints the world the macro
# wrote.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")"
cargo build -p guest-macro --target wasm32-wasip2
cargo test -p host-macro -- --nocapture --test-threads=1
