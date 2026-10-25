#!/bin/sh
# Download the pinned `scip` CLI (the parity reference of scip-index-core's
# tests) into duckdb-ext/target/tools/ and print its path. Never a runtime
# dependency of the extension.
set -eu

VERSION=v0.10.0
ARCHIVE=scip-linux-amd64.tar.gz
SHA256=eeb28ebbff443c01609fb1691809958f3f0c7a8651af689a5bc8560061cd8ab0

cd "$(dirname "$0")/.."
dir=target/tools/scip-$VERSION
if [ ! -x "$dir/scip" ]; then
    rm -rf "$dir.download"
    mkdir -p "$dir.download"
    curl -sSfL -o "$dir.download/$ARCHIVE" \
        "https://github.com/scip-code/scip/releases/download/$VERSION/$ARCHIVE"
    echo "$SHA256  $dir.download/$ARCHIVE" | sha256sum -c --quiet
    tar -xzf "$dir.download/$ARCHIVE" -C "$dir.download" scip
    mkdir -p "$dir"
    mv "$dir.download/scip" "$dir/scip"
    rm -rf "$dir.download"
fi
realpath "$dir/scip"
