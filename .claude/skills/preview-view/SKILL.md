---
name: preview-view
description: Use when implementing or changing a gpui view in Horizon and you want it checkable before merge - registering a named preview with sample data, building the preview plugin, checking it windowlessly, and showing it to the owner in a pane of the running Horizon. Trigger words: preview a view, preview pane, show the view to the owner, view feedback, horizon preview, make a view previewable.
---

# Previewing a view under development

Horizon can show a gpui view from your branch inside a pane of the owner's
running Horizon, drawn by the real renderer, without a merge or a restart.
The view is compiled into a WebAssembly plugin (`preview-plugin/`). Design:
`docs/preview-pane-design.md`.

## 1. Make the view's code buildable for the plugin

The plugin is `wasm32-wasip2`: no sockets, no subprocesses, no direct
filesystem, no tokio runtime. Code that goes into the plugin must not
reach them.

- Whatever the view uses to reach the outside world must be replaceable
  with sample data. A view backed by a session entity gets a sample
  entity; a view that reads a store gets a store with sample contents.
  Put that seam in the store or session entity, not in a new layer inside
  the view. Keep it minimal.
- In `src/lib.rs`, a module is native-only when its declaration carries
  `#[cfg(not(target_family = "wasm"))]`. To preview a view, remove that
  gate from the view's module and gate only the parts inside it that need
  native-only dependencies. Dependencies that cannot build for wasm stay
  under `[target.'cfg(not(target_family = "wasm"))'.dependencies]` in the
  root `Cargo.toml`.
- `./scripts/check-preview-wasm.sh` tells you whether the library still
  builds for the plugin. It is part of the quality gate.

## 2. Register a named preview next to the view

Copy the shape of `src/preview/sample.rs`: a `NAME`, and a
`build(&mut Window, &mut App) -> AnyView` that constructs the view with
sample data. Add one entry to `PREVIEWS` in `src/preview/registry.rs`.
Write one preview per state worth looking at (empty, long titles, error,
many rows) rather than one preview with switches. `preview-plugin/` itself
never changes.

## 3. Build and check it yourself, windowlessly

```sh
(cd preview-plugin && cargo build --locked --profile quick --target wasm32-wasip2)
./scripts/check-preview-plugin.sh
```

Use the `quick` profile: load time tracks component size, and a debug
component loads several times slower. Your own verification is tests, not
the pane — assert on what the preview paints and how it reacts to input
the way `src/preview/e2e.rs` does. You cannot see the pane.

## 4. Ask the owner to look

Only when you want a person's feedback on the view:

```sh
horizon preview preview-plugin/target/wasm32-wasip2/quick/horizon_preview_plugin.wasm --name <preview>
```

This opens a pane in the owner's running Horizon without taking focus
(`--active` would take it; do not pass it unless asked). The pane appearing
is itself the request to look, so do not open it for your own checks. It
reloads by itself whenever you rebuild the artifact, and calling the
command again for the same path reloads instead of opening a second pane.
Say in your message which preview names to look at and what you want
judged.

## Limits

IME composition, clipboard, and modifier-key state do not reach a preview;
Latin typing, clicks, scrolling, hover, and focus do. A guest's window is
the pane's rectangle; how popovers and modals behave inside it has not
been checked.
