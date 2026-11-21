# Status Bar Design

Scope: the bar at the bottom of the window, which says the keys that
work where the keys are, each with what it does (board #84). The
decisions behind it, and the comparisons they were made on, are on that
item; this describes the bar as built.

## 1. What it says

What it says follows where the keys are (`src/workspace/hints.rs`):

| Where the keys are | What the bar says |
| --- | --- |
| Workspace mode, nothing over the workspace | The mode's keys (`src/workspace/keys.rs`, `MODE`) |
| Workspace mode with no tab | `:` for the commands |
| Anything else | The keys of the elements the focus is in, from the one with the focus outwards; and, but while something is over the workspace, the chords the configuration binds (workspace mode's first, then each command that can be carried out now, by its title) |

It says the keys of where the keys are now, not every operation there is:
finding any operation is the palette's (`docs/ux-principles.md`). It is
not pressed.

## 2. How it looks

One line, as wide as the window, under the workspace: the panes end
above it. Its ground is the views' (`--background`), a line of
`--border` above it. Each key is in a box of its own
(`--chip-muted-surface`, its edge drawn by an inset shadow, which takes
no room), its words after it in `--text-muted`, at the micro size.

## 3. What does not fit

The bar is one line high, and its hints wrap: a line that wraps goes
under the bar's padding and out of sight (`overflow: hidden`), so what
does not fit is left out whole, from the end. Nothing says that some
were left out, and a label is not shortened. Whoever says keys says them
the most wanted first.

## 4. Where the keys come from

A view registers its keys once on an element with `El::keys`
(`crates/horizon-ui/src/keys.rs`): each key, or keys that do one thing
between them (`j/k`, `↑/↓`), with what it does, and what each key runs.
The element hears them (`keydown`) and keeps them, spelled and named, in
an attribute (`data-keys`); the bar reads that attribute off the
elements the focus is in (`Page::focused_keys`). So the keys the bar
says are those of the elements there are and the focus is in: a search
field's while it has the focus, a dialog's while it is open. Keys are
spelled as a key binding is (`horizon_ui::Chord`): `enter`, `j`,
`shift+x`, `ctrl+shift+c`.

A plugin's view registers its keys the same way: the attribute reaches
the document through the `document` interface's `set-attribute`, as any
other attribute does, so nothing else crosses the plugin boundary for
them.

- **Typed keys.** A character with no modifier but Shift is typed, not
  pressed, in a text field (`input`, `textarea`): an element does not
  hear it there, and the bar leaves out a group of keys with one in it.
  A plugin's view knows which of its nodes are text fields from the
  nodes it made (`guest::is_text_field`); the host reads the document.
- **An element that takes every key.** `El::keys_alone` (the alert
  dialog) takes every key while the focus is in it, and is the last
  element the bar reads: nothing behind it is said.
- **The same key twice.** Of keys spelled the same along the way out, the
  first, the innermost, is said.
- **A key that is not taken.** A key registered with `Key::when` says
  whether it took the key; one not taken goes on to what is around the
  element (Escape in the shell's sheets, which the sheet takes to close
  them).

What registers its keys this way: the board's two views (their key
tables, `src/board/model.rs`), the shell's sheets (the picker and the
finder), the alert dialog, and the list of sessions. A view that hears a
key with a `keydown` handler of its own does not have it said.

## 5. What is not in it yet

What else the bar may say -- a command's failure (board #85), a mode
indicator -- is not in it. The bar is a line of hints with room beside
them.
