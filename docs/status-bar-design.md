# Status Bar Design

Scope: the bar at the bottom of the window, which says the keys that
work where the keys are, each with what it does (board #84). The
decisions behind it, and the comparisons they were made on, are on that
item; this describes the bar as built.

## 1. An extension in a place of the shell's

The bar is an extension (`plugins/status-bar`): its view is what it
exports as `horizon:ui/status-bar` (`mount`), and the shell shows the
view of the extension named `status-bar` in its place under the
workspace (`src/workspace/hints.rs`). Another bar is one of that name:
named in the configuration's `extensions`, or used for now with
`horizon extension use <file>`, which shows it there at once. It is made
again when its component is built again.

The shell keeps no more of the bar than the place, and its own keys
(§3): what the bar says and how it looks are the extension's.

## 2. What it says

What the elements the focus is in keep of their keys, read after each
layout through `horizon:ui/keys` (`focused`: each element's keys, from
the one with the focus outwards, and whether it takes every key;
`typing`: whether the focus is in a text field), and made into what is
said by `horizon_ui::keys::said`:

- each element's keys, in their order, up to an element that takes
  every key;
- in a text field, none of a group of keys with one that is typed there
  (a character with no modifier but Shift);
- of keys spelled the same, the first, the innermost.

It says the keys of where the keys are now, not every operation there is:
finding any operation is the palette's (`docs/ux-principles.md`). It is
not pressed.

## 3. Where the keys come from

A view registers its keys once on an element with `El::keys`
(`crates/horizon-ui/src/keys.rs`): each key, or keys that do one thing
between them (`j/k`, `↑/↓`), with what it does, and what each key runs.
The element hears them (`keydown`) and keeps them, spelled and named, in
an attribute (`data-keys`); `El::keys_alone` (the alert dialog) is one
that takes every key, and marks it so (`data-keys-alone`). Keys are
spelled as a key binding is (`horizon_ui::Chord`): `enter`, `j`,
`shift+x`, `ctrl+shift+c`. A plugin's view keeps them through the
document's `set-attribute`, as any other attribute.

The shell keeps its own keys the same way, so the bar knows nothing of
workspace mode or of sheets:

| Where | What it keeps |
| --- | --- |
| The shell's root, in workspace mode | The mode's keys (`src/workspace/keys.rs`, `MODE`); with no tab, `:` |
| The shell's root, out of it | The chords the configuration binds: workspace mode's, then each command that can be carried out now, by its title |
| A sheet over the workspace (`over`) | That it takes every key |

The focus is in the root in the mode, and the root is around every view,
so its keys are the last read; under a sheet nothing past the sheet is.

What registers its keys this way: the board's two views (their key
tables, `src/board/model.rs`), the shell's sheets (the picker and the
finder), the alert dialog, and the list of sessions. A view that hears a
key with a `keydown` handler of its own does not have it said.

## 4. How it looks

One line, as wide as the window, under the workspace: the panes end
above it. Its ground is the views' (`--background`), a line of
`--border` above it. Each key is in a box of its own, as faint as a
pane's tag: the key in `--text-muted`, not bold, on a ground a step off
the bar's (`--chip-muted-surface`; the tag's own, `--band`, is next to
none on the views' ground, where a tag is never drawn), an edge of
`--surface-chrome` drawn by an inset shadow, which takes no room. Its
words are after it in `--text-muted`, at the micro size.

## 5. What does not fit

The bar is one line high, and its hints wrap: a line that wraps goes
under the bar's padding and out of sight (`overflow: hidden`), so what
does not fit is left out whole, from the end. Nothing says that some
were left out, and a label is not shortened. Whoever says keys says them
the most wanted first.

## 6. What is not in it yet

What else the bar may say -- a command's failure (board #85), a mode
indicator -- is not in it.
