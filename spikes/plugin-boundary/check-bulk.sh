#!/bin/sh
# Builds the guest that builds the same list three ways (`guest-bulk`) for
# wasm32-wasip2 and the host that times them (`host-bulk`), both for
# release, and prints what each way costs for a thousand rows and for
# twenty: a call for each write, the writes together as typed values, the
# writes together as bytes.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")"
cargo build --release -p guest-bulk --target wasm32-wasip2
cargo build --release -p host-bulk
./target/release/host-bulk target/wasm32-wasip2/release/guest_bulk.wasm 1000
./target/release/host-bulk target/wasm32-wasip2/release/guest_bulk.wasm 20
