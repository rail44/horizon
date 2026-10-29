#!/bin/sh
# Produce the two jscpd reports views/clones.sql reads, for the given files of a repository.
#
# Usage: duckdb-ext/sql/jscpd.sh REPO OUT_DIR [PATH...]
#   REPO     the repository root (paths are relative to it)
#   OUT_DIR  receives exact/jscpd-report.json and normalized/jscpd-report.json
#   PATH     files (or directories) to scan; `code-db index` passes the files of the
#            view `files`, so the settings' scope applies (default: .)
#
# Uses the pinned jscpd 5.3.1 of the refactor audit (run
# `uv run --locked --script scripts/refactor-audit/run.py setup` first; JSCPD overrides
# the binary) with the audit's settings, once preserving and once normalizing identifiers
# and literals. --absolute is load-bearing: with several scan paths jscpd 5.3.1 reports
# names relative to each scan path, which clones.sql cannot map back to the repository.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
jscpd=${JSCPD:-$here/../../target/refactor-audit/tools/jscpd}
repo=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
shift 2
[ $# -gt 0 ] || set -- .

config=$out/jscpd-config.json
echo '{}' > "$config"
cd "$repo"
for mode in exact normalized; do
    if [ "$mode" = normalized ]; then
        extra="--ignore-identifiers --ignore-literals"
    else
        extra=
    fi
    # shellcheck disable=SC2086
    "$jscpd" "$@" --config "$config" --format rust --min-tokens 70 --min-lines 5 --mode weak \
        --reporters json --output "$out/$mode" --max-size 10mb --max-lines 100000 --no-colors \
        --workers 4 --absolute --silent $extra >&2
done
echo "$out/exact/jscpd-report.json"
echo "$out/normalized/jscpd-report.json"
