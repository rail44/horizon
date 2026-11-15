#!/bin/sh
# Builds an extension for wasm32-wasip2 and puts its component beside its
# manifest, where the manifest names it: the extension's directory is then
# its package, as the configuration's `extensions` and `horizon preview`
# take one. The component is not kept in git.
#
#   scripts/build-extension.sh [--release] [<extension's directory>]
#
# The directory is `plugins/markdown` unless one is given. A pane that
# shows the extension reads it again by itself once this has run.
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")/.."

profile=debug
if [ "${1:-}" = "--release" ]; then
  profile=release
  shift
fi
directory="${1:-plugins/markdown}"

# The crate's name, and the file the manifest names for its component.
field() {
  sed -n "s/^$1 *= *\"\(.*\)\" *$/\1/p" "$2" | head -n 1
}
crate="$(field name "$directory/Cargo.toml")"
component="$(field component "$directory/plugin.toml")"
if [ -z "$crate" ] || [ -z "$component" ]; then
  echo "build-extension: $directory has no crate name or no component in its manifest" >&2
  exit 1
fi

if [ "$profile" = release ]; then
  cargo build -p "$crate" --target wasm32-wasip2 --locked --release
else
  cargo build -p "$crate" --target wasm32-wasip2 --locked
fi
# (The target normalizes `-` in a crate's name to `_` in the file's.)
built="target/wasm32-wasip2/$profile/$(printf '%s' "$crate" | tr - _).wasm"
# Written beside and moved into place: a pane that watches the component
# never reads half of it.
cp "$built" "$directory/$component.building"
mv "$directory/$component.building" "$directory/$component"
echo "build-extension: $directory/$component ($profile)"
