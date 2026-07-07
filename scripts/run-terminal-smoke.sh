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

# These scenarios enter workspace mode (`ctrl+'`, the shipped default
# from `docs/workspace-mode-design.md`) and open the palette with `:` --
# the mode-based replacement for the retired global `ctrl+shift+p` chord
# (`docs/tasks/backlog.md` item 1, resolved). Placement-first session
# creation (`docs/roadmap.md`) means all three now go through a two-step
# flow: the typed command (`new tab` / `split right` / `split down`) opens
# the palette's view chooser, and a second `Return` picks its first row
# (`Terminal`, always listed first -- see
# `control_surface::items::view_chooser_rows`). `split right`/`split down`
# must each type past "split " to disambiguate between the two verbs
# (`docs/recursive-layout-design.md`'s slice 3) -- typing just "split"
# would match both rows.
run_case new-terminal-focus \
  HORIZON_TEST_XDOTOOL="key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key n e w space t a b Return sleep 0.3 key Return sleep 0.8 type --clearmodifiers horizon-focus-ok" \
  HORIZON_EXPECT_DUMP_CONTAINS="horizon-focus-ok" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 tab(s)"

run_case split-right \
  HORIZON_TEST_XDOTOOL="key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key s p l i t space r i g h t Return sleep 0.3 key Return sleep 0.8" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 pane(s)"

run_case split-down \
  HORIZON_TEST_XDOTOOL="key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key s p l i t space d o w n Return sleep 0.3 key Return sleep 0.8" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 pane(s)"

# Regression guard for the stale-render bug fixed in 5fb0b97 (PositionShape):
# layout_position_view's dyn_container only re-fired on an is_split: bool
# memo, so a leaf-to-different-leaf transition at the tree root (two
# single-pane tabs both look like `is_split == false`) never re-invoked the
# builder closure -- the very first pane's widget stays mounted forever,
# regardless of how many tabs get created or activated afterward. A
# model-only assert (status bar, active-session dump) can't catch this:
# the workspace model itself is correct throughout (`ActivateTab`/`NewTab`
# always flip the *right* tab/pane active), only the rendered widget is
# stale. Verified empirically (temporarily reverting this file to its
# pre-fix `5fb0b97^` content) that the observable symptom is *not* "the
# new tab's keystrokes leak into the old tab" -- it's that a keystroke
# aimed at any pane whose widget was never actually built (because the
# frozen `dyn_container` never rendered it) reaches no terminal at all:
# `request_active_pane_focus` bumps that pane's own dedicated
# `focus_request` signal, but nothing is listening (its `pane_view` closure,
# with the `.request_focus` binding, was never constructed), so the
# keystrokes are simply dropped rather than misdirected. Switching back to
# the *original* pane (the one whose widget really is still mounted) then
# works fine regardless of the bug -- that pane's own `request_focus`
# binding is alive the whole time -- so a guard must never round-trip
# through that original pane; it has to move between two *newly created*
# tabs (neither is the one the `dyn_container` is frozen on) to actually
# exercise the failure.
#
# `HORIZON_TERMINAL_DUMP` is written by whichever terminal session last
# produced a Snapshot (src/app/runtime/terminal.rs), so it always reflects
# whichever terminal actually received the last keystroke (or, on the bug,
# whichever terminal was last touched *before* the dropped keystrokes --
# i.e. a stale marker that should have been superseded). Markers are typed
# via per-letter `key` chains (like the existing `n e w space t a b`
# chooser navigation above), not `xdotool type`: `type` swallows every
# remaining word in the xdotool chain as literal text (it doesn't stop at
# the next recognized verb the way `key`/`sleep` do), so it can only ever
# be the last command in a chain -- unusable here since more actions must
# follow each marker.
#
# HORIZON_INPUT_SETTLE=1.5 (above the script's 0.7 default): these two
# scenarios chain more actions (extra mode-entry/palette round trips) than
# the other scenarios here, so under real contention (many concurrent
# Xvfb/cargo runs) they're more exposed to the floem input-readiness
# window (docs/trust-boundaries.md's "floem" entry) than a single split/
# new-tab flow -- verified flaky at the 0.7 default and reliably green at
# 1.5 under a loaded machine.
run_case new-tab-no-stale-render \
  HORIZON_INPUT_SETTLE=1.5 \
  HORIZON_TEST_XDOTOOL="key m a r k o n e Return sleep 0.3 key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key n e w space t a b Return sleep 0.3 key Return sleep 0.8 key m a r k t w o Return sleep 0.3" \
  HORIZON_EXPECT_DUMP_CONTAINS="marktwo" \
  HORIZON_EXPECT_DUMP_NOT_CONTAINS="markone" \
  HORIZON_EXPECT_STATUS_CONTAINS="2 tab(s)"

# Same bug, exercising `ActivateTab` (the palette's "tab N" row --
# keyboard-only, more robust than a tab-strip click coordinate) rather than
# `NewTab`'s own creation-dive focus path: create tab 2 and tab 3 (neither
# is the original tab 1, per the "must not round-trip through the original
# pane" note above), then switch from tab 3 back to tab 2 and type a
# fourth marker there. `ActivateTab` doesn't itself exit workspace mode
# (only a *creating* operation does, per docs/workspace-mode-design.md's
# "creating operations dive; everything else restores"), so `Escape`
# cancels the mode afterward -- discarding its (now-stale, still
# tab-3-pointing) cursor without touching the tab `ActivateTab` already
# made active, unlike `Enter`/commit which would re-activate whatever the
# cursor points at.
run_case tab-switch-among-new-tabs \
  HORIZON_INPUT_SETTLE=1.5 \
  HORIZON_TEST_XDOTOOL="key m a r k o n e Return sleep 0.3 key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key n e w space t a b Return sleep 0.3 key Return sleep 0.8 key m a r k t w o Return sleep 0.3 key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key n e w space t a b Return sleep 0.3 key Return sleep 0.8 key m a r k t h r e e Return sleep 0.3 key ctrl+apostrophe sleep 0.7 key colon sleep 0.2 key t a b space 2 Return sleep 0.5 key Escape sleep 0.3 key m a r k f o u r Return sleep 0.3" \
  HORIZON_EXPECT_DUMP_CONTAINS="markfour" \
  HORIZON_EXPECT_DUMP_NOT_CONTAINS="markthree" \
  HORIZON_EXPECT_STATUS_CONTAINS="3 tab(s)"

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
