#!/usr/bin/env bash
set -euo pipefail

server_display="${HORIZON_TEST_DISPLAY:-:99}"
display_number="${server_display#:}"
client_display="${HORIZON_CLIENT_DISPLAY:-localhost:${display_number}}"
artifact_dir="${HORIZON_ARTIFACT_DIR:-/tmp/horizon-visual-$(date +%Y%m%d-%H%M%S)-$$}"
window_name="${HORIZON_WINDOW_NAME:-Horizon}"
wait_secs="${HORIZON_WAIT_SECS:-20}"
capture_delay="${HORIZON_CAPTURE_DELAY:-1}"
stddev_min="${HORIZON_SCREENSHOT_STDDEV_MIN:-0.003}"

mkdir -p "$artifact_dir"

dump="$artifact_dir/terminal.txt"
clipboard_dump="$artifact_dir/clipboard.txt"
status_dump="$artifact_dir/status.txt"
pre_capture_dump="$artifact_dir/terminal-before-screenshot.txt"
png="$artifact_dir/screenshot.png"
xwd_file="$artifact_dir/screenshot.xwd"
app_log="$artifact_dir/horizon.log"
xvfb_log="$artifact_dir/xvfb.log"
metadata="$artifact_dir/metadata.txt"

xvfb_pid=""
app_pid=""
# Set below only in hermetic mode (the default); left empty in
# HORIZON_REAL_RUNTIME=1 mode so `cleanup`'s scratch-agentd search is a
# guaranteed no-op instead of matching the owner's real agentd.
scratch_runtime_dir=""

cleanup() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi

  # Hermetic runs (the default, see below) give Horizon its own scratch
  # XDG_RUNTIME_DIR, which makes agentd bind a scratch socket at
  # "$scratch_runtime_dir/horizon/agentd.sock" (horizon_agent::socket::
  # default_socket_path) instead of the owner's real one. Horizon exiting
  # does NOT kill agentd -- sessions are designed to survive pane/app
  # restarts -- so without this, every hermetic run would leak one
  # horizon-agentd process. We find *only this run's* agentd by grepping
  # for that scratch socket path in the process command line: agentd is
  # always spawned as `horizon-agentd --socket <that exact path>` (see
  # src/agent/agentd_client.rs's agentd_command), so the path is a literal
  # argv entry, not just an inherited env var. That path lives under this
  # run's artifact_dir (itself timestamp+pid-qualified), so it cannot
  # collide with any other run's agentd, past or concurrent -- this is a
  # match on a run-unique identifier, not an indiscriminate name/pattern
  # kill of "any horizon-agentd".
  if [[ -n "$scratch_runtime_dir" ]]; then
    scratch_agentd_pids="$(pgrep -f -- "${scratch_runtime_dir}/horizon/agentd.sock" 2>/dev/null || true)"
    if [[ -n "$scratch_agentd_pids" ]]; then
      # shellcheck disable=SC2086 # word-splitting a pid list is intentional
      kill $scratch_agentd_pids 2>/dev/null || true
    fi
  fi

  if [[ -n "$xvfb_pid" ]] && kill -0 "$xvfb_pid" 2>/dev/null; then
    kill "$xvfb_pid" 2>/dev/null || true
    wait "$xvfb_pid" 2>/dev/null || true
  fi
}

trap cleanup EXIT

echo "artifact_dir=$artifact_dir" > "$metadata"
echo "server_display=$server_display" >> "$metadata"
echo "client_display=$client_display" >> "$metadata"

# Hermetic by default (docs/tasks/backlog.md item 13): isolate Horizon's
# agentd (event log, DuckDB state projection, and the agentd control
# socket itself) into scratch paths under this run's artifact_dir so a
# verification run never reads from or writes into the owner's real
# ~/.local/share/horizon state or steals the owner's real agentd
# connection slot (agentd accepts one connection at a time). Set
# HORIZON_REAL_RUNTIME=1 to opt back into the real environment (e.g. to
# reproduce a bug that only shows up against real session history).
real_runtime="${HORIZON_REAL_RUNTIME:-0}"
runtime_env_args=()
if [[ "$real_runtime" != "1" ]]; then
  scratch_runtime_dir="$artifact_dir/xdg-runtime"
  mkdir -p "$scratch_runtime_dir"
  chmod 700 "$scratch_runtime_dir"
  scratch_event_log="$artifact_dir/scratch-agent-events.jsonl"
  scratch_state_db="$artifact_dir/scratch-agent-state.duckdb"
  runtime_env_args=(
    "XDG_RUNTIME_DIR=$scratch_runtime_dir"
    "HORIZON_AGENT_EVENT_LOG=$scratch_event_log"
    "HORIZON_AGENT_STATE_DB=$scratch_state_db"
  )
  echo "runtime_mode=hermetic" >> "$metadata"
  echo "scratch_xdg_runtime_dir=$scratch_runtime_dir" >> "$metadata"
  echo "scratch_agent_event_log=$scratch_event_log" >> "$metadata"
  echo "scratch_agent_state_db=$scratch_state_db" >> "$metadata"
