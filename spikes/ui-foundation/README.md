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

- `vendor/blitz-shell/` — blitz-shell 0.3.0-beta.4 with two changes, each
  marked where it is. In `src/lib.rs` the input method is enabled with its
  cursor area, so that winit keeps the positions it is told afterwards. In
  `src/window.rs` the borders that layout rounded to nothing at a fractional
  display scale are given back after each resolve. `Cargo.toml` puts it in
  place of the published crate.
- `src/bin/observed.rs`, `src/bin/look.rs` — reproductions, without a
  window, of what the window showed.

## Running

```sh
cargo test
cargo run --release --bin measure
cargo run --release --bin shot -- /tmp/shot.png
cargo run --release --bin window
```

Add `--features blitz-dom/log-phase-times` to `measure` for Blitz's own
timing of each stage of a resolve.
