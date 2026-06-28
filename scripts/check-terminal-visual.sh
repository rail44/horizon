#!/usr/bin/env bash
set -euo pipefail

server_display="${HORIZON_TEST_DISPLAY:-:99}"
display_number="${server_display#:}"
client_display="${HORIZON_CLIENT_DISPLAY:-localhost:${display_number}}"
artifact_dir="${HORIZON_ARTIFACT_DIR:-/tmp/horizon-visual-$(date +%Y%m%d-%H%M%S)-$$}"
window_name="${HORIZON_WINDOW_NAME:-Horizon}"
wait_secs="${HORIZON_WAIT_SECS:-20}"

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

cleanup() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
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

DISPLAY="$client_display" xdotool windowmove "$window_id" 0 0
DISPLAY="$client_display" xdotool windowsize "$window_id" 1100 720
DISPLAY="$client_display" xdotool windowfocus "$window_id" 2>/dev/null || true

if [[ "${HORIZON_TEST_SPLIT:-0}" == "1" ]]; then
  DISPLAY="$client_display" xdotool key ctrl+shift+p
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

sleep 1

if ! kill -0 "$app_pid" 2>/dev/null; then
  echo "Horizon exited before screenshot capture. See $app_log" >&2
  exit 1
fi

if [[ -f "$dump" ]]; then
  cp "$dump" "$pre_capture_dump"
fi

DISPLAY="$client_display" xwd -silent -id "$window_id" -out "$xwd_file"
convert "$xwd_file" "$png"

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

dimensions="$(identify -format '%wx%h' "$png")"
stddev="$(convert "$png" -colorspace Gray -format '%[fx:standard_deviation]' info:)"
echo "screenshot=$png" >> "$metadata"
echo "terminal_dump=$dump" >> "$metadata"
echo "clipboard_dump=$clipboard_dump" >> "$metadata"
echo "status_dump=$status_dump" >> "$metadata"
echo "pre_capture_dump=$pre_capture_dump" >> "$metadata"
echo "dimensions=$dimensions" >> "$metadata"
echo "gray_stddev=$stddev" >> "$metadata"

awk -v sd="$stddev" 'BEGIN { exit(sd > 0.003 ? 0 : 1) }' || {
  echo "Screenshot appears nearly blank. gray_stddev=$stddev, screenshot=$png" >&2
  exit 1
}

echo "OK"
echo "$artifact_dir"
