#!/bin/sh
# One-command entry to the code database views (duckdb-ext/sql/README.md).
#
# Usage: duckdb-ext/sql/run.sh COMMAND REPO [ARGS...]
#   index REPO                          write REPO/target/code-db/index.scip (rust-analyzer
#                                       scip) and REPO/target/code-db/jscpd/{exact,normalized}
#   query REPO QUERY [NAME=VALUE ...]   load the views and the persistent cache, then run
#                                       queries/QUERY.sql and print the result
#   sql REPO FILE.sql [NAME=VALUE ...]  the same loading, then FILE.sql
#   shell REPO [NAME=VALUE ...]         the same loading, then an interactive session
#
# NAME=VALUE sets a DuckDB variable before anything is read: a number, true, false or
# NULL is passed as it is, anything else as a string. NAME:=EXPR passes EXPR as SQL text
# (for lists and maps: 'exclude_patterns:=['"'"'src/*'"'"']'). Two names belong to this
# script: FORMAT (a DuckDB CLI mode, default markdown; box, csv, json ...) and LIMIT (the
# number of rows printed, by wrapping the query in a SELECT ... LIMIT; query only).
#
# The views are read in README.md's order; views/scip.sql and views/graph.sql only when
# the SCIP index exists. refresh.sql keeps the cache in REPO/target/code-db/cache.duckdb.
# Environment: DUCKDB_CLI (default ~/.local/bin/duckdb2), RUST_ANALYZER (default
# rust-analyzer), JSCPD_PATHS (directories jscpd scans, default the ones of src, crates
# and preview-plugin/src that exist).
set -eu

sql_rel=duckdb-ext/sql
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cli=${DUCKDB_CLI:-$HOME/.local/bin/duckdb2}
ext_dir=$root/duckdb-ext/target/extension/v2.0.0/linux_amd64

die() {
    echo "run.sh: $*" >&2
    exit 1
}

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

now() {
    date +%s.%N
}

elapsed() {
    awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.1f", b - a }'
}

[ $# -ge 2 ] || usage
command=$1
[ -d "$2" ] || die "no such directory: $2"
repo=$(cd "$2" && pwd)
shift 2
out=$repo/target/code-db

sql_string() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

# Turn NAME=VALUE / NAME:=EXPR arguments into SET VARIABLE statements; FORMAT and LIMIT
# go to this script.
format=markdown
limit=
variables=
set_variables() {
    for arg in "$@"; do
        case $arg in
            *:=*) name=${arg%%:=*}; value=${arg#*:=} ;;
            *=*)
                name=${arg%%=*}; value=${arg#*=}
                if ! printf '%s' "$value" | grep -Eqx -- '-?[0-9]+(\.[0-9]+)?|true|false|NULL|null'; then
                    value=$(sql_string "$value")
                fi ;;
            *) die "expected NAME=VALUE, got: $arg" ;;
        esac
        case $name in
            '' | *[!A-Za-z0-9_]*) die "not a variable name: $name" ;;
            FORMAT) format=${arg#*=} ;;
            LIMIT)
                limit=${arg#*=}
                case $limit in '' | *[!0-9]*) die "LIMIT must be a number: $limit" ;; esac ;;
            *) variables="${variables}SET VARIABLE $name = $value;
" ;;
        esac
    done
}

check_tools() {
    [ -x "$cli" ] || die "DuckDB 2.0.0 CLI not found at $cli (set DUCKDB_CLI)"
    for ext in git_history scip_index; do
        [ -f "$ext_dir/$ext.duckdb_extension" ] ||
            die "extension $ext not built: run duckdb-ext/build.sh in $root"
    done
}

check_inputs() {
    for mode in exact normalized; do
        [ -f "$out/jscpd/$mode/jscpd-report.json" ] ||
            die "no jscpd report at $out/jscpd/$mode/jscpd-report.json: run '$0 index $repo'"
    done
}

# needs_index FILE: whether FILE says it needs the SCIP or graph views.
needs_index() {
    grep -q 'Needs views/\(scip\|graph\)\.sql' "$1"
}

