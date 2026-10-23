#!/usr/bin/env bash
set -euo pipefail

# Installs DuckDB 2.0's prebuilt shared library into `~/opt/duckdb/<version>`
# (no root needed), so builds link DuckDB instead of compiling it.
#
# This is a machine-level install, deliberately outside the repository: the
# repo stays config-free and every worktree of it picks the library up from
# the user-level cargo config below.
#
# Wire it up once, in `~/.cargo/config.toml` (replace <you>):
#
#   [env]
#   DUCKDB_LIB_DIR = "/home/<you>/opt/duckdb/current"
#
#   [target.'cfg(target_os = "linux")']
#   rustflags = ["-C", "link-arg=-Wl,-rpath,/home/<you>/opt/duckdb/current"]
#
#   [target.'cfg(target_os = "macos")']
#   rustflags = ["-C", "link-arg=-Wl,-rpath,/Users/<you>/opt/duckdb/current"]
#
# The rpath is what makes the built binary load *this* library: libduckdb-sys
# links by name only, so without it the loader resolves `libduckdb.so` from
# the system (1.5.0 on Void) and the binary would run a different engine than
# the bindings target. An rpath also survives a GUI started from a desktop
# entry, which inherits no shell environment.
#
# Prefer the shared archive over the static one: DuckDB 2.0 ships
# `core_functions` (and `now()`, `range()`, …) as a separate static archive,
# which libduckdb-sys's static link does not pull in -- a statically linked
# `libduckdb_static.a` alone fails with "Scalar Function with name "now" is
# not in the catalog". The shared library has them built in.
#
# Without this install, `cargo build --workspace --features
# horizon-agent/bundled-duckdb` compiles DuckDB from source (~7 min per
# build/check/clippy/test profile).
#
#   scripts/install-duckdb-prefix.sh [--force]

usage() {
  echo "usage: $0 [--force]" >&2
  exit 2
}

force=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --force) force=1 ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
  shift
done

# The engine version the `duckdb` crate's version encodes: 1.20000.0 => 2.0.0.
# Bump this (and the checksums below) together with that crate.
duckdb_version="v2.0.0"

script_dir="$(cd "$(dirname "$0")" && pwd)"
prefix="${DUCKDB_PREFIX:-$HOME/opt/duckdb}"
dest="$prefix/${duckdb_version#v}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) platform="linux-amd64" ;;
      aarch64 | arm64) platform="linux-arm64" ;;
      *) echo "install-duckdb: unsupported architecture '$arch'" >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      x86_64) platform="osx-amd64" ;;
      arm64 | aarch64) platform="osx-arm64" ;;
      *) echo "install-duckdb: unsupported architecture '$arch'" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "install-duckdb: unsupported OS '$os'" >&2
    exit 1
    ;;
esac

# sha256 of duckdb-shared-libs-<platform>.tar.gz for $duckdb_version, taken
# from the release's published asset digests.
case "$platform" in
  linux-amd64) sha256="993ad89e7dfca66cd6da994c2cad090c2fc160ddf9f550498a098ab4bb13cfb1" ;;
  linux-arm64) sha256="c60eecf91143ca6bbac55c4f4d40dc22520488b0ac257c86e0fe9a2b451cf2b7" ;;
  osx-amd64) sha256="98fef9fcad856555ca0403fe4d9d6680acfd26c67e8c6f004d61a248d6bc0eff" ;;
  osx-arm64) sha256="c35d60080b31ba5d11a3ce505a6831be784f003b4c575d32a4c2badb3b41cdf0" ;;
  *) echo "install-duckdb: no checksum for '$platform'" >&2; exit 1 ;;
esac

case "$os" in
  Linux) libname="libduckdb.so" ;;
  Darwin) libname="libduckdb.dylib" ;;
esac

name="duckdb-shared-libs-$platform.tar.gz"

if [[ $force -eq 0 && -f "$dest/$libname" && -f "$dest/duckdb.h" ]]; then
  echo "install-duckdb: $duckdb_version is already installed in $dest"
else
  echo "install-duckdb: downloading $name ($duckdb_version)"
  mkdir -p "$dest"
  tmp="$(mktemp "$dest/.download.XXXXXX")"
  trap 'rm -f "$tmp"' EXIT

  url="https://github.com/duckdb/duckdb/releases/download/$duckdb_version/$name"
  if command -v curl > /dev/null 2>&1; then
    curl --fail --location --silent --show-error --output "$tmp" "$url"
  elif command -v wget > /dev/null 2>&1; then
    wget --quiet --output-document="$tmp" "$url"
  else
    echo "install-duckdb: neither curl nor wget is available" >&2
    exit 1
  fi

  if command -v sha256sum > /dev/null 2>&1; then
    actual="$(sha256sum "$tmp" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$tmp" | cut -d' ' -f1)"
  fi
  if [[ "$actual" != "$sha256" ]]; then
    echo "install-duckdb: checksum mismatch for $name" >&2
    echo "  expected $sha256" >&2
    echo "  got      $actual" >&2
    exit 1
  fi

  # Flat tarball: the shared library and the public headers side by side,
  # which is the layout DUCKDB_LIB_DIR expects (it serves as the include dir
  # too).
  rm -f "$dest"/*.so "$dest"/*.dylib "$dest"/*.h
  tar xzf "$tmp" -C "$dest"
  rm -f "$tmp"
  trap - EXIT

  if [[ ! -f "$dest/$libname" ]]; then
    echo "install-duckdb: the archive did not contain $libname" >&2
    exit 1
  fi
fi

ln -sfn "$dest" "$prefix/current"
echo "install-duckdb: $duckdb_version installed in $dest"
echo "install-duckdb: point DUCKDB_LIB_DIR and an rpath at $prefix/current (see the header of this script)"
