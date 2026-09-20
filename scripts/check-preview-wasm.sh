#!/usr/bin/env bash
set -euo pipefail

# Builds the `horizon` library for wasm32-wasip2 -- the target a preview
# pane's plugin view compiles to, so the plugin can import view code and
# the theme from this crate rather than duplicating them.
#
# Everything in src/ that needs the host (sockets, PTYs, the daemon
# clients, the native gpui platform backend) is gated
# `#[cfg(not(target_family = "wasm"))]` at its module declaration, and the
# dependencies those modules pull in live under
# [target.'cfg(not(target_family = "wasm"))'.dependencies] in Cargo.toml.
# Nothing else fails when that gating slips: the native build stays green,
# so this check is the only thing that notices.
#
# Not wired into hooks/pre-commit.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
target="wasm32-wasip2"

cd "$repo_root"

if command -v rustup > /dev/null 2>&1; then
  if ! rustup target list --installed 2> /dev/null | grep -qx "$target"; then
    echo "preview-wasm: the $target target is not installed" >&2
    echo "  install it with: rustup target add $target" >&2
    exit 1
  fi
fi

echo "preview-wasm: cargo check -p horizon --lib --target $target"
if ! cargo check -p horizon --lib --target "$target" --locked; then
  echo >&2
  echo "preview-wasm: the horizon library does not build for $target." >&2
  echo "  A module or dependency reachable from src/lib.rs needs the host." >&2
  echo "  Gate the module with #[cfg(not(target_family = \"wasm\"))] at its" >&2
  echo "  declaration, and move the dependency it needs under" >&2
  echo "  [target.'cfg(not(target_family = \"wasm\"))'.dependencies]." >&2
  exit 1
fi

echo "preview-wasm: ok"