# The statements that load everything: variables, the view files, refresh.sql.
load_script() {
    printf "SET VARIABLE repo = %s;\n" "$(sql_string "$repo")"
    printf '%s' "$variables"
    echo ".output /dev/null"
    for f in 00_load.sql views/files.sql views/nodes.sql views/blobs.sql lang/rust.sql \
        views/units.sql views/history.sql views/unit_changes.sql views/clones.sql; do
        echo ".read $sql_rel/$f"
    done
    if [ -f "$out/index.scip" ]; then
        echo ".read $sql_rel/views/scip.sql"
        echo ".read $sql_rel/views/graph.sql"
    fi
    echo ".read $sql_rel/refresh.sql"
    echo ".output"
    echo ".mode $format"
    echo ".maxrows -1"
    echo ".maxwidth 0"
}

run_cli() {
    cd "$root"
    mkdir -p "$out"
    "$cli" -unsigned -bail -no-agent -batch
}

case $command in
    index)
        [ $# -eq 0 ] || usage
        mkdir -p "$out"
        if [ -n "${JSCPD_PATHS:-}" ]; then
            # shellcheck disable=SC2086
            set -- $JSCPD_PATHS
        else
            set --
            for d in src crates preview-plugin/src; do
                [ -d "$repo/$d" ] && set -- "$@" "$d"
            done
        fi
        [ $# -gt 0 ] || die "no directory to scan in $repo (set JSCPD_PATHS)"
        [ -x "$root/target/refactor-audit/tools/jscpd" ] || [ -n "${JSCPD:-}" ] ||
            die "jscpd not installed: run 'uv run --locked --script scripts/refactor-audit/run.py setup' in $root"
        start=$(now)
        "$here/jscpd.sh" "$repo" "$out/jscpd" "$@" >/dev/null 2>"$out/jscpd.log" ||
            die "jscpd failed; see $out/jscpd.log"
        echo "jscpd ($*): $(elapsed "$start") s -> $out/jscpd (log $out/jscpd.log)" >&2
        ra=${RUST_ANALYZER:-rust-analyzer}
        command -v "$ra" >/dev/null || die "rust-analyzer not found (set RUST_ANALYZER)"
        start=$(now)
        (cd "$repo" && "$ra" scip . --output "$out/index.scip.tmp" >"$out/index.scip.log" 2>&1) ||
            die "rust-analyzer scip failed; see $out/index.scip.log"
        mv "$out/index.scip.tmp" "$out/index.scip"
        echo "rust-analyzer scip: $(elapsed "$start") s -> $out/index.scip (log $out/index.scip.log)" >&2
        ;;
    query)
        [ $# -ge 1 ] || usage
        query=${1%.sql}
        shift
        file=$here/queries/$query.sql
        if [ ! -f "$file" ]; then
            echo "run.sh: no query $query; available:" >&2
            ls "$here/queries" | sed -n 's/\.sql$//p' | sed 's/^/  /' >&2
            exit 1
        fi
        set_variables "$@"
        check_tools
        check_inputs
        if needs_index "$file" && [ ! -f "$out/index.scip" ]; then
            die "$query needs the SCIP index $out/index.scip: run '$0 index $repo'"
        fi
        {
            load_script
            if [ -n "$limit" ]; then
                printf 'SELECT * FROM (\n%s\n) LIMIT %s;\n' "$(sed 's/;[[:space:]]*$//' "$file")" "$limit"
            else
                cat "$file"
            fi
        } | run_cli
        ;;
    sql)
        [ $# -ge 1 ] || usage
        [ -f "$1" ] || die "no such file: $1"
        file=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
        shift
        set_variables "$@"
        [ -z "$limit" ] || die "LIMIT applies to query only"
        check_tools
        check_inputs
        if needs_index "$file" && [ ! -f "$out/index.scip" ]; then
            die "$file needs the SCIP index $out/index.scip: run '$0 index $repo'"
        fi
        {
            load_script
            echo ".read $file"
        } | run_cli
        ;;
    shell)
        set_variables "$@"
        check_tools
        check_inputs
        [ -f "$out/index.scip" ] ||
            echo "run.sh: no SCIP index at $out/index.scip; loading without views/scip.sql and views/graph.sql" >&2
        init=$(mktemp)
        trap 'rm -f "$init"' EXIT INT TERM
        load_script >"$init"
        echo ".bail off" >>"$init"
        cd "$root"
        mkdir -p "$out"
        "$cli" -unsigned -bail -init "$init"
        ;;
    *) usage ;;
esac
