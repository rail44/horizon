#!/bin/sh
# Build git_history.duckdb_extension and scip_index.duckdb_extension for this
# machine's platform (linux_amd64, linux_arm64, osx_arm64 or osx_amd64) / DuckDB
# v2.0.0, then the code-db command, and print their paths.
#
# The extensions use DuckDB's unstable C API v2 (ABI type C_STRUCT_UNSTABLE),
# so the binaries load only into exactly TARGET_DUCKDB_VERSION. They go to
# target/extension/$TARGET_DUCKDB_VERSION/$PLATFORM/, and `host` beside it links
# to that directory: the path sql/00_load.sql loads them from.
set -eu

TARGET_DUCKDB_VERSION=v2.0.0
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) PLATFORM=linux_amd64 LIB_SUFFIX=so ;;
    Linux/aarch64) PLATFORM=linux_arm64 LIB_SUFFIX=so ;;
    Darwin/arm64) PLATFORM=osx_arm64 LIB_SUFFIX=dylib ;;
    Darwin/x86_64) PLATFORM=osx_amd64 LIB_SUFFIX=dylib ;;
    *) echo "build.sh: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

cd "$(dirname "$0")"
out=target/extension/$TARGET_DUCKDB_VERSION/$PLATFORM
mkdir -p "$out"
ln -sfn "$PLATFORM" "target/extension/$TARGET_DUCKDB_VERSION/host"

# build EXTENSION_NAME CRATE_DIR
build() {
    name=$1
    version=v$(sed -n 's/^version = "\(.*\)"$/\1/p' "$2/Cargo.toml" | head -n 1)
    # The variables mirror extension-ci-tools' rust.Makefile.
    DUCKDB_EXTENSION_NAME=$name DUCKDB_EXTENSION_MIN_DUCKDB_VERSION=$TARGET_DUCKDB_VERSION \
        nice -n 19 cargo build --release --locked -j 4 -p "$name" >&2
    python3 append_metadata.py "target/release/lib$name.$LIB_SUFFIX" "$out/$name.duckdb_extension" \
        --name "$name" --platform "$PLATFORM" --duckdb-version "$TARGET_DUCKDB_VERSION" \
        --extension-version "$version" --abi-type C_STRUCT_UNSTABLE
    realpath "$out/$name.duckdb_extension"
}

build git_history git-history
build scip_index scip-index

# The entry point, code-db (sql/README.md, Running): a workspace of its own, linked
# against the machine's libduckdb (code-db/Cargo.toml).
nice -n 19 cargo build --release --locked -j 4 --manifest-path code-db/Cargo.toml >&2
realpath code-db/target/release/code-db
