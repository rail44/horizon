#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
binary="${1:-$repo_root/target/debug/horizon}"
sessiond_binary="$(dirname "$binary")/horizon-sessiond"
out="$(mktemp -d "${TMPDIR:-/tmp}/horizon-restore-check.XXXXXX")"
sessiond_socket="$out/sessiond.sock"
runtime_dir="$out/runtime"
control_socket="$runtime_dir/horizon/control.sock"
state_file="$out/workspace.json"
dump_file="$out/restored-frame.txt"
app_pid=""
host_runtime_dir="${XDG_RUNTIME_DIR:-}"
wayland_display="${WAYLAND_DISPLAY:-}"

if [[ ! -x "$binary" || ! -x "$sessiond_binary" ]]; then
  echo "build the workspace first: cargo build --workspace" >&2
  exit 1
fi

cleanup_app() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  app_pid=""
}

cleanup() {
  cleanup_app
  while read -r pid; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done < <(pgrep -f "^${sessiond_binary} --socket ${sessiond_socket}$" 2>/dev/null || true)
}
trap cleanup EXIT

start_app() {
  local log="$1"
  local dump="${2:-}"
  rm -f "$control_socket"
  mkdir -p "$runtime_dir"
  if [[ -n "$host_runtime_dir" && -n "$wayland_display" ]]; then
    ln -sf "$host_runtime_dir/$wayland_display" "$runtime_dir/$wayland_display"
  fi
  HORIZON_SESSIOND_SOCKET="$sessiond_socket" \
    XDG_RUNTIME_DIR="$runtime_dir" \
    HORIZON_WORKSPACE_STATE="$state_file" \
    HORIZON_AGENT_EVENT_LOG="$out/events.jsonl" \
    HORIZON_AGENT_STATE_DB="$out/state.duckdb" \
    HORIZON_GPUI_DUMP="$dump" \
    "$binary" >"$log" 2>&1 &
  app_pid=$!
  for _ in $(seq 1 100); do
    [[ -S "$control_socket" ]] && return
    kill -0 "$app_pid" 2>/dev/null || {
      echo "Horizon exited during startup; see $log" >&2
      exit 1
    }
    sleep 0.1
  done
  echo "timed out waiting for $control_socket; see $log" >&2
  exit 1
}

query() {
  "$binary" --socket "$control_socket" --json "$1"
}

start_app "$out/first.log"
[[ "$(query state | jq -r '.payload.tab_count')" == "1" ]]
"$binary" --socket "$control_socket" new-terminal --active >/dev/null
split_target="$(query sessions | jq -r '.payload.sessions[0].session_id')"
"$binary" --socket "$control_socket" new-terminal --split "$split_target" --active >/dev/null
first_state="$(query state)"
first_sessions="$(query sessions)"
[[ "$(jq -r '.payload.tab_count' <<<"$first_state")" == "2" ]]
[[ "$(jq -r '.payload.visible_pane_count' <<<"$first_state")" == "2" ]]
[[ "$(jq -r '.payload.sessions | length' <<<"$first_sessions")" == "3" ]]
first_ids="$(jq -r '.payload.sessions[].session_id' <<<"$first_sessions" | sort)"
[[ -s "$state_file" ]]

cleanup_app
start_app "$out/second.log" "$dump_file"
for _ in $(seq 1 100); do
  [[ -s "$dump_file" ]] && break
  kill -0 "$app_pid" 2>/dev/null || {
    echo "Horizon exited during restore; see $out/second.log" >&2
    exit 1
  }
  sleep 0.1
done
[[ -s "$dump_file" ]]

second_state="$(query state)"
second_sessions="$(query sessions)"
second_ids="$(jq -r '.payload.sessions[].session_id' <<<"$second_sessions" | sort)"
[[ "$(jq -r '.payload.tab_count' <<<"$second_state")" == "2" ]]
[[ "$(jq -r '.payload.visible_pane_count' <<<"$second_state")" == "2" ]]
[[ "$(jq -r '.payload.sessions | length' <<<"$second_sessions")" == "3" ]]
[[ "$second_ids" == "$first_ids" ]]

echo "OK: restored 2 tabs, a 2-pane split, and the same 3 terminal sessions"
echo "OK: state=$state_file"
echo "OK: restored frame=$dump_file"
