#!/usr/bin/env bash
# Runs a cargo command over the extensions in this repository
# (`plugins/*`), for the target they are built for: wasm components,
# which `cargo build --workspace` does not build. Five Horizon comes
# with; the components' gallery is one to look at in a preview pane, and
# the fixture is what tests drive the host with.
#
#   scripts/extensions.sh build              # beside a debug Horizon
#   scripts/extensions.sh build --release    # beside a release one
#   scripts/extensions.sh clippy -- -D warnings
#
# Horizon reads each from beside its executable, or from where this
# leaves it (`src/extension.rs`'s `own`, which names the same files).
#
# Needs the target: rustup target add wasm32-wasip2
set -euo pipefail

cd "$(dirname "$0")/.."

command="$1"
shift
exec cargo "$command" \
  -p horizon-markdown-plugin \
  -p horizon-theme-settings-plugin \
  -p horizon-wiki-plugin \
  -p horizon-sessions-plugin \
  -p horizon-image-plugin \
  -p horizon-status-bar-plugin \
  -p horizon-gallery-plugin \
  -p horizon-fixture-plugin \
  --target wasm32-wasip2 "$@"
