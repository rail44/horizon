#!/bin/sh
# The sub-workspace's whole check: format, lints and tests of every member
# (both extensions, git-history-core, scip-index-core, gix-probe,
# scip-probe), a release build of both extensions with the metadata footer,
# the host check that LOADs them through the `duckdb` crate and the
# machine's libduckdb, and code-db's format, lints and tests (its end-to-end test
# needs the extensions build.sh just built).
#
# scip-index-core's parity tests need the pinned `scip` CLI, fetched here into
# target/tools/, and `rust-analyzer` on PATH.
set -eu
cd "$(dirname "$0")"
./scripts/fetch-scip-cli.sh >/dev/null
nice -n 19 cargo fmt --all --check
nice -n 19 cargo clippy --locked -j 4 --workspace --all-targets -- -D warnings
nice -n 19 cargo test --locked -j 4 --workspace
./build.sh
cd host-check
nice -n 19 cargo fmt --check
nice -n 19 cargo clippy --locked -j 4 --all-targets -- -D warnings
nice -n 19 cargo test --locked -j 4
cd ../code-db
nice -n 19 cargo fmt --check
nice -n 19 cargo clippy --locked -j 4 --all-targets -- -D warnings
nice -n 19 cargo test --locked -j 4
