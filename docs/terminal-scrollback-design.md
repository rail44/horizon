# Terminal Scrollback

Status: designed 2026-11-22; being implemented on the branch
`worktree-terminal-row-texture-design`. It replaces the windowed design
that came before it (in git history), whose windows were sized and fetched
for a daemon round trip that no longer happens.

Scrolling back through a terminal's history moves the viewport of the
view's own terminal (the session's mirror,
`docs/terminal-stream-design.md`). The view keeps only the part of a row
it is into. Each frame reads only the rows that came into view.

## 1. What prompted this

A pane 400 columns wide and 120 rows tall cost 9–11 ms of input handling
per wheel tick in a debug build (`cargo bench --bench scenarios --
terminal/scrollback`). Every tick fetched a new window of history:

- **Sizing.** A window's rows were bounded by 4 MiB at a worst-case 128
  bytes a cell. That was the item cap of the daemon's events channel when a
  window crossed it. 400 columns fit only 81 rows, less than the screen, so
  the window had no overscan and any movement left it.
- **Fetching.** Since 2026-11-19 a window is cut out of the view's own
  terminal, synchronously on the UI thread (`Session::window` →
  `TerminalCore::snapshot_window`): one screen of cells copied per tick.
- **What was left over.** The request, the reply, the prefetch and the
  byte bound existed for the round trip. None of it carries anything any
  more.

## 2. What the engine offers

libghostty-vt, observed in
`engine_observations.rs::what_the_engine_says_of_rows_as_the_viewport_scrolls`:

- **Moving the viewport** (`Terminal::scroll_viewport`) marks the render
  state `Full` and every row changed. The rows still in view keep their
  ids, moved by exactly the rows the viewport moved.
- **Output while the viewport is back in the history** leaves it on the
  same rows. The scrollbar's offset grows with the history, and no row is
  marked changed.
- **Overscan.** `RenderState::set_overscan` captures rows above or below
  the viewport. There is none below the live bottom.

## 3. Design

### 3.1 Where the view is in the history

The view's place is `back`: how many rows above the live bottom its top
is, continuous.

- **The engine's viewport** is `ceil(back)` rows back. The view keeps only
  `into = ceil(back) − back`, the part of a row the view is into the
  viewport's top row. `0 ≤ into < 1`.
- **Each frame reads the engine's offset.** Output while scrolled back
  moves the engine's offset up with the history, and the view stays on the
  same rows without doing anything.
- **A wheel tick** turns its distance into rows and adds them to `back`.
  `back` is clamped to `0` and to the history's length. The engine is asked
  to put its viewport `ceil(back)` rows back (`TerminalCore::scroll_back_to`,
  which answers with the offset it reached) and `into` is set from it.
- **At the top of the history**, `into` is `0`.
- **At the bottom**, `back` is `0`: the live screen, its cursor drawn.

### 3.2 The frame

`TerminalFrame` carries:

- **`back`**: the engine's offset, in rows above the live bottom.
- **`below`**: while the viewport is back in the history, the row under its
  last, captured as one row of overscan below. A view `into` a row shows
  part of that row at the bottom.

The view draws the frame's rows and `below` shifted up by `into` rows, in
whole display pixels (`src/terminal/rows.rs`'s `scroll_shift`), clipped to
the grid's box. The cursor and the input method's preedit are drawn only at
`back = 0`.

### 3.3 Reading only what came into view

`render::drawn` keeps the rows it read, by the engine's row id.

- **Moved, not written.** When nothing was written to the engine since the
  last frame, a `Full` state comes from the viewport moving. A row whose id
  was read before is then the row read before, and is taken as it was.
  Only rows new to the view are read.
- **What counts as written.** The core marks the rows it holds as possibly
  stale whenever the engine's content can change: bytes taken in
  (`write_vt`), a resize, the colors set. After that the engine's own
  dirty flags decide, as before.

This rule is the core's, not the engine's: libghostty-vt's contract says a
row marked changed is to be read again. It holds because the core knows
every way it writes to the engine.

### 3.4 Selection, links and the pointer

- **Selection.** It works in the history as on the live screen. It starts
  at a viewport cell and is tracked by the engine across scrolling
  (`TerminalCore::start_selection`). Starting one no longer leaves the
  history.
- **The pointer's row** is `floor(y / row + into)`, for selecting and for
  following a link.

### 3.5 When the history is not the view's to scroll

When the program owns the screen (the alternate screen, or a program that
asked for the mouse), the frame says `scrollback_available = false`. The
view then puts the engine's viewport back at the bottom, and the wheel is
the program's, as before.

### 3.6 One session, one pane

A session's views share one terminal, so they would share its viewport.
`Workspace::attach_existing_session_to_split_activated` puts a session in a
new pane only when no pane shows it. For one already shown it answers with
that pane, and shows it when asked to activate. `horizon attach` goes
through it.

## 4. What goes

- `horizon-shell-runtime`'s `terminal_scrollback` (the windowed state
  machine and its tests).