else
  echo "runtime_mode=real (HORIZON_REAL_RUNTIME=1)" >> "$metadata"
fi

Xvfb "$server_display" -screen 0 1280x800x24 -listen tcp -nolisten unix >"$xvfb_log" 2>&1 &
xvfb_pid="$!"

for _ in $(seq 1 50); do
  if DISPLAY="$client_display" xdpyinfo >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

if ! DISPLAY="$client_display" xdpyinfo >/dev/null 2>&1; then
  echo "Xvfb did not become ready on $server_display via $client_display" >&2
  exit 1
fi

env -u WAYLAND_DISPLAY \
  DISPLAY="$client_display" \
  HORIZON_TERMINAL_DUMP="$dump" \
  HORIZON_CLIPBOARD_DUMP="$clipboard_dump" \
  HORIZON_STATUS_DUMP="$status_dump" \
  WGPU_BACKEND="${WGPU_BACKEND:-gl}" \
  LIBGL_ALWAYS_SOFTWARE="${LIBGL_ALWAYS_SOFTWARE:-1}" \
  "${runtime_env_args[@]}" \
  cargo run --quiet >"$app_log" 2>&1 &
app_pid="$!"

window_id=""
deadline=$((SECONDS + wait_secs))
while (( SECONDS < deadline )); do
  if ! kill -0 "$app_pid" 2>/dev/null; then
    echo "Horizon exited before creating a window. See $app_log" >&2
    exit 1
  fi

  window_id="$(DISPLAY="$client_display" xdotool search --name "$window_name" 2>/dev/null | head -n 1 || true)"
  if [[ -n "$window_id" ]]; then
    break
  fi
  sleep 0.2
done

if [[ -z "$window_id" ]]; then
  echo "Could not find a '$window_name' window within ${wait_secs}s. See $app_log" >&2
  exit 1
fi

echo "window_id=$window_id" >> "$metadata"

# Accepted upstream input-readiness regression (see docs/trust-boundaries.md,
# "floem" entry): for ~0.3-0.5s after the window appears, the Lapce git pin
# silently drops all input. Settle before sending any xdotool input
# (window moves/resizes, focus click, typed text) so interactions are
# reliable. Override with HORIZON_INPUT_SETTLE; set to 0 to disable.
input_settle="${HORIZON_INPUT_SETTLE:-0.7}"
sleep "$input_settle"

DISPLAY="$client_display" xdotool windowmove "$window_id" 0 0
DISPLAY="$client_display" xdotool windowsize "$window_id" 1100 720
DISPLAY="$client_display" xdotool windowfocus "$window_id" 2>/dev/null || true

# Enters workspace mode (`docs/workspace-mode-design.md`'s `ctrl+'`
# default) and opens the palette with `:` -- the mode-based replacement
# for the retired global `ctrl+shift+p` chord (`docs/tasks/backlog.md`
# item 1, resolved). xdotool's X11 keysym name for the apostrophe key is
# "apostrophe"; "colon" is a named keysym independent of physical layout.
if [[ "${HORIZON_TEST_SPLIT:-0}" == "1" ]]; then
  DISPLAY="$client_display" xdotool key ctrl+apostrophe
  sleep "$input_settle"
  DISPLAY="$client_display" xdotool key colon
  sleep 0.2
  DISPLAY="$client_display" xdotool key s p l i t Return
  sleep 0.2
fi

DISPLAY="$client_display" xdotool mousemove --window "$window_id" 20 90 click 1 2>/dev/null || true

if [[ -n "${HORIZON_TEST_TEXT:-}" ]]; then
  DISPLAY="$client_display" xdotool type --clearmodifiers --delay 5 "$HORIZON_TEST_TEXT"
fi

if [[ "${HORIZON_TEST_ENTER:-0}" == "1" ]]; then
  DISPLAY="$client_display" xdotool key Return
fi

