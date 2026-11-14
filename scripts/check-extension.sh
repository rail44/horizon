#!/bin/sh
# Builds the Markdown extension (`plugins/markdown`) for wasm32-wasip2 and
# runs the tests that show it in a pane of the shell (`tests/extension.rs`),
# with no window. Not part of the gate: the extension's build and wasmtime's
# compiling of it take a while.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")/.."
cargo build -p horizon-markdown-plugin --target wasm32-wasip2 --locked
cargo nextest run -p horizon --test extension --run-ignored only --locked
