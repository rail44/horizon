#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
check_script="$root_dir/scripts/check-terminal-visual.sh"
artifact_root="${HORIZON_SMOKE_ARTIFACT_ROOT:-/tmp/horizon-terminal-smoke-$(date +%Y%m%d-%H%M%S)-$$}"
base_display="${HORIZON_SMOKE_DISPLAY_BASE:-99}"

# No hermetic-mode wiring needed here: each scenario below gets its own
# HORIZON_ARTIFACT_DIR (see run_case), and check-terminal-visual.sh derives
# its scratch XDG_RUNTIME_DIR/event-log/state-db from that per-scenario dir
# by default (docs/tasks/backlog.md item 13), so every scenario already
# gets an isolated agentd. Export HORIZON_REAL_RUNTIME=1 before invoking
# this script to opt every scenario back into the real environment; `env`
# below only overrides HORIZON_ARTIFACT_DIR/HORIZON_TEST_DISPLAY, so an
# already-exported HORIZON_REAL_RUNTIME passes through untouched.

mkdir -p "$artifact_root"

run_case() {
  local name="$1"
  shift

  local display_number=$((base_display + case_count))
  local artifact_dir="$artifact_root/$name"
  mkdir -p "$artifact_dir"

  echo "==> $name"
  env \
    HORIZON_ARTIFACT_DIR="$artifact_dir" \
    HORIZON_TEST_DISPLAY=":$display_number" \
    "$@" \
    "$check_script"
  echo "$name $artifact_dir" >> "$artifact_root/index.txt"
  case_count=$((case_count + 1))
}

case_count=0
: > "$artifact_root/index.txt"

run_case basic-shell \
  HORIZON_TEST_TEXT="printf horizon-smoke-basic" \
  HORIZON_TEST_ENTER=1 \
  HORIZON_EXPECT_DUMP_CONTAINS="horizon-smoke-basic"

# Both scenarios below enter workspace mode (`ctrl+'`, the shipped default
# from `docs/workspace-mode-design.md`) and open the palette with `:` --
# the mode-based replacement for the retired global `ctrl+shift+p` chord
# (`docs/tasks/backlog.md` item 1, resolved).
run_case new-terminal-focus \
  HORIZON_TEST_XDOTOOL="key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key n e w space t e r m i n a l Return sleep 0.8 type --clearmodifiers horizon-focus-ok" \
  HORIZON_EXPECT_DUMP_CONTAINS="horizon-focus-ok" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 tab(s)"

run_case split-pane \
  HORIZON_TEST_XDOTOOL="key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key s p l i t Return sleep 0.8" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 pane(s)"

if command -v python3 >/dev/null 2>&1; then
  run_case color-grid \
    HORIZON_TEST_TEXT="python3 -c \"print('\\033[41m RED \\033[42m GREEN \\033[44m BLUE \\033[0m done')\"" \
    HORIZON_TEST_ENTER=1 \
    HORIZON_EXPECT_DUMP_CONTAINS="RED  GREEN  BLUE  done"
else
  echo "==> color-grid skipped: python3 not found"
  echo "color-grid skipped" >> "$artifact_root/index.txt"
fi

if command -v nvim >/dev/null 2>&1; then
  run_case nvim-screen \
    HORIZON_TEST_TEXT="nvim" \
    HORIZON_TEST_ENTER=1 \
    HORIZON_CAPTURE_DELAY=5 \
    HORIZON_EXPECT_DUMP_CONTAINS="NVIM"
else
  echo "==> nvim-screen skipped: nvim not found"
  echo "nvim-screen skipped" >> "$artifact_root/index.txt"
fi

echo "OK"
echo "$artifact_root"
