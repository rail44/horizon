#!/bin/sh
# One-command entry to the code database views (duckdb-ext/sql/README.md).
#
# Usage: duckdb-ext/sql/run.sh COMMAND REPO [ARGS...]
#   index REPO [tool_memory_max=SIZE]   write REPO/target/code-db/index.scip (rust-analyzer
#                                       scip) and REPO/target/code-db/jscpd/{exact,normalized},
#                                       each tool under a memory cap (README.md, Limits)
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
# 00_load.sql sets DuckDB's memory, thread and spill limits (README.md, Limits; variables
# memory_limit, threads, temp_limit) and its one-line summary goes to stderr.
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
    sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//' >&2
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

# The free space, in bytes, of the file system that holds the spill directory
# (00_load.sql caps spilling at half of it). Empty when df cannot tell.
temp_free_bytes() {
    mkdir -p "$out/tmp"
    df -Pk "$out/tmp" 2>/dev/null | awk 'NR == 2 && $4 ~ /^[0-9]+$/ { printf "%.0f", $4 * 1024 }'
}

# The statements that load everything: variables, the view files, refresh.sql.
# 00_load.sql's last statement, the limits line, goes to stderr; the rest of the
# loading's output is dropped.
load_script() {
    printf "SET VARIABLE repo = %s;\n" "$(sql_string "$repo")"
    free=$(temp_free_bytes)
    [ -z "$free" ] || printf "SET VARIABLE temp_free_bytes = %s;\n" "$free"
    printf '%s' "$variables"
    echo ".output /dev/stderr"
    echo ".mode list"
    echo ".headers off"
    echo ".read $sql_rel/00_load.sql"
    echo ".output /dev/null"
    echo ".headers on"
    for f in views/files.sql views/nodes.sql views/blobs.sql lang/rust.sql \
        views/units.sql views/history.sql views/unit_changes.sql views/lineage.sql views/snapshot.sql \
        views/clones.sql; do
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

# The CLI's stderr goes through a pipe to this script's: `.output /dev/stderr` reopens
# the path, which truncates and overwrites a file stderr is redirected to, but not a pipe.
run_cli() {
    cd "$root"
    mkdir -p "$out"
    exec 4>&1
    status=$({ {
        rc=0
        "$cli" -unsigned -bail -no-agent -batch 2>&1 1>&4 4>&- 5>&- || rc=$?
        echo "$rc" >&5
    } | cat >&2; } 5>&1)
    exec 4>&-
    return "$status"
}

# Memory caps for the external tools of `index` (README.md, Limits).
# to_bytes SIZE: SIZE (a number with an optional K, M, G or T suffix, base 1024, as
# systemd's MemoryMax takes it) in bytes; empty when SIZE is not one.
to_bytes() {
    printf '%s\n' "$1" | awk '/^[0-9]+(\.[0-9]+)?[KMGTkmgt]?$/ {
        n = $0 + 0; u = toupper(substr($0, length($0)))
        f = u == "K" ? 1024 : u == "M" ? 1024 ^ 2 : u == "G" ? 1024 ^ 3 : u == "T" ? 1024 ^ 4 : 1
        printf "%.0f", n * f }'
}

human() {
    awk -v b="$1" 'BEGIN { printf "%.1f GB", b / 1e9 }'
}

# Sets tool_cap (bytes, empty for none), tool_cap_with (systemd, prlimit or none) and
# prints the choice. The cap is tool_memory_max, else 80% of MemAvailable.
choose_tool_cap() {
    if [ -n "$tool_memory_max" ]; then
        tool_cap=$(to_bytes "$tool_memory_max")
        [ -n "$tool_cap" ] || die "tool_memory_max must be a size such as 8G: $tool_memory_max"
        why="tool_memory_max=$tool_memory_max"
    else
        avail=$(awk '/^MemAvailable:/ { printf "%.0f", $2 * 1024 }' /proc/meminfo 2>/dev/null || true)
        if [ -n "$avail" ]; then
            tool_cap=$(awk -v a="$avail" 'BEGIN { printf "%.0f", a * 0.8 }')
            why="80% of $(human "$avail") available"
        else
            tool_cap=
        fi
    fi
    if [ -z "$tool_cap" ]; then
        tool_cap_with=none
        echo "run.sh: warning: /proc/meminfo unreadable and tool_memory_max unset; the tools run without a memory cap" >&2
    elif command -v systemd-run >/dev/null && systemd-run --user --scope --quiet true >/dev/null 2>&1; then
        tool_cap_with=systemd
        echo "tool memory cap: $(human "$tool_cap") ($why), systemd-run --user --scope -p MemoryMax (resident memory of the tool's processes)" >&2
    elif command -v prlimit >/dev/null; then
        tool_cap_with=prlimit
        echo "tool memory cap: $(human "$tool_cap") ($why), prlimit --as (address space of each process, not resident memory)" >&2
    else
        tool_cap_with=none
        echo "run.sh: warning: neither systemd-run --user nor prlimit is available; the tools run without a memory cap" >&2
    fi
}

# capped COMMAND...: run COMMAND under the cap choose_tool_cap chose.
capped() {
    case $tool_cap_with in
        systemd) systemd-run --user --scope --quiet -p MemoryMax="$tool_cap" -p MemorySwapMax=0 -- "$@" ;;
        prlimit) prlimit --as="$tool_cap" -- "$@" ;;
        *) "$@" ;;
    esac
}

# failed TOOL STATUS LOG: the one-line error for a tool that exited with STATUS.
failed() {
    if [ "$2" -gt 128 ]; then
        how="was killed by signal $(($2 - 128))"
    else
        how="exited with status $2"
    fi
    case $tool_cap_with in
        none) die "$1 $how; see $3" ;;
        *) die "$1 $how under the $(human "$tool_cap") memory cap ($tool_cap_with; raise it with tool_memory_max=SIZE if the log shows an allocation failure); see $3" ;;
    esac
}

case $command in
    index)
        tool_memory_max=
        for arg in "$@"; do
            case $arg in
                tool_memory_max=*) tool_memory_max=${arg#*=} ;;
                *) usage ;;
            esac
        done
        mkdir -p "$out"
        choose_tool_cap
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
        rc=0
        capped "$here/jscpd.sh" "$repo" "$out/jscpd" "$@" >/dev/null 2>"$out/jscpd.log" || rc=$?
        [ "$rc" -eq 0 ] || failed jscpd "$rc" "$out/jscpd.log"
        echo "jscpd ($*): $(elapsed "$start") s -> $out/jscpd (log $out/jscpd.log)" >&2
        ra=${RUST_ANALYZER:-rust-analyzer}
        command -v "$ra" >/dev/null || die "rust-analyzer not found (set RUST_ANALYZER)"
        start=$(now)
        rc=0
        (cd "$repo" && capped "$ra" scip . --output "$out/index.scip.tmp" >"$out/index.scip.log" 2>&1) || rc=$?
        [ "$rc" -eq 0 ] || failed "rust-analyzer scip" "$rc" "$out/index.scip.log"
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
