# Preview pane

A preview pane shows an extension that is still being written inside a
pane of the running Horizon, without merging the branch and without
restarting the app. Horizon loads the extension's component from its
file, shows its view in its own document, and reads the component again
when the file on disk changes.

(Until 2026-11-13 the pane showed a gpui view, drawn in the plugin and
replayed by the GPUI shell's renderer. That went with the GPUI shell.
Until 2026-11-16 it also showed "preview plugins": views of the shell's
own, built into a wasm plugin over sample data, for which the root
library was kept building for `wasm32-wasip2`. With the Markdown view,
the wiki and the theme settings extensions themselves, the views that
could be shown that way and were not extensions came down to the board's
list, and that kind went: the root package builds for the host alone.)

## Mechanism

A view is a function that builds elements once and follows signals
afterwards, through `horizon-ui`'s `El` and `Page`. For `wasm32-wasip2`
`horizon-ui` is a different implementation of the same names
(`crates/horizon-ui/src/guest.rs`): an `El` is a number, and what the view
does to it is a call out of the component
(`crates/horizon-ui/wit/plugin.wit`). What an extension is and what
crosses between it and the host is `docs/extension-boundary-design.md`'s.

The extension has the view's signals, effects and handlers. The document
is the shell's: the host (`src/plugin_host.rs`) does what the extension's
calls ask in the shell's own document, on nodes the extension made and no
others. So a previewed view is styled by the shell's theme, laid out and
drawn by the shell's renderer with the shell's fonts, beside the other
panes.

An extension in a preview pane is held to what any extension is: the
directories its manifest names are lent to it, it has at most 512 MiB of
memory, and five seconds for any one poll of its future (a call into it,
or what its tasks do when they are woken). One that traps is let go of,
and the pane says what it said.

What a preview pane does not give an extension: its commands are not
carried out there, and the shell's theme is not given to it
(`horizon:ui/theme`), so the theme settings are not looked at this way.

## The pane

A preview pane is the `preview` plugin (`src/plugin.rs`,
`preview::PreviewPlugin`). Its state is the component's file and what the
extension's pane is made from, which the workspace saves, so after a
restart the pane comes back on the same file. A pane with nothing to show
yet is empty, and an empty preview pane under the cursor takes the next
`horizon new preview`. A pane given another file, or other arguments, is
made again on them.

The component is compiled off the UI thread. While it is, the pane says
so, and what it showed before stays until the new one is ready. The pane
watches the file's directory, and reads the file again 300 ms after it
last changed (a build unlinks and links it again).

A pane given the keys before its extension was read has them on itself;
when the extension's view is put in, they go on to it.

The extension is the pane's alone: one of the same name that the shell
has loaded (one it comes with, or one the configuration names) stays as
it is, and so do its panes. So a build from any checkout is looked at
beside what is in use.

## Commands

```
horizon new preview <extension.wasm> [<argument>...] [--split [<session-id>]] [--active]
```

The arguments are what the extension's pane is made from, as
`horizon new <extension>` takes them: a file among them is a path from
where the command is run.

Without `--active` the pane does not take the focus. A file that a pane
shows already is read again by that pane, made on what was asked for;
otherwise an empty preview pane under the cursor takes it; otherwise a new
tab or split opens.

`Reload Preview` (the plugin's command `reload-preview`, no default
chord) reads the file of the cursor's preview pane again.

An agent's own checks are windowless. Opening a preview in the running
Horizon is how an agent asks a person to look at a view.

## What is looked at this way

- An extension being written, from any checkout:
  `scripts/extensions.sh build`, then
  `horizon new preview target/wasm32-wasip2/debug/horizon_<name>_plugin.wasm <arguments>`.
- The components' gallery (`plugins/gallery`): every component of
  `horizon-components` in every kind it has. It is an extension the shell
  does not come with, there to be looked at in a preview pane.

A view of the shell's own (the board, the terminal, the agent) is not
shown this way.

## Checks

`tests/preview.rs`, in the gate, drives a preview pane through real
wasmtime with no window, over the fixture extension (`plugins/fixture`,
whose views are each there for one thing the host does for a guest) and
the gallery: a view is drawn with its sheet and takes its keys, a drag
asks the page what is under it, a view hears what is selected of its
text, a file replaced is read again, a call that does not come back is
ended, what cannot be shown is said in the pane, and the control plane's
`new preview` shows one in a pane of the whole shell.
