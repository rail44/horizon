---
name: preview-view
description: Use when writing or changing an extension's view in Horizon and you want it shown to the owner from the branch you are on - building the extension, checking it windowlessly, and showing it in the running Horizon, in a preview pane or in place of the one in use. Trigger words: preview a view, preview pane, show the view to the owner, view feedback, horizon preview, extension being written, horizon extension use, try an extension.
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
The extension's commands are not carried out in a preview pane, and
nothing the shell lends an extension beyond the document is lent to it:
not the theme, not the workspace's sessions, not a view over the
workspace. An extension that asks for one of those stops in a preview
pane ("the sessions are not given to this plugin").

## Trying it in place of the one in use

For an extension that needs what a preview pane does not lend (the list
of sessions, the theme settings), or to look at it where the one in use
is shown (a command's sheet over the workspace, the startup layout):

```sh
scripts/extensions.sh build
horizon extension use target/wasm32-wasip2/debug/horizon_<name>_plugin.wasm
horizon extension list
horizon extension reset <name>
```

`use` takes the file's extension in place of the one of its name in the
Horizon the command reaches, with all an extension is lent; its panes are
made again, and the file is read again whenever a build replaces it.
`list` says where each loaded extension is from (`temporary` for one put
in this way). It stays until `reset` (the `Reset Extensions` command does
that from the palette) or until Horizon is started again; reading the
configuration again leaves it in. What it does is real: the list of
sessions ends real sessions. Reset it when the look is over.

Either way this shows something on a person's screen: do it when they
have asked to see the view, not as a check of your own.
