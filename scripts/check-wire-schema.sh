#!/usr/bin/env bash
# Merge-time skew-discipline check for the wire-schema artifacts
# (docs/remoc-adoption-design.md §4 rule 3, second half). The nextest drift
# tests (crates/horizon-agent/tests/wire_schema.rs,
# crates/horizon-terminal-core/tests/wire_schema.rs) already guarantee each
# committed artifact matches its live wire types; this script compares them
# against the merge-base's copies and fails if any change is a *reshape*
# (removed/renamed/reordered/retyped, or newly required) rather than
# additive (new optional field, appended variant, new definition) -- unless
# the same change bumps that hub's protocol version, which the artifact
# embeds as x-session-protocol-version. Classification lives in
# horizon_wire::schema_check; this wrapper only supplies git plumbing.
# Runs from hooks/pre-commit.
#
# There are two artifacts because there are two runtimes
# (docs/runtime-crate-alignment-design.md phase 2): each hub carries its own
# version pair, so an agent-side bump no longer drains horizon-terminald's
# PTYs. A branch whose merge-base predates that split has neither file
# there, only the union `session-wire.json` -- handled by the transition arm
# below, which reassembles the two into the union and classifies *that*
# rather than skipping the check.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
agent_artifact="crates/horizon-agent/schema/agent-wire.json"
terminal_artifact="crates/horizon-terminal-core/schema/terminal-wire.json"
# The pre-phase-2 union artifact, only ever read out of an old merge-base.
union_artifact="crates/horizon-session-protocol/schema/session-wire.json"
# The ref additive-only evolution is measured against; override for a PR
# stacked on another branch, e.g. WIRE_SCHEMA_BASE=origin/feature.
base_ref="${WIRE_SCHEMA_BASE:-origin/main}"

cd "$repo_root"

if ! base="$(git merge-base HEAD "$base_ref" 2>/dev/null)"; then
  echo "wire-schema: no merge-base with $base_ref (shallow clone or unfetched remote); skipping"
  exit 0
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/horizon-wire-schema.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

# Writes "$base:$1" to "$2"; returns non-zero when the merge-base has no
# such file.
extract_base() {
  git show "$base:$1" > "$2" 2>/dev/null
}

classify_one() {
  local artifact="$1" label="$2"
  if git diff --quiet "$base" -- "$artifact" 2>/dev/null && [ -f "$artifact" ]; then
    # Fast path: the artifact is byte-identical to the merge-base's copy.
    echo "wire-schema: $label artifact unchanged since merge-base"
    return 0
  fi
  echo "wire-schema: classifying $label"
  cargo run --quiet -p horizon-wire --example check_wire_schema -- \
    "$work_dir/base-$label.json" "$artifact"
}

if extract_base "$agent_artifact" "$work_dir/base-agent.json" \
  && extract_base "$terminal_artifact" "$work_dir/base-terminal.json"; then
  # Steady state: both artifacts existed at the merge-base, so each is
  # classified against its own predecessor.
  classify_one "$agent_artifact" agent
  classify_one "$terminal_artifact" terminal
  exit 0
fi

if extract_base "$union_artifact" "$work_dir/base-union.json"; then
  # Transition: the merge-base predates the artifact split. Reassemble this
  # tree's two artifacts into the union they were split out of and classify
  # that against the merge-base's copy -- the split is then held to exactly
  # the same standard as any other wire change (a section that quietly lost
  # a definition, a channel, or a hub method still fails).
  cargo run --quiet -p horizon-wire --example check_wire_schema -- \
    --transition "$work_dir/base-union.json" "$agent_artifact" "$terminal_artifact"
  exit $?
fi

echo "wire-schema: no committed artifact at merge-base $base; nothing to classify"
exit 0
