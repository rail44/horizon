# Preview pane

A preview pane shows a view that is still being written inside a pane of
the running Horizon, without merging the branch and without restarting
the app. The view is compiled into a WebAssembly plugin; Horizon loads the
plugin, shows the view in its own document, and reads the plugin again
when the file on disk changes.

(Until 2026-11-13 the pane showed a gpui view, drawn in the plugin and
replayed by the GPUI shell's renderer. That went with the GPUI shell.)

## Mechanism

A view is a function that builds elements once and follows signals
afterwards, through `horizon-ui`'s `El` and `Page`. For `wasm32-wasip2`
`horizon-ui` is a different implementation of the same names
(`crates/horizon-ui/src/guest.rs`): an `El` is a number, and what the view
does to it is a call out of the plugin (`crates/horizon-ui/wit/plugin.wit`).

The plugin has the view's signals, effects and handlers. The document is
the shell's: `src/preview.rs` does what the plugin's calls ask in the
shell's own document, on nodes the plugin made and no others. So a
previewed view is styled by the shell's theme, laid out and drawn by the
shell's renderer with the shell's fonts, beside the native panes.

- An event that reaches a node the plugin listens at is told to the
  plugin (`handle`). What crosses: keys, pointer events, `input`.
- The plugin's effects run when the shell's do (`run-effects`), and its
  work after a layout when the shell's does (`after-layout`), with the
  page lent for the length of that call.
- The plugin gets no WASI grants (no files, sockets or environment), at
  most 512 MiB of memory, and five seconds for any one call or turn of
  its tasks: a watchdog thread moves the engine's epoch on when a call
  outlasts that, and the call traps. The engine is shared by every
  pane's plugin, so a store checks, when the epoch moves, whether its
  own call has gone on that long. A plugin that traps is let go of, and the pane says what it
  said.

What does not cross to a plugin: a paint widget, `takes_text`, `on_paste`,
the clipboard, the page's selection, `windowed`. A view that needs one of
them cannot be previewed yet.

## The plugin

`preview-plugin/` is a workspace member whose crate is empty for any
target but wasm. It names the previews (`horizon_ui::plugin!`): each is a
name and a function that builds one of the root library's views over
sample data. The root library therefore builds for `wasm32-wasip2`:
`src/lib.rs` marks the modules that cannot
(`#[cfg(not(target_family = "wasm"))]`), and a view that can keeps what it
needs of the host (a thread, a socket, a file) behind that gate inside
itself. `scripts/check-preview-wasm.sh`, in the gate, checks that the
library still builds for the target.

```sh
cargo build -p horizon-preview-plugin --target wasm32-wasip2
# target/wasm32-wasip2/debug/horizon_preview_plugin.wasm
```

## The pane

A preview pane is the `preview` plugin (`src/plugin.rs`,
`preview::PreviewPlugin`). Its state is the file and which of its previews
it shows, which the workspace saves, so after a restart the pane comes
back on the same file. A pane with nothing to show yet is empty, and an
empty preview pane under the cursor takes the next `horizon preview`.
A pane given another file or preview is made again on it.

The file is compiled off the UI thread. While it is, the pane says so,
and what it showed before stays until the new one is ready. The pane
watches the file's directory, and reads the file again 300 ms after it
last changed (a build unlinks and links it again).

A pane given the keys before its plugin was read has them on itself; when
the plugin's view is put in, they go on to it.

## Commands

```
horizon preview <path-to-wasm> [--name <preview>] [--split [<session-id>]] [--active]
```

Without `--active` the pane does not take the focus. A file that a pane
shows already is read again by that pane, with the preview asked for;
otherwise an empty preview pane under the cursor takes it; otherwise a new
tab or split opens. Without `--name` the plugin's first preview is shown.

`Reload Preview` (the plugin's command `reload-preview`, no default
chord) reads the file of the cursor's preview pane again.

An agent's own checks are windowless. Opening a preview in the running
Horizon is how an agent asks a person to look at a view.

## Checks

- `scripts/check-preview-wasm.sh`: the library builds for the plugin's
  target. In the gate.
- `scripts/check-preview-plugin.sh`: builds the plugin and runs
  `tests/preview.rs` (`#[ignore]`d otherwise) through real wasmtime, with
  no window: a view is drawn and takes its keys, a drag asks the page
  where it is, a file replaced is read again, a call that does not come
  back is ended, what cannot be shown is said in the pane, and the control
  plane's `preview` shows one in a pane of the whole shell. Not in the
  gate: the plugin's build and wasmtime's compiling of it take a while.
