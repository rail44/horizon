---
name: gui-verify
description: Use when verifying Horizon GUI changes end-to-end - launching the shell headless, capturing terminal frame dumps, or running the terminal check script. Trigger words: verify GUI, screenshot, smoke test, visual check, run the app.
---

# GUI Verification (GPUI shell)

Agents cannot see the GUI. The shell has built-in headless taps, read
once at session spawn (`src/terminal/session.rs`):

- `HORIZON_GPUI_DUMP=<path>` — mirrors every terminal frame to `<path>`
  on each update: the plain text, then a `--- spans ---` table with the
  cursor position and each span's text + logical fg/bg colors (see
  `dump_frame` in `src/terminal/mod.rs`). Last writer wins when several
  sessions share the path — the tap is per-process, so drive the pane
  you assert on.
- `HORIZON_GPUI_DRIVE=<bytes>` — typed as raw PTY input into the first
  session ~1.5s after startup. `HORIZON_GPUI_DRIVE_ENTER=1` sends the
  trailing Enter through the key encoder (exercises the core-side kitty
  path). This bypasses the real winit→gpui key/IME pipeline entirely
  (writes straight to the session's command channel), so it cannot
  verify that pipeline — use `HORIZON_INPUT_TRACE` for that.
- `HORIZON_INPUT_TRACE=1` (or a file path) — traces every hop of the
  real key/IME pipeline: winit's `KeyboardInput`/`Ime` arrival
  (`crates/horizon-winit-platform`), `TerminalView::handle_key`/
  `replace_text_in_range` entry and verdict (`src/terminal/mod.rs`), and
  the PTY-send decision. One `input-trace:`-prefixed line per event, key
  names and event kinds only — never the actual typed/composed text.
  Zero cost when unset.

One-shot check script:

```sh
scripts/check-gpui-terminal.sh [--binary <path>] [--out <dir>] [--force-kill]
```

Builds nothing itself — build first (`cargo build --workspace`). It
launches the binary with the taps, types a marker plus 256-color and
truecolor samples, polls the dump up to ~10s, and asserts marker,
`Indexed(208)`, and `Spec(Rgb` appear. It refuses to run while another
`horizon` process exists unless `--force-kill` is passed — never force
it when the owner may be running Horizon. Its workspace state is isolated
under the output directory.

Caveats:
- Pixel output is NOT verified — frame dumps prove the model/paint
  inputs, not the pixels. Real visual checks need a human (macOS screen
  capture requires Screen Recording permission the dev terminal usually
  lacks).
- The control socket is fixed per-uid (`/tmp/horizon-control-<uid>.sock`);
  a second instance logs a bind failure and runs without external control.
  Both one-shot scripts isolate sessiond, agent persistence, and workspace
  state under their output directory (mind macOS's ~104-byte `SUN_LEN`
  socket-path limit).
- Kill only processes your test started.
