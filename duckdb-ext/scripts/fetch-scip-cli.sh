#!/bin/sh
# Download the pinned `scip` CLI (the parity reference of scip-index-core's
# tests) into duckdb-ext/target/tools/ and print its path. Never a runtime
# dependency of the extension.
set -eu

VERSION=v0.10.0
# The release's archive for this machine and its sha256 (the release's own
# asset digests, of which linux-amd64's was the first pin).
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) ARCHIVE=scip-linux-amd64.tar.gz
        SHA256=eeb28ebbff443c01609fb1691809958f3f0c7a8651af689a5bc8560061cd8ab0 ;;
    Linux/aarch64) ARCHIVE=scip-linux-arm64.tar.gz
        SHA256=6ab677dc2c4bf2955975d0530766152e45daaa988f9404068d8adecacd0bb24c ;;
    Darwin/arm64) ARCHIVE=scip-darwin-arm64.tar.gz
        SHA256=7ea200390e0790b3da8999b7b1cd4e3597700dcb3d354911872523bfe0090779 ;;
    Darwin/x86_64) ARCHIVE=scip-darwin-amd64.tar.gz
        SHA256=e1d5eb355e5fcfc1c6d6479dd94cb9334c24262a34e38ff514dd5a2231b779ec ;;
    *) echo "fetch-scip-cli.sh: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
dir=target/tools/scip-$VERSION
if [ ! -x "$dir/scip" ]; then
    rm -rf "$dir.download"
    mkdir -p "$dir.download"
    curl -sSfL -o "$dir.download/$ARCHIVE" \
        "https://github.com/scip-code/scip/releases/download/$VERSION/$ARCHIVE"
    # GNU's or BSD's sha256sum (whose -c options differ), else perl's shasum.
    if command -v sha256sum >/dev/null; then
        actual=$(sha256sum < "$dir.download/$ARCHIVE")
    else
        actual=$(shasum -a 256 < "$dir.download/$ARCHIVE")
    fi
    if [ "${actual%% *}" != "$SHA256" ]; then
        echo "fetch-scip-cli.sh: $ARCHIVE: sha256 ${actual%% *}, expected $SHA256" >&2
        exit 1
    fi
    tar -xzf "$dir.download/$ARCHIVE" -C "$dir.download" scip
    mkdir -p "$dir"
    mv "$dir.download/scip" "$dir/scip"
    rm -rf "$dir.download"
fi
realpath "$dir/scip"
