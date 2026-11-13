---
name: preview-view
description: Use when implementing or changing a view in Horizon and you want it shown from the preview plugin - registering a named preview with sample data, building the plugin, checking it windowlessly, and showing it to the owner in a pane of the running Horizon. Trigger words: preview a view, preview pane, show the view to the owner, view feedback, horizon preview, make a view previewable.
---

# Previewing a view under development

A view can be built into a WebAssembly plugin (`preview-plugin/`) and shown
from it in a pane of the running Horizon (`src/preview.rs`,
`docs/preview-pane-design.md`): the plugin has the view's signals, effects
and handlers, and the document is the shell's.

## Making a view previewable

1. The view's module has to build for `wasm32-wasip2`. In `src/lib.rs`,
   leave it out of the `#[cfg(not(target_family = "wasm"))]` gate, and keep
   what it needs of the host (a thread, a socket, a file, the daemon
   clients) behind that gate inside it. `src/board/` is the example: the
   store is handed in whole (`Source::Ready`), and following the log is
   native-only. `Background::run` runs its work in place in a plugin.
2. A view in a plugin uses `horizon_ui` as it does natively. What does not
   cross: a paint widget, `takes_text`, `on_paste`, the clipboard, the
   page's selection, `windowed`. The events that cross are keys, pointer
   events and `input` (`crates/horizon-ui/wit/plugin.wit`).
3. Add the preview to `preview-plugin/src/lib.rs`: a name in `names`, an arm
   in `view` that builds the view over sample data, and the view's CSS in
   `css`.

## Checking it

```sh
scripts/check-preview-plugin.sh
```

builds the plugin and runs `tests/preview.rs` (`#[ignore]`d otherwise),
which draws the preview through the plugin and drives it. Add a test there
for a new preview, the way `tests/board.rs` drives the same view built
natively. Needs `rustup target add wasm32-wasip2`. Not part of the gate.

`scripts/check-preview-wasm.sh` (in the gate) only checks that the library
still builds for the plugin's target.

## Showing it to the owner

```sh
cargo build -p horizon-preview-plugin --target wasm32-wasip2
horizon preview target/wasm32-wasip2/debug/horizon_preview_plugin.wasm --name <preview>
```

opens a preview pane in the Horizon the command reaches (the one whose
pane it is run in), without taking the focus unless `--active` is given.
The pane reads the file again whenever a build replaces it, so the loop
after that is edit and build. This shows something on a person's screen:
do it when they have asked to see the view, not as a check of your own.