- `TerminalScrollWindow`, `TerminalCore::snapshot_window`,
  `render::snapshot_window`, `max_window_rows` and its byte bound.
  None of them is on a wire.

## 5. Tests

- **Core.**
  - A frame after the viewport moved, reading only the rows new to it, is
    the frame built from nothing.
  - Output while scrolled back leaves the rows in view as they were.
  - `below` is the row under the viewport.
  - `scroll_back_to` clamps at the top and at the bottom.
- **View** (`tests/terminal.rs`, over a real shell).
  - The wheel moves the history by its distance, part of a row too.
  - Output while scrolled back leaves the view on the same rows.
  - A program taking the screen puts the view back at the bottom.
  - Selecting in the history selects the rows shown.
  - The kept-rows pixel test of history scrolled into a row
    (`docs/terminal-row-texture-design.md` §6).
- **Workspace.** Attaching a session a pane shows adds no pane.
- **Measuring.** `terminal/scrollback` at 48000 cells, before and after.

### Measured 2026-11-22

Debug build, the median in milliseconds:

- **`cargo bench --bench scenarios -- terminal/scrollback`** at 48000
  cells, no window: input handling (`act`) 7.8 → 0.03; the whole frame
  9.3 → 1.7. At 1920 and 12000 cells both were under a millisecond before
  and after.
- **A frame after moving the viewport three rows** (`engine_observations.rs`'s
  `how_long_a_frame_takes_as_the_viewport_scrolls`):
  - 400×120: 9.0 → 1.3, and in release 3.6 → 0.25;
  - 200×60: 2.6 → 0.46, and in release 0.95 → 0.10.
- **In a window** (`--window`, with the owner's consent), three binaries in
  turn, twice each: before the row store, the row store with the windowed
  scrollback, and this design. `terminal/scrollback` at 48000 cells:

  | | the whole frame | `render` | late of 200 |
  | --- | --- | --- | --- |
  | before the row store | 11.3–14.5 | 3.7–4.2 | 1–50 |
  | the row store, windowed scrollback | 14.2–16.5 | 6.3–6.6 | 35–99 |
  | this design | 7.4–7.8 | 6.7–7.3 | 0 |

  At 12000 cells the whole frame was 2.3–2.4, 2.5–2.6 and 3.4–3.7, never
  late. Typing, idle frames and lines of output came out as before this
  design.

**Open.** `render` while scrolling back was about 3 ms over what it was
before the row store at 48000 cells, and about 1 ms at 12000. That cost
was in painting from the row store, not in scrolling. Since 2026-11-23,
two things have cut a wheel step's `render` at 48000 cells from about
6.5 ms to 5.7–5.9:

- rows shaped before are found wherever scrolling back moved them;
- the rows new to a frame are painted into it.

What is left is filling from the store image
(`docs/terminal-row-texture-design.md` §7, "Measured 2026-11-23").

## References

- `docs/terminal-stream-design.md` — the daemon sends bytes and the view
  has a terminal of its own.
- `docs/terminal-row-texture-design.md` — the rows painted from the row
  store, keyed by content.
- `crates/horizon-terminal-core/src/core/engine_observations.rs` — what the
  engine was seen to do.
