---
name: gui-verify
description: Use when verifying Horizon GUI changes end-to-end - launching the app headless, capturing screenshots and terminal model dumps, or running the terminal smoke suite. Trigger words: verify GUI, screenshot, smoke test, visual check, run the app.
---

# Verifying the Horizon GUI

**Hermetic by default (2026-07-06):** both scripts isolate agentd's
control socket, event log, and DuckDB state projection into scratch
paths under each run's own artifact dir (a scratch `XDG_RUNTIME_DIR`
that a fresh agentd binds under, plus `HORIZON_AGENT_EVENT_LOG`/
`HORIZON_AGENT_STATE_DB`) — a run never touches the owner's real
`horizon-agentd`, real event log, or real DuckDB state (backlog item
13), and `check-terminal-visual.sh`'s cleanup trap kills only the
scratch agentd it spawned (matched by its run-unique `--socket` path,
never a bare process-name kill). Set `HORIZON_REAL_RUNTIME=1` to opt
back into the real environment (e.g. to reproduce a bug that only
shows up against real session history).

**Two isolation caveats when opting out with `HORIZON_REAL_RUNTIME=1`
(2026-07-06):** (1) If the owner's desktop Horizon is running, headless
boots can stall before the window ever maps — the shared agentd accepts
one connection at a time and startup blocks on it (backlog item 14);
the default hermetic mode above already sidesteps this by giving each
run its own agentd, so this only bites in real-runtime mode. (2) The
scripts already `env -u WAYLAND_DISPLAY`; any *manual* launch outside
them must do the same, or winit picks the Wayland backend and ignores
your Xvfb `DISPLAY` entirely.

Horizon is a Floem desktop app. Two scripts launch it headless on an isolated
Xvfb display, drive it with xdotool, and capture text dumps plus a screenshot.

## Prerequisites

System dependencies: `Xvfb`, `xdotool`, `xwd`, ImageMagick (`magick` preferred,
falls back to `convert`). Optional: `python3` (color-grid scenario), `nvim`
(nvim-screen scenario) — the smoke suite skips those scenarios when missing.

Quick check:

```sh
command -v Xvfb xdotool xwd convert nvim python3
```

Run `cargo build` first. The scripts launch via `cargo run --quiet`, and the
window-wait budget (`HORIZON_WAIT_SECS`, default 20s) includes any compile time
— a cold build will blow the deadline and report a false failure.

## Quick visual check

```sh
scripts/check-terminal-visual.sh
```

On success it prints `OK` followed by the artifact directory
(default `/tmp/horizon-visual-<timestamp>-<pid>`, override with
`HORIZON_ARTIFACT_DIR`). Any failure exits 1 with a reason on stderr
(app exited early, window not found, empty dump, expectation mismatch,
missing/blank screenshot).

Artifacts:

- `terminal.txt` — the app's terminal model dumped as plain text (the app
  writes it because the script sets `HORIZON_TERMINAL_DUMP`). Best source for
  assertions.
- `screenshot.png` — the Horizon window captured via `xwd` and converted; Read
  it as an image to inspect visually. (`screenshot.xwd` is the raw capture.)
- `status.txt`, `clipboard.txt` — status line / clipboard dumps (only written
  when the app has something to report).
- `terminal-before-screenshot.txt` — dump snapshot taken just before capture.
- `horizon.log`, `xvfb.log` — app stdout/stderr and Xvfb log; check
  `horizon.log` first when the app dies early.
- `metadata.txt` — display, window id, screenshot dimensions, gray stddev,
  and (hermetic mode) `runtime_mode` plus the scratch
  `XDG_RUNTIME_DIR`/event-log/state-db paths used for that run.
- `xdg-runtime/`, `scratch-agent-events.jsonl`, `scratch-agent-state.duckdb`
  — hermetic mode's scratch state (only present unless `HORIZON_REAL_RUNTIME=1`);
  safe to ignore, or inspect `scratch-agent-events.jsonl` if a scenario's
  agent-adjacent behavior needs debugging.

Driving input (all optional env vars): `HORIZON_TEST_TEXT` (typed into the
terminal), `HORIZON_TEST_ENTER=1` (press Return), `HORIZON_TEST_SPLIT=1`
(split via workspace mode: `ctrl+'` then `:`), `HORIZON_TEST_XDOTOOL="<args>"`
(raw xdotool command chain). Assertions: `HORIZON_EXPECT_DUMP_CONTAINS`,
`HORIZON_EXPECT_STATUS_CONTAINS`, `HORIZON_EXPECT_CLIPBOARD_CONTAINS`
(fixed-string grep against the respective dump; mismatch exits 1).

