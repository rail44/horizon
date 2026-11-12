---
name: preview-view
description: Use when implementing or changing a view in Horizon and you want it shown from the preview plugin - registering a named preview with sample data, building the plugin, and checking it windowlessly. Trigger words: preview a view, preview pane, show the view to the owner, view feedback, horizon preview, make a view previewable.
---

# Previewing a view under development

A view can be built into a WebAssembly plugin (`preview-plugin/`) and shown
from it by `src/preview.rs`: the plugin has the view's signals, effects and
handlers, and the document is the shell's. This is a prototype: there is
no pane kind and no `horizon preview` command for it yet, so today it is a
windowless check, not a way to show a person a view in their running
Horizon.

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
