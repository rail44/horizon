# Spike: run-once components over signals, on blitz-dom

A trial of composing a UI foundation from small crates instead of GPUI:
`reactive_graph` for signals, `blitz-dom` for the document (Stylo for CSS,
Taffy for layout, Parley for text, and its own hit testing, focus and event
dispatch), `blitz-paint` and Vello for drawing. What is written here is the
layer between the first two (`src/lib.rs`, about 310 lines).

It is its own workspace: nothing here is part of Horizon's build or gate.
The findings are in `docs/research/ui-foundation-candidates-2026-11-11.md`.

## What is here

- `src/lib.rs` — the layer. A view function runs once and builds document
  nodes; a closure over signals becomes an effect that writes one text node,
  one attribute, or a keyed list's children.
- `tests/layer.rs` — without a window: a click through the document's own
  hit testing reaches a handler, a signal's write reaches one text node, a
  keyed list keeps the nodes of the items that stay.
- `src/bin/measure.rs` — one label changing among 100 to 10,000 rows, stage
  by stage.
- `src/bin/shot.rs` — draws a small view on the CPU into a PNG, with a
  hand-painted widget in it.
- `src/bin/window.rs` — the same layer in a window, for what a test cannot
  try: typing, the input method, hover, scrolling.

- `Cargo.toml` takes blitz-dom, blitz-paint and blitz-traits from the head
  of upstream's fix for hit testing through a clip (DioxusLabs/blitz#1324,
  58 commits past 0.3.0-beta.4 and not merged when this was written).
- `vendor/blitz-shell/` — blitz-shell from that same revision with two
  changes, each marked where it is. In `src/lib.rs` the input method is enabled with its
  cursor area, so that winit keeps the positions it is told afterwards. In
  `src/window.rs` the borders that layout rounded to nothing at a fractional
  display scale are given back after each resolve. `Cargo.toml` puts it in
  place of the published crate.
- `src/markdown.rs` — Horizon's Markdown view on this stack, using Horizon's
  own front matter reader and link resolver (`src/markdown/document.rs`);
  `tests/markdown.rs` holds it to what the GPUI view's tests hold that to,
  and `src/bin/markdown.rs` opens a file in a window.
- `src/agent.rs` — Horizon's agent pane on this stack, over Horizon's own
  display model and labels (`src/agent/view/{display,chrome}.rs`);
  `src/agent/sample.rs` is a made-up session to show in it, `tests/agent.rs`
  drives it with real input events, and `src/bin/agent.rs` shows it in a
  window with a turn streaming in, times an update against histories of
  several lengths, or draws one frame.
- `src/windowed.rs` — a long list in a scrolling box with only the items
  near what is in view built as elements; the agent pane's column uses it.
- `src/board.rs` — a list whose rows are reordered by dragging, the part of
  Horizon's board list (`src/board/list.rs`) that asks the most of the
  stack; `tests/board.rs` drives it with the mouse's own events.
- `src/workspace.rs` — the shell around the views: tabs, panes split across
  and down with a boundary to drag, workspace mode and a palette of
  commands, with the four views above in its panes; `tests/workspace.rs`
  drives it, and `src/bin/workspace.rs` shows it in a window or draws one
  frame.
- `src/term.rs` — a stand-in for a terminal's screen, drawn by hand, and the
  widget that puts it in a document.
- `src/bin/terminal.rs` — what that screen costs to shape and draw, alone
  and as a widget in a document, at three sizes and three scales.
- `src/bin/terminal_window.rs` — the same screen in a window, showing its
  own frame times: `-- scroll`, `-- flood` or `-- still`.
- `src/bin/adapter.rs` — which graphics adapter the renderers are given.
- `src/bin/observed.rs`, `src/bin/look.rs`, `src/bin/hit.rs` —
  reproductions, without a window, of what the window showed.

## Running

```sh
cargo test
cargo run --release --bin measure
cargo run --release --bin shot -- /tmp/shot.png
cargo run --release --bin window
```

Add `--features blitz-dom/log-phase-times` to `measure` for Blitz's own
timing of each stage of a resolve.
