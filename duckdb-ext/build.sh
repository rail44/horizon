#!/bin/sh
# Build git_history.duckdb_extension for linux_amd64 / DuckDB v2.0.0 and print its path.
#
# The extension uses DuckDB's unstable C API v2 (ABI type C_STRUCT_UNSTABLE),
# so the binary loads only into exactly TARGET_DUCKDB_VERSION.
set -eu

TARGET_DUCKDB_VERSION=v2.0.0
PLATFORM=linux_amd64
EXTENSION_NAME=git_history

cd "$(dirname "$0")"
EXTENSION_VERSION=v$(sed -n 's/^version = "\(.*\)"$/\1/p' git-history/Cargo.toml | head -n 1)

# The variables mirror extension-ci-tools' rust.Makefile.
DUCKDB_EXTENSION_NAME=$EXTENSION_NAME DUCKDB_EXTENSION_MIN_DUCKDB_VERSION=$TARGET_DUCKDB_VERSION \
    nice -n 19 cargo build --release --locked -j 4 -p git_history >&2

out=target/extension/$TARGET_DUCKDB_VERSION/$PLATFORM
mkdir -p "$out"
python3 append_metadata.py "target/release/lib$EXTENSION_NAME.so" "$out/$EXTENSION_NAME.duckdb_extension" \
    --name "$EXTENSION_NAME" --platform "$PLATFORM" --duckdb-version "$TARGET_DUCKDB_VERSION" \
    --extension-version "$EXTENSION_VERSION" --abi-type C_STRUCT_UNSTABLE
realpath "$out/$EXTENSION_NAME.duckdb_extension"
