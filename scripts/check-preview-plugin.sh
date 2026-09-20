#!/usr/bin/env bash
set -euo pipefail

# Builds the preview plugin (`preview-plugin/`, a wasm32-wasip2 component
# outside the root workspace) and runs the preview pane's end-to-end check
# against it: a real wasmtime store, no window, no GUI.
#
# The check itself is `#[ignore]`d in the `horizon` library's test target,
# so a normal `cargo nextest run --workspace` never pays for the multi-minute
# wasm build this script does first.
#
# Not wired into hooks/pre-commit.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
plugin_dir="$repo_root/preview-plugin"
target="wasm32-wasip2"
profile="quick"
artifact="$plugin_dir/target/$target/$profile/horizon_preview_plugin.wasm"

cd "$repo_root"

if command -v rustup > /dev/null 2>&1; then
  if ! rustup target list --installed 2> /dev/null | grep -qx "$target"; then
    echo "preview-plugin: the $target target is not installed" >&2
    echo "  install it with: rustup target add $target" >&2
    exit 1
  fi
fi

# Seed the plugin's lockfile from the root one so the shared crates -- gpui,
# gpui-component, embedded_gpui -- resolve to exactly the versions the shell
# is built against. cargo extends a seeded lockfile in place; only
# `cargo generate-lockfile` would discard it.
if [ ! -f "$plugin_dir/Cargo.lock" ]; then
  echo "preview-plugin: seeding Cargo.lock from the root lockfile"
  cp "$repo_root/Cargo.lock" "$plugin_dir/Cargo.lock"
fi

echo "preview-plugin: cargo build --profile $profile --target $target"
( cd "$plugin_dir" && nice -n 19 cargo build -j 4 --profile "$profile" --target "$target" )

if [ ! -f "$artifact" ]; then
  echo "preview-plugin: expected the component at $artifact" >&2
  exit 1
fi
echo "preview-plugin: built $(stat -c %s "$artifact") bytes"

echo "preview-plugin: running the end-to-end check"
HORIZON_PREVIEW_WASM="$artifact" nice -n 19 cargo nextest run -j 4 \
  -p horizon --lib --run-ignored ignored-only -E 'test(preview_plugin_)'

echo "preview-plugin: ok"
