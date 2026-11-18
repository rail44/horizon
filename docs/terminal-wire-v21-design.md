# Terminal wire v21: who paces frames, and selection over history

Status: proposed 2026-11-18, not decided. Three changes to the terminal
wire (`crates/horizon-terminal-core`), made together so that the running
`horizon-terminald` is replaced once. Each is marked with what is the
owner's to decide.

What led here, all on 2026-11-18:

- With the daemon's frame interval set to 4 ms instead of 16
  (`HORIZON_TERMINAL_FRAME_INTERVAL_MS`), scrolling inside a full-screen
  program and dragging a selection were smoother to the owner's eye.
- Pressing in a view that is scrolled back throws the view to the bottom:
  a selection is of the live screen only.
- A survey of how others keep sessions alive
  (the session's research; not yet recorded under `docs/research/`):
  the three that send structured screens rather than bytes (WezTerm's
  mux, herdr, Zellij's native client since 0.46) all leave the pace of
  frames to something other than a fixed clock in the process that holds
  the PTY.

## A. The view paces the frames

### Today

`FramePublisher` (`session_loop/frames.rs`) sends a frame at once when
none was sent in the last 16 ms, and otherwise one at the end of that
window. The clock is the daemon's: it is not the display's, it holds
frames to about 60 a second whatever the display shows, and it makes
frames for a session no view is showing.

### Proposed

The view says when it has taken a frame, and the session makes the next
one only then.

- A new command, `TerminalCommand::FrameTaken`, with nothing in it.
- The session loop keeps two flags, `ready` (true at the start) and
  `dirty`.
  - The screen changes: if `ready`, snapshot and send, `ready = false`;
    otherwise `dirty = true`.
  - `FrameTaken` arrives: `ready = true`; if `dirty`, snapshot and send,
    `ready = false`, `dirty = false`.
- The view sends `FrameTaken` when it takes a frame in
  (`src/terminal/mod.rs`'s `pump`, run at every layout), and once when it
  is built. In the window a layout is run by a redraw, which the display
  paces (`vendor/blitz-shell/src/window.rs`: a wake asks for a redraw,
  the redraw lays out), so the frames come at the display's rate. A test
  that lays out in a loop gets them as fast as it asks.
- An attachment begins `ready`: `TerminalHost::install_subscriber` has
  the session take a `FrameTaken`, so a shell that went away between a
  frame and its `FrameTaken` does not leave the session waiting for one
  that will not come.
- `COALESCE_WINDOW` and `HORIZON_TERMINAL_FRAME_INTERVAL_MS` go.

What follows from it:

- An idle session's first change is a frame at once, as today.
- Under output that keeps coming, a frame is made for each one the view
  has drawn, and not otherwise: the snapshot's work is bounded by the
  display, not by a number.
- A session no view shows (its tab is not the one in front, or it is
  detached) makes no frames. Its title, directory, notifications and end
  are events, not frames, and are unchanged.
- A view built over a session that changed while unseen draws the frame
  the session last sent, which is old, then the new one a round trip
  later.
- The daemon reads the shell's directory 200 ms after the last frame
  (`CWD_SETTLE`). With no frames for an unseen session it would stop:
  the session loop tells the forwarder of a change whether or not it
  made a frame of it.

### To decide

1. **Whether a floor stays.** With no floor, a view that takes frames
   faster than a display would (a platform whose redraws are not paced)
   has the daemon snapshot as fast as the round trip. Recommended: no
   floor, and the daemon's CPU measured under `yes` before this lands;
   a floor of a millisecond or two if that shows the need.
2. **Whether the first frame after a pause waits for more.** foot waits
   0.5 ms for the rest of what a program writes before it draws, so that
   a redraw written in pieces is not shown half done
   (`delayed-render-lower`). Not proposed here: whether half-done frames
   are seen in Horizon has not been checked, and the wait is added to
   every key's echo.

## B. A selection over history

### Today

A selection's ends are cells of the live screen
(`TerminalSelectionPoint { row, col }`, `core.rs`'s `selection_point`).
A view that is scrolled back holds a window of history the daemon served
it and draws that (`horizon-shell-runtime`'s `terminal_scrollback`); a
press there drops the window first (`abandon`), and the view is at the
bottom again. The press and the moves are always the cell's left and
right edges, wherever in the cell the pointer is.

### What stands in the way of the simple form

Giving the ends a row that can be in history is not enough. A window is
a snapshot: output that arrives after it was served moves the history
under it, so a row counted from the window is the wrong row by however
many lines came since. `alacritty_terminal` keeps no count of the lines
that have gone into history (`grid/mod.rs`, `scroll_up`), so the daemon
cannot correct for it either.

### Proposed

The engine already keeps a scrolled view on the same text as output
arrives (`display_offset += positions` in `scroll_up`), and a selection
with it. A selection over history uses that.

- A press in a view that holds a window: the view asks the daemon to
  show history at the window's place (`TerminalCommand::ShowHistory {
  above }`, the rows above the bottom), then starts the selection with
  the cell on screen, as today. From there the view draws the live frame,
  which is now of history, and the daemon's selection, which is exact.
- `TerminalFrame` says how far above the bottom it is (`above`). A frame
  that is above the bottom is drawn as it is; the wheel then asks for a
  window at that place and has the daemon return to the bottom
  (`ShowHistory { above: 0 }`), and the view scrolls its window as today.
- `TerminalScrollWindow` carries the selection that lies in it, in the
  window's rows, read with the rows it was served with: a selection made
  and then scrolled past stays drawn.
- `SelectionStart` and `SelectionUpdate` say which half of the cell the
  point is in.

What it costs: the view's place snaps to a whole row at the press (a
window can be part of a row in; the daemon's view cannot), and where
output arrived between the window being served and the press, the text
shown moves by those lines at the press. The selection is of what is
shown after.

### To decide

3. **This form, or ends that name a row of history and are wrong by the
   lines that arrived.** Recommended: this form. The other is less code
   in the view and copies the wrong lines when output is arriving, with
   nothing on screen to say so.

## C. What is no longer used

- `TerminalCommand::Key`: the key event before `KeyInput`. Nothing in
  the shell, the CLI or the control plane sends it.
- `TerminalFrame.keys_as_escape_codes`: the view sends every key as
  `KeyInput` and does not read it.

Both go.

## Not in this change

- **Marks for the rows that changed.** The view finds a row it has
  shaped by comparing lines. Nothing measured says that costs enough to
  matter. It can be added later with a frame number.
- **A span's first column.** The view counts columns off the text's
  widths. No fault has been found in that.
- **One wheel notch as one report to a program**, where the view sends
  three. Not the wire, and the large jumps it was looked at for did not
  come back after the 2026-11-18 changes.
- **Keeping the PTYs when the daemon is replaced.** herdr and Paradev
  hand the PTYs' descriptors to the daemon that takes over. It would
  take the cost out of every change like this one; it is its own design.

## What changing the wire does

`TERMINAL_PROTOCOL_VERSION` goes from 20 to 21. A shell at 21 drains a
daemon at 20 when it connects, and every terminal session ends
(`docs/terminald-split-design.md`). The spike under
`spikes/ui-foundation/` runs the terminal core itself and is not kept
building against it.
