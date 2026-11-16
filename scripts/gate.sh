#!/usr/bin/env bash
# The local quality gate (AGENTS.md, "Commands"): its steps, in order,
# stopping at the first that fails. Nothing runs this by itself.
#
#   scripts/gate.sh
#
# It checks the formatting and changes no file: run `cargo fmt` when the
# first step fails.
set -euo pipefail

cd "$(dirname "$0")/.."

step() {
  echo "gate: $*"
  "$@"
}

step cargo fmt --all -- --check
step cargo clippy --workspace --all-targets --locked -- -D warnings

# The extensions (`plugins/*`) are built for a guest only: the clippy above sees empty crates, so they are linted for their
# own target, and the tests that show them load their components from
# where this builds them.
step ./scripts/extensions.sh clippy --locked -- -D warnings
step ./scripts/extensions.sh build --locked

# Inside a Horizon sandboxed agent session the default nextest profile
# cannot pass (AGENTS.md): the tests that probe the host boundary are
# skipped only by the `sandboxed` profile. The sandbox redirects TMPDIR to
# a `.horizon-sandbox-tmp` directory inside the worktree
# (`crates/horizon-sandbox/src/tmpdir.rs`), so that suffix says this run is
# contained.
case "${TMPDIR:-}" in
  *.horizon-sandbox-tmp)
    step cargo nextest run --profile sandboxed --workspace --locked
    ;;
  *)
    step cargo nextest run --workspace --locked
    ;;
esac

step ./scripts/check-wire-schema.sh
echo "gate: clean"
