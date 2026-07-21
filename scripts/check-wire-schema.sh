#!/usr/bin/env bash
# Merge-time skew-discipline check for the session-wire schema artifact
# (docs/remoc-adoption-design.md §4 rule 3, second half). The nextest drift
# test (crates/horizon-sessiond/tests/wire_schema.rs) already guarantees the
# committed artifact matches the live wire types; this script compares that
# artifact against the merge-base's copy and fails if any change is a
# *reshape* (removed/renamed/reordered/retyped, or newly required) rather
# than additive (new optional field, appended variant, new definition) —
# unless the same change bumps SESSION_PROTOCOL_VERSION, which the artifact
# embeds as x-session-protocol-version. Classification lives in
# horizon_session_protocol::schema_check; this wrapper only supplies git
# plumbing. Runs from hooks/pre-commit.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
artifact="crates/horizon-session-protocol/schema/session-wire.json"
# The ref additive-only evolution is measured against; override for a PR
# stacked on another branch, e.g. WIRE_SCHEMA_BASE=origin/feature.
base_ref="${WIRE_SCHEMA_BASE:-origin/main}"

cd "$repo_root"

if ! base="$(git merge-base HEAD "$base_ref" 2>/dev/null)"; then
  echo "wire-schema: no merge-base with $base_ref (shallow clone or unfetched remote); skipping"
  exit 0
fi

if ! old="$(git show "$base:$artifact" 2>/dev/null)"; then
  echo "wire-schema: no committed artifact at merge-base $base; nothing to classify"
  exit 0
fi

if git diff --quiet "$base" -- "$artifact" 2>/dev/null && [ -f "$artifact" ]; then
  # Fast path: the artifact is byte-identical to the merge-base's copy.
  echo "wire-schema: artifact unchanged since merge-base"
  exit 0
fi

old_file="$(mktemp "${TMPDIR:-/tmp}/session-wire-base.XXXXXX.json")"
trap 'rm -f "$old_file"' EXIT
printf '%s\n' "$old" > "$old_file"

cargo run --quiet -p horizon-session-protocol --example check_wire_schema -- \
  "$old_file" "$artifact"