if [[ -n "${HORIZON_TEST_XDOTOOL:-}" ]]; then
  # shellcheck disable=SC2086
  DISPLAY="$client_display" xdotool ${HORIZON_TEST_XDOTOOL}
fi

sleep "$capture_delay"

if ! kill -0 "$app_pid" 2>/dev/null; then
  echo "Horizon exited before screenshot capture. See $app_log" >&2
  exit 1
fi

if [[ -f "$dump" ]]; then
  cp "$dump" "$pre_capture_dump"
fi

DISPLAY="$client_display" xwd -silent -id "$window_id" -out "$xwd_file"
if command -v magick >/dev/null 2>&1; then
  magick "$xwd_file" "$png"
else
  convert "$xwd_file" "$png"
fi

if [[ ! -s "$dump" ]]; then
  echo "Terminal dump is empty or missing: $dump" >&2
  exit 1
fi

if [[ -n "${HORIZON_EXPECT_DUMP_CONTAINS:-}" ]]; then
  if ! grep -F -- "${HORIZON_EXPECT_DUMP_CONTAINS}" "$dump" >/dev/null; then
    echo "Terminal dump does not contain expected text: ${HORIZON_EXPECT_DUMP_CONTAINS}" >&2
    echo "Dump path: $dump" >&2
    exit 1
  fi
fi

# The negative counterpart: some regressions (e.g. stale-render input
# misrouting) are only observable as unwanted content leaking into the
# dump, not as expected content being absent -- see the
# tab-switch/new-tab smoke scenarios in run-terminal-smoke.sh, which rely
# on this to catch input landing in the wrong (stale) terminal.
if [[ -n "${HORIZON_EXPECT_DUMP_NOT_CONTAINS:-}" ]]; then
  if grep -F -- "${HORIZON_EXPECT_DUMP_NOT_CONTAINS}" "$dump" >/dev/null; then
    echo "Terminal dump unexpectedly contains: ${HORIZON_EXPECT_DUMP_NOT_CONTAINS}" >&2
    echo "Dump path: $dump" >&2
    exit 1
  fi
fi

if [[ -n "${HORIZON_EXPECT_CLIPBOARD_CONTAINS:-}" ]]; then
  if [[ ! -s "$clipboard_dump" ]]; then
    echo "Clipboard dump is empty or missing: $clipboard_dump" >&2
    exit 1
  fi
  if ! grep -F -- "${HORIZON_EXPECT_CLIPBOARD_CONTAINS}" "$clipboard_dump" >/dev/null; then
    echo "Clipboard dump does not contain expected text: ${HORIZON_EXPECT_CLIPBOARD_CONTAINS}" >&2
    echo "Clipboard path: $clipboard_dump" >&2
    exit 1
  fi
fi

if [[ -n "${HORIZON_EXPECT_STATUS_CONTAINS:-}" ]]; then
  if [[ ! -s "$status_dump" ]]; then
    echo "Status dump is empty or missing: $status_dump" >&2
    exit 1
  fi
  if ! grep -F -- "${HORIZON_EXPECT_STATUS_CONTAINS}" "$status_dump" >/dev/null; then
    echo "Status dump does not contain expected text: ${HORIZON_EXPECT_STATUS_CONTAINS}" >&2
    echo "Status path: $status_dump" >&2
    exit 1
  fi
fi

if [[ ! -s "$png" ]]; then
  echo "Screenshot is empty or missing: $png" >&2
  exit 1
fi

if command -v magick >/dev/null 2>&1; then
  dimensions="$(magick identify -format '%wx%h' "$png")"
  stddev="$(magick "$png" -colorspace Gray -format '%[fx:standard_deviation]' info:)"
else
  dimensions="$(identify -format '%wx%h' "$png")"
  stddev="$(convert "$png" -colorspace Gray -format '%[fx:standard_deviation]' info:)"
fi
echo "screenshot=$png" >> "$metadata"
echo "terminal_dump=$dump" >> "$metadata"
echo "clipboard_dump=$clipboard_dump" >> "$metadata"
echo "status_dump=$status_dump" >> "$metadata"
echo "pre_capture_dump=$pre_capture_dump" >> "$metadata"
echo "dimensions=$dimensions" >> "$metadata"
echo "gray_stddev=$stddev" >> "$metadata"

awk -v sd="$stddev" -v min="$stddev_min" 'BEGIN { exit(sd > min ? 0 : 1) }' || {
  echo "Screenshot appears nearly blank. gray_stddev=$stddev, min=$stddev_min, screenshot=$png" >&2
  exit 1
}

echo "OK"
echo "$artifact_dir"
