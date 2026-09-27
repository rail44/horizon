#!/usr/bin/env bash
# Isolated end-to-end check of the ACP line between the shell and
# horizon-agentd: an isolated Horizon instance on a local mock
# OpenAI-compatible provider runs one plain turn, one approval round trip
# through the shell, and one UI restart that resumes the agent session and
# runs a further turn. No network, no real provider.
#
# Usage: scripts/check-acp-agent-flow.sh [<horizon binary>]
# Needs: cargo build --workspace, python3, jq, curl.
set -uo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
binary="${1:-$repo_root/target/debug/horizon}"
agentd_binary="$(dirname "$binary")/horizon-agentd"
terminald_binary="$(dirname "$binary")/horizon-terminald"
mock_server="$repo_root/scripts/mock-openai-provider.py"
out="$(mktemp -d "${TMPDIR:-/tmp}/horizon-acp-flow-check.XXXXXX")"
agentd_socket="$out/agentd.sock"
terminald_socket="$out/terminald.sock"
runtime_dir="$out/runtime"
control_socket="$runtime_dir/horizon/control.sock"
state_file="$out/workspace.json"
events="$out/events.jsonl"
config_file="$out/config.toml"
mock_port="${HORIZON_ACP_CHECK_MOCK_PORT:-18081}"
app_pid=""
mock_pid=""
host_runtime_dir="${XDG_RUNTIME_DIR:-}"
wayland_display="${WAYLAND_DISPLAY:-}"

if [[ ! -x "$binary" || ! -x "$agentd_binary" || ! -x "$terminald_binary" ]]; then
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
  done < <(pgrep -f "^${agentd_binary} --socket ${agentd_socket}$" 2>/dev/null || true)
  while read -r pid; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done < <(pgrep -f "^${terminald_binary} --socket ${terminald_socket}$" 2>/dev/null || true)
  if [[ -n "$mock_pid" ]] && kill -0 "$mock_pid" 2>/dev/null; then kill "$mock_pid" 2>/dev/null || true; fi
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; echo "artifacts: $out" >&2; exit 1; }

cat >"$config_file" <<EOF
default_provider = "mock"
auxiliary_provider = "mock"

[[providers]]
name = "mock"
kind = "openai-compatible"
base_url = "http://127.0.0.1:$mock_port/v1"
api_key_env = "HORIZON_ACP_CHECK_MOCK_KEY"
default_model = "mock-model"
EOF

python3 "$mock_server" "$mock_port" "$out/mock.log" &
mock_pid=$!
for _ in $(seq 1 50); do
  curl -sf "http://127.0.0.1:$mock_port/v1/models" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -sf "http://127.0.0.1:$mock_port/v1/models" >/dev/null || fail "mock provider did not come up on port $mock_port"

start_app() {
  local log="$1"
  rm -f "$control_socket"
  mkdir -p "$runtime_dir"
  if [[ -n "$host_runtime_dir" && -n "$wayland_display" ]]; then
    ln -sf "$host_runtime_dir/$wayland_display" "$runtime_dir/$wayland_display"
  fi
  env -u OPENAI_BASE_URL -u HORIZON_RIG_MODEL -u OPENAI_API_KEY \
    HORIZON_CONFIG="$config_file" \
    HORIZON_ACP_CHECK_MOCK_KEY="mock" \
    HORIZON_AGENTD_SOCKET="$agentd_socket" \
    HORIZON_TERMINALD_SOCKET="$terminald_socket" \
    XDG_RUNTIME_DIR="$runtime_dir" \
    HORIZON_WORKSPACE_STATE="$state_file" \
    HORIZON_AGENT_EVENT_LOG="$events" \
    HORIZON_AGENT_STATE_DB="$out/state.duckdb" \
    "$binary" >"$log" 2>&1 &
  app_pid=$!
  for _ in $(seq 1 100); do
    [[ -S "$control_socket" ]] && return 0
    kill -0 "$app_pid" 2>/dev/null || fail "Horizon exited during startup; see $log"
    sleep 0.1
  done
  fail "timed out waiting for $control_socket; see $log"
}
cli() { "$binary" --socket "$control_socket" "$@"; }
query() { "$binary" --socket "$control_socket" --json "$1"; }
ev() { jq -c --arg sid "$1" --arg kind "$2" 'select(.session_id == $sid and .event_kind == $kind)' "$events" 2>/dev/null; }
count_ev() { ev "$1" "$2" | wc -l; }
wait_event() {
  local sid="$1" kind="$2" secs="$3"
  for _ in $(seq 1 $((secs * 2))); do
    [[ -s "$events" ]] && [[ -n "$(ev "$sid" "$kind" | head -1)" ]] && return 0
    sleep 0.5
  done
  fail "no $kind for $sid within ${secs}s"
}
wait_turn_end() { # wait_turn_end <sid> <count before> <seconds>
  local sid="$1" before="$2" secs="$3"
  for _ in $(seq 1 $((secs * 2))); do
    [[ "$(count_ev "$sid" turn_ended)" -gt "$before" ]] && return 0
    sleep 0.5
  done
  fail "turn did not end for $sid within ${secs}s"
}
wait_state() { # wait_state <field> <value> <seconds>
  local field="$1" value="$2" secs="$3"
  for _ in $(seq 1 $((secs * 4))); do
    [[ "$(query state | jq -r ".payload.$field")" == "$value" ]] && return 0
    sleep 0.25
  done
  fail "shell state $field never became $value"
}
assistant_text() {
  jq -r --arg sid "$1" 'select(.session_id == $sid and .event_kind == "message_committed") | .event.MessageCommitted | select(.role == "Assistant") | .text' "$events" | tail -1
}

