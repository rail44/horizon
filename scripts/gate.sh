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

# `-D warnings` is for what rustc and clippy say of the code. What Cargo
# says of a manifest (a dependency declared and not used) is a warning of
# its own, which `build.warnings` has fail the step.
export CARGO_BUILD_WARNINGS=deny
step cargo clippy --workspace --all-targets --locked -- -D warnings

# The extensions (`plugins/*`) are built for a guest only: the clippy above sees empty crates, so they are linted for their
# own target, and the tests that show them load their components from
# where this builds them.
step ./scripts/extensions.sh clippy --locked -- -D warnings
unset CARGO_BUILD_WARNINGS
step ./scripts/extensions.sh build --locked

# Inside a Horizon sandboxed agent session the default nextest profile
# cannot pass (AGENTS.md): the tests that probe the host boundary are
# skipped only by the `sandboxed` profile. A contained call is told the
# runtime that contains it in `HORIZON_SANDBOX`
# (`crates/horizon-sandbox/src/runtime.rs`), so its being set says this run
# is contained.
case "${HORIZON_SANDBOX:-}" in
  ?*)
    step cargo nextest run --profile sandboxed --workspace --locked
    ;;
  *)
    step cargo nextest run --workspace --locked
    ;;
esac

step ./scripts/check-wire-schema.sh
echo "gate: clean"
