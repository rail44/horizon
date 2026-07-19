#!/usr/bin/env bash
set -euo pipefail

# Headless verification for the GPUI shell -- the gui-verify successor of
# the retired Floem shell's check-terminal-visual.sh; see
# docs/gpui-migration-design.md's "GUI verification rebuild" section.
#
# The GPUI shell has no Xvfb-based path; it has built-in headless taps
# instead (env vars read once at session spawn, src/terminal/session.rs):
#
#   HORIZON_GPUI_DUMP=<path>    mirrors every terminal frame (plain text
#                               plus a per-line span/color table -- see
#                               src/terminal/mod.rs's dump_frame) to
#                               <path> on each update.
#   HORIZON_GPUI_DRIVE=<bytes>  typed as raw PTY input into the first
#                               session ~1.5s after startup.
#   HORIZON_GPUI_DRIVE_ENTER=1  sends the trailing Enter through the key
#                               encoder after HORIZON_GPUI_DRIVE, so a
#                               typed command line actually runs.
#
# This script drives those taps: HORIZON_GPUI_DRIVE is a one-line `printf`
# command that emits a marker string plus a 256-color (indexed) span and
# a truecolor span, waits for the dump to reflect it, and asserts the
# span table recorded both color kinds.

usage() {
  echo "usage: $0 [--binary <path>] [--out <dir>] [--force-kill]" >&2
}

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

binary="$repo_root/target/debug/horizon"
out=""
force_kill=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      binary="$2"
      shift 2
      ;;
    --out)
      out="$2"
      shift 2
      ;;
    --force-kill)
      force_kill=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ ! -x "$binary" ]]; then
  echo "binary not found or not executable: $binary" >&2
  echo "build it first: cargo build --workspace" >&2
  exit 1
fi

binary_name="$(basename "$binary")"
control_socket="/tmp/horizon-control-$(id -u).sock"

existing_pids="$(pgrep -x "$binary_name" 2>/dev/null || true)"
if [[ -n "$existing_pids" ]]; then
  if [[ "$force_kill" != "1" ]]; then
    echo "another $binary_name is already running (pid(s): $existing_pids)." >&2
    echo "pass --force-kill to kill it and remove $control_socket, or stop it yourself." >&2
    exit 1
  fi
  # shellcheck disable=SC2086 # word-splitting a pid list is intentional
  kill $existing_pids 2>/dev/null || true
  sleep 0.5
  rm -f "$control_socket"
fi

if [[ -z "$out" ]]; then
  out="$(mktemp -d "${TMPDIR:-/tmp}/horizon-gpui-check.XXXXXX")"
fi
mkdir -p "$out"
echo "out=$out"

dump="$out/dump.txt"
app_log="$out/app.log"
sessiond_socket="$out/sessiond.sock"
sessiond_binary="$(dirname "$binary")/horizon-sessiond"

marker="HORIZON_GPUI_CHECK_MARKER"
# A single line, typed verbatim as PTY input (this script never
# shell-escapes it further -- it becomes literal keystrokes at the pty's
# own shell prompt): prints the marker, then a 256-color (indexed) span
# and a truecolor span. printf's format string interprets \033 as the
# actual ESC byte, matching the escape-sequence style used elsewhere in
# this project's manual verification.
drive_cmd="printf '${marker}\\n\\033[38;5;208mINDEXED208\\033[0m\\n\\033[38;2;10;20;30mTRUECOLOR\\033[0m\\n'"

app_pid=""
cleanup() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  while read -r pid; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done < <(pgrep -f "^${sessiond_binary} --socket ${sessiond_socket}$" 2>/dev/null || true)
}
trap cleanup EXIT

HORIZON_GPUI_DUMP="$dump" \
  HORIZON_GPUI_DRIVE="$drive_cmd" \
  HORIZON_GPUI_DRIVE_ENTER=1 \
  HORIZON_SESSIOND_SOCKET="$sessiond_socket" \
  HORIZON_AGENT_EVENT_LOG="$out/events.jsonl" \
  HORIZON_AGENT_STATE_DB="$out/state.duckdb" \
  HORIZON_WORKSPACE_STATE="$out/workspace.json" \
  "$binary" >"$app_log" 2>&1 &
app_pid=$!

wait_secs=10
deadline=$((SECONDS + wait_secs))
marker_ok=0
indexed_ok=0
truecolor_ok=0
while ((SECONDS < deadline)); do
  if ! kill -0 "$app_pid" 2>/dev/null; then
    echo "app exited before the drive script produced output; see $app_log" >&2
    exit 1
  fi
  if [[ -s "$dump" ]]; then
    grep -qF -- "$marker" "$dump" && marker_ok=1 || marker_ok=0
    grep -qF -- "Indexed(208)" "$dump" && indexed_ok=1 || indexed_ok=0
    grep -qF -- "Rgb([" "$dump" && truecolor_ok=1 || truecolor_ok=0
    if [[ "$marker_ok" == "1" && "$indexed_ok" == "1" && "$truecolor_ok" == "1" ]]; then
      break
    fi
  fi
  sleep 0.2
done

fail=0
if [[ "$marker_ok" == "1" ]]; then
  echo "OK: marker present in dump"
else
  echo "FAIL: marker ($marker) not found in dump within ${wait_secs}s: $dump" >&2
  fail=1
fi
if [[ "$indexed_ok" == "1" ]]; then
  echo "OK: 256-color span present (Indexed(208))"
else
  echo "FAIL: 256-color span (Indexed(208)) not found in dump: $dump" >&2
  fail=1
fi
if [[ "$truecolor_ok" == "1" ]]; then
  echo "OK: truecolor span present (Rgb([...]))"
else
  echo "FAIL: truecolor span (Rgb([) not found in dump: $dump" >&2
  fail=1
fi

if [[ "$fail" != "0" ]]; then
  echo "dump preserved at: $dump" >&2
  exit 1
fi

echo "OK: dump=$dump"
