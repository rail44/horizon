#!/bin/sh
# Builds the preview plugin for wasm32-wasip2 and runs the tests that show
# a view from it (`app/tests/preview.rs`), with no window. Not part of the
# gate: the plugin's build and wasmtime's compiling of it take a while.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")/.."
cargo build -p horizon-preview-plugin --target wasm32-wasip2 --locked
cargo nextest run -p horizon --test preview --run-ignored only --locked