Example:

```sh
HORIZON_TEST_TEXT="printf hello" HORIZON_TEST_ENTER=1 \
HORIZON_EXPECT_DUMP_CONTAINS="hello" scripts/check-terminal-visual.sh
```

## Smoke suite

```sh
scripts/run-terminal-smoke.sh
```

Runs check-terminal-visual.sh once per scenario, each on its own display
(`:99`, `:100`, ... from `HORIZON_SMOKE_DISPLAY_BASE`). Scenarios:

1. `basic-shell` — types `printf horizon-smoke-basic`, expects it in the dump.
2. `new-terminal-focus` — opens a second terminal tab via the command surface,
   expects typed text in the dump and `2 tab(s)` in status.txt.
3. `split-pane` — splits via the command surface, expects `2 pane(s)` in status.
4. `color-grid` — ANSI background colors via python3 (skipped if no python3).
5. `nvim-screen` — launches nvim with a 5s capture delay, expects `NVIM`
   (skipped if no nvim).

Artifacts land under `/tmp/horizon-terminal-smoke-<timestamp>-<pid>/<scenario>/`
(root overridable with `HORIZON_SMOKE_ARTIFACT_ROOT`); each scenario dir has the
full artifact set above. `index.txt` at the root maps scenario name to its dir
and records skips. On success the suite prints `OK` and the root dir. The suite
uses `set -e`: the first failing scenario aborts the whole run, so later
scenarios never execute and `index.txt` only lists completed ones.

## Reading results as an agent

- Prefer `terminal.txt` and `status.txt` for pass/fail reasoning — they are
  deterministic text. Better yet, encode assertions in
  `HORIZON_EXPECT_*_CONTAINS` so the script's exit code is the verdict.
- Read `screenshot.png` with the Read tool to visually confirm rendering
  (colors, layout, splits) that text dumps cannot show.
- Exit code 0 + `OK` on stdout = pass; anything else = fail, with the reason on
  stderr and details in `horizon.log`.

## Caveats

- Timing is sleep-based: 1s before capture by default. Slow-starting programs
  need `HORIZON_CAPTURE_DELAY` (the nvim scenario uses 5). Command-surface
  interactions rely on fixed 0.2–0.8s sleeps inside the xdotool chains.
- Xvfb listens on TCP (`-listen tcp -nolisten unix`); the client connects via
  `localhost:<n>`. Default display is `:99` (`HORIZON_TEST_DISPLAY`). A stale
  process holding the display makes startup fail — pick another display.
- A blank-screenshot guard rejects captures whose grayscale stddev is below
  `HORIZON_SCREENSHOT_STDDEV_MIN` (0.003), catching renders-nothing bugs.
- Cleanup kills the app, the scratch agentd it spawned (hermetic mode), and
  Xvfb on exit (trap), but artifact dirs in /tmp are never removed — clean
  them up yourself if they accumulate.
- The script forces `WGPU_BACKEND=gl` and `LIBGL_ALWAYS_SOFTWARE=1` (both
  overridable) and unsets `WAYLAND_DISPLAY` so rendering works headless.
- The window is resized to 1100x720 and clicked at (20, 90) to focus the
  terminal pane before any typing.
- `xdotool type` silently drops non-ASCII characters (CJK, emoji,
  box-drawing) in this headless setup rather than erroring. To verify such
  glyphs, write them to a UTF-8 file and drive `cat <file>` through
  `HORIZON_TEST_TEXT` instead of typing them directly.
- Floem's git pin has an accepted input-readiness regression: for ~0.3-0.5s
  after the window appears, all input is silently dropped (see
  `docs/trust-boundaries.md`, "floem" entry). `check-terminal-visual.sh`
  sleeps `HORIZON_INPUT_SETTLE` (default `0.7`) right after the window is
  found and before any xdotool input (moves, focus click, typed text) to
  compensate. `run-terminal-smoke.sh` inherits this since it drives the same
  script; raise it if a scenario still flakes, or set it to `0` once/if the
  regression is fixed upstream.