# 1. a plain turn, shared (non-isolated) session in the active pane
start_app "$out/first.log"
spawn="$(cli --json new-agent --share --active --prompt 'Reply with exactly the word ACP-OK and nothing else.')"
sid="$(jq -r '.payload.session_id' <<<"$spawn")"
[[ -n "$sid" && "$sid" != "null" ]] || fail "new-agent returned no session id: $spawn"
wait_state has_turn_in_flight true 10
wait_turn_end "$sid" 0 60
[[ "$(assistant_text "$sid")" == "ACP-OK" ]] || fail "unexpected assistant text: $(assistant_text "$sid")"
wait_state has_turn_in_flight false 10
echo "OK: plain turn streamed and ended (session $sid)"

# 2. an approval round trip through the shell
cli send "$sid" 'Use the bash tool to run: echo acp-approval-check' >/dev/null
wait_event "$sid" approval_requested 60
approval="$(ev "$sid" approval_requested | tail -1)"
call_id="$(jq -r '.event.ApprovalRequested.call_id' <<<"$approval")"
occ_id="$(jq -r '.event.ApprovalRequested.occurrence_id' <<<"$approval")"
wait_state has_pending_approval true 10
before="$(count_ev "$sid" turn_ended)"
cli approve "$sid" "$call_id" "$occ_id" >/dev/null
wait_turn_end "$sid" "$before" 60
[[ "$(ev "$sid" approval_resolved | tail -1 | jq -r '.event.ApprovalResolved.decision')" == "Approve" ]] || fail "no Approve resolution recorded"
[[ "$(ev "$sid" tool_call_finished | tail -1 | jq -r '.event.ToolCallFinished.output.output')" == 'acp-approval-check' ]] || fail "bash output missing"
wait_state has_pending_approval false 10
echo "OK: approval requested, shown by the shell, approved over the CLI, tool ran"

# 3. restart the UI against the same agentd: resume, replay, and a further turn
cleanup_app
sleep 1
start_app "$out/second.log"
for _ in $(seq 1 40); do
  [[ "$(query sessions | jq -r --arg sid "$sid" '.payload.sessions[] | select(.session_id == $sid) | .attached')" == "true" ]] && break
  sleep 0.25
done
[[ "$(query sessions | jq -r --arg sid "$sid" '.payload.sessions[] | select(.session_id == $sid) | .attached')" == "true" ]] || fail "agent session not re-attached after restart"
[[ "$(query state | jq -r '.payload.tab_count')" == "2" ]] || fail "tab count after restart: $(query state | jq -r '.payload.tab_count')"
before="$(count_ev "$sid" turn_ended)"
# The workspace lists the session as attached as soon as it is restored; the
# runtime attachment (session/resume + replay) completes shortly after, and
# only then does the control plane accept commands for it.
sent=0
for _ in $(seq 1 80); do
  if cli send "$sid" 'Reply with exactly the word ACP-OK and nothing else.' >/dev/null 2>"$out/send.err"; then sent=1; break; fi
  sleep 0.25
done
[[ "$sent" == "1" ]] || fail "send after restart kept failing: $(cat "$out/send.err")"
wait_turn_end "$sid" "$before" 60
echo "OK: restarted UI resumed the session and ran a further turn"

if grep -iE 'panic|mismatch' "$out/first.log" "$out/second.log" >/dev/null; then
  grep -iE 'panic|mismatch' "$out/first.log" "$out/second.log" >&2
  fail "shell log reports a panic or a version mismatch"
fi

cli terminate-session "$sid" --yes >/dev/null 2>&1 || true
echo "OK: artifacts in $out"
