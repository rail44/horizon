---
name: preview-view
description: Use when writing or changing an extension's view in Horizon and you want it shown to the owner from the branch you are on - building the extension, checking it windowlessly, and showing it in a preview pane of the running Horizon. Trigger words: preview a view, preview pane, show the view to the owner, view feedback, horizon preview, extension being written.
---

# Showing an extension that is being written

A view that is an extension (`plugins/*`: the Markdown view, the wiki,
the theme settings, the components' gallery) can be shown from its
component's file in a pane of the running Horizon, beside the one in use,
without merging and without a restart (`src/preview.rs`,
`docs/preview-pane-design.md`). The extension has the view's signals,
effects and handlers, and the document is the shell's.

A view of the shell's own (the board, the terminal, the agent) cannot be
shown this way: it is checked by its windowless tests, and looked at in a
Horizon built from the branch.

## Writing a view as an extension

1. The extension is one crate under `plugins/`, built for `wasm32-wasip2`
   only, with one module under `#[horizon_ui::plugin(..)]`
   (`plugins/markdown/src/lib.rs` is the smallest that keeps a state;
   `plugins/gallery/src/lib.rs` the smallest of all). It depends on
   `horizon-ui`, on `horizon-components` for what views are made of, and
   on no crate of the shell's.
2. A view in an extension uses `horizon_ui` as a view of the shell does.
   What does not cross: a paint widget, `takes_text`, `on_paste`, the
   clipboard, `windowed`. The events that cross are keys, pointer events
   and `input`, and a view reads what is selected of its own text
   (`crates/horizon-ui/wit/plugin.wit`).
3. Add the crate to `scripts/extensions.sh`, and, if Horizon is to come
   with it, to `OWN` in `src/extension.rs`.

## Checking it

```sh
scripts/extensions.sh build
cargo nextest run -p horizon --test extension --test wiki --test preview
```

The tests show an extension in a pane of the shell with no window and
drive it through the document (`tests/extension.rs`, `tests/wiki.rs`).
Add a test there for a new extension. They are in the gate
(`scripts/gate.sh`), which builds the extensions first. Needs
`rustup target add wasm32-wasip2`.

## Showing it to the owner

```sh
scripts/extensions.sh build
horizon new preview target/wasm32-wasip2/debug/horizon_<name>_plugin.wasm <arguments>
```

opens a preview pane in the Horizon the command reaches (the one whose
pane it is run in), without taking the focus unless `--active` is given.
The arguments are what the extension's pane is made from (a file for the
Markdown view, a page's name for the wiki). The pane reads the file again
whenever a build replaces it, so the loop after that is edit and build.
The extension's commands are not carried out in a preview pane, and the
theme is not given to it. This shows something on a person's screen: do
it when they have asked to see the view, not as a check of your own.
