#!/bin/sh
# Builds the Markdown extension (`plugins/markdown`) for wasm32-wasip2 and
# puts its package together -- the manifest beside the component it names --
# in a directory (`target/extensions/markdown` unless one is given). The
# running Horizon shows it as it was just built:
#
#   scripts/package-extension.sh
#   horizon new target/extensions/markdown README.md
#
# Needs the target: rustup target add wasm32-wasip2
set -eu
cd "$(dirname "$0")/.."
out="${1:-target/extensions/markdown}"
cargo build -p horizon-markdown-plugin --target wasm32-wasip2 --locked
mkdir -p "$out"
cp plugins/markdown/plugin.toml "$out/plugin.toml"
cp target/wasm32-wasip2/debug/horizon_markdown_plugin.wasm "$out/markdown.wasm"
echo "package-extension: $out"
