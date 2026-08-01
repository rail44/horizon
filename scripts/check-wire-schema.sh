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
# There is one artifact per runtime (docs/runtime-crate-alignment-design.md
# phase 2): each hub carries its own version pair, so an agent-side bump no
# longer drains horizon-terminald's PTYs. The set is *discovered*, not
# listed, so a third runtime (the WASM plugin view is the expected next
# one) cannot slip past this check by being unknown to the script.
#
#   Discovery convention: a runtime's wire-schema artifact lives at
#   crates/<crate>/schema/<name>-wire.json. Put a new one there and it is
#   picked up automatically, on both sides of the merge-base.
#
# Every discovered artifact lands in exactly one of four buckets, none of
# them silent: present on both sides (classified, or reported unchanged),
# new on this branch (reported as having no predecessor to diff), gone from
# this branch (an error -- deleting a wire is not an additive change), or,
# for a merge-base that predates the artifact split, the transition arm
# below, which reassembles this tree's artifacts into the old union
# `session-wire.json` and classifies *that* rather than skipping.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# The discovery convention above, as a matcher for paths on either side.
artifact_pattern='^crates/[^/]+/schema/[^/]+-wire\.json$'
# The pre-phase-2 union artifact, normally only read out of an old
# merge-base. It matches the pattern too, so it is special-cased only when
# this tree no longer has it (i.e. the split happened on this branch).
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

# Artifacts the merge-base has, and the ones this tree has. The tree side
# lists tracked *and* untracked-but-not-ignored files, so an artifact added
# by the very commit being checked counts as present.
base_artifacts=()
while IFS= read -r path; do
  base_artifacts+=("$path")
done < <(git ls-tree -r --name-only "$base" | grep -E "$artifact_pattern" || true)
tree_artifacts=()
while IFS= read -r path; do
  # A tracked-but-deleted path still lists here; it is not an artifact this
  # tree has, and the removal check below is what owns that case.
  if [ -f "$path" ]; then
    tree_artifacts+=("$path")
  fi
done < <(
  git ls-files --cached --others --exclude-standard | grep -E "$artifact_pattern" | sort -u || true
)

# Membership test over an array passed as the remaining arguments.
has() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    if [ "$item" = "$needle" ]; then
      return 0
    fi
  done
  return 1
}

# A label for messages: crates/x/schema/agent-wire.json -> agent.
label_of() {
  local name
  name="$(basename "$1")"
  echo "${name%-wire.json}"
}

status=0

# An artifact the merge-base had and this tree does not is a removal, which
# is never additive. The union artifact is exempt: losing it *is* the
# artifact split, handled by the transition arm below.
for artifact in ${base_artifacts[@]+"${base_artifacts[@]}"}; do
  if [ "$artifact" = "$union_artifact" ] || [ -f "$artifact" ]; then
    continue
  fi
  echo "RESHAPE:  $artifact existed at merge-base $base and is gone from this tree"
  status=1
done
if [ "$status" -ne 0 ]; then
  echo "a wire-schema artifact was removed. Dropping a runtime's wire is a reshape by" >&2
  echo "definition: it needs an owner decision and a protocol-version bump" >&2
  echo "(docs/remoc-adoption-design.md §4), and the file is what this check classifies." >&2
  exit 1
fi

if has "$union_artifact" ${base_artifacts[@]+"${base_artifacts[@]}"}; then
  base_has_union=yes
else
  base_has_union=no
fi

if [ "$base_has_union" = yes ] && [ ! -f "$union_artifact" ]; then
  # Transition: the merge-base predates the artifact split. Reassemble this
  # tree's artifacts into the union they were split out of and classify that
  # against the merge-base's copy -- the split is then held to exactly the
  # same standard as any other wire change (a section that quietly lost a
  # definition, a channel, or a hub method still fails).
  if [ "${#tree_artifacts[@]}" -eq 0 ]; then
    echo "merge-base $base has $union_artifact, but this tree has no" >&2
    echo "wire-schema artifact at all; there is nothing to reassemble the union from." >&2
    exit 1
  fi
  git show "$base:$union_artifact" > "$work_dir/base-union.json"
  cargo run --quiet -p horizon-wire --example check_wire_schema -- \
    --transition "$work_dir/base-union.json" "${tree_artifacts[@]}"
  exit $?
fi

classified=0
index=0
for artifact in ${tree_artifacts[@]+"${tree_artifacts[@]}"}; do
  label="$(label_of "$artifact")"
  index=$((index + 1))
  if ! has "$artifact" ${base_artifacts[@]+"${base_artifacts[@]}"}; then
    # A new runtime, or a renamed artifact: there is genuinely no
    # predecessor to diff. Say so rather than passing quietly, so a reader
    # can tell this apart from "checked and clean".
    echo "wire-schema: $label artifact is new since merge-base; no predecessor to classify"
    continue
  fi
  classified=$((classified + 1))
  if git diff --quiet "$base" -- "$artifact" 2>/dev/null; then
    # Fast path: the artifact is byte-identical to the merge-base's copy.
    echo "wire-schema: $label artifact unchanged since merge-base"
    continue
  fi
  echo "wire-schema: classifying $label"
  base_copy="$work_dir/base-$index-$label.json"
  git show "$base:$artifact" > "$base_copy"
  # Keep going after a failure: a second reshape in the same commit should
  # be visible in the same run.
  cargo run --quiet -p horizon-wire --example check_wire_schema -- \
    "$base_copy" "$artifact" || status=1
done

if [ "$classified" -eq 0 ]; then
  echo "wire-schema: no artifact present on both sides of merge-base $base; nothing to classify"
fi

exit "$status"
