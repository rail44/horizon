#!/bin/sh
# Builds the Markdown extension's package (`scripts/build-extension.sh`)
# and runs the tests that show it in a pane of the shell
# (`tests/extension.rs`), with no window. Not part of the gate: the extension's build and wasmtime's
# compiling of it take a while.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")/.."
scripts/build-extension.sh plugins/markdown
cargo nextest run -p horizon --test extension --run-ignored only --locked
