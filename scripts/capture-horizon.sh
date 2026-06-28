#!/usr/bin/env bash
set -euo pipefail

out="${1:-/tmp/horizon.png}"
mode="${2:-window}"
delay="${HORIZON_CAPTURE_DELAY:-2}"

case "$mode" in
  window)
    echo "Focus the Horizon window within ${delay}s..."
    sleep "$delay"
    gnome-screenshot -w -f "$out"
    ;;
  area)
    echo "Select the Horizon area..."
    gnome-screenshot -a -f "$out"
    ;;
  screen)
    gnome-screenshot -f "$out"
    ;;
  *)
    echo "usage: $0 [output.png] [window|area|screen]" >&2
    exit 2
    ;;
esac

echo "$out"
