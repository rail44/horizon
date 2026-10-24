#!/bin/sh
# The sub-workspace's whole check: format, lints and tests of every member
# (the extension, git-history-core, gix-probe), a release build with the
# metadata footer, and the host check that LOADs the result through the
# `duckdb` crate and the machine's libduckdb.
set -eu
cd "$(dirname "$0")"
nice -n 19 cargo fmt --all --check
nice -n 19 cargo clippy --locked -j 4 --workspace --all-targets -- -D warnings
nice -n 19 cargo test --locked -j 4 --workspace
./build.sh
cd host-check
nice -n 19 cargo fmt --check
nice -n 19 cargo clippy --locked -j 4 --all-targets -- -D warnings
nice -n 19 cargo test --locked -j 4
