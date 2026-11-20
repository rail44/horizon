# Workspace layout: tab groups as a kind of container

Board #86 holds how this was arrived at.

## Problem

The workspace was a list of tabs, each holding a tree of splits: tabs ⊃
split tree, one level. Nothing could be shown that did not belong to a
tab, so a view one wants kept open whatever tab is shown -- the list of
sessions on the left, say -- had to be opened again in every tab, and
switching tabs hid it. Opening a session from a list pane also split the
list pane itself, since a new pane always split the active pane of the
active tab.

## Shape

One tree for the whole workspace (`crates/horizon-workspace/src/types/
tree.rs`), whose nodes are of three kinds:

- a **pane** (a leaf);
- a **split**: an axis and weighted children
  (`docs/recursive-layout-design.md`);
- a **tab group**: a list of tabs and which one is shown, each tab a
  subtree of its own with the pane in it focus goes back to.

```
LayoutNode = Pane(PaneId)
           | Split { axis, children: [ { node, weight } ] }
           | Tabs(TabGroup)

TabGroup   = { id, tabs: [Tab], shown: TabId }
Tab        = { id, root: LayoutNode, active: PaneId }
```

A fresh workspace is the tree whose root is one tab group. The list of
sessions kept on the left is

```
Split(horizontal, [ Pane(list), Tabs[ tab 1, tab 2, … ] ])
```

and two groups side by side are the same thing again. "Outside the tabs"
is not a place of its own: it is whatever is not under a tab group.

### Invariants

- No split with one child, no split child sharing its parent's axis.
- A tab group that is not the root has at least one tab: one whose last
  tab closes is removed from its parent, which is flattened again.
- The root may be a tab group with no tabs: the empty workspace, the
  implicit command surface. Panes beside it stay.
- A tab group with one tab keeps its strip.

The tree is normalized by `layout.rs`; a group is never spliced into
anything.

## Focus and the cursor

- **Focus** (where keys go) is one pane of the workspace, under a tab
  group or not. Each tab remembers its own active pane, where focus goes
  when the tab is shown again; the workspace remembers the group that
  last had focus.
- **The workspace-mode cursor** (`mode.rs`'s `Cursor`) is a pane, or the
  strip of a tab group: the group as a whole. A strip is no pane, so
  pane commands (terminate, close pane) do nothing there.
- `h/j/k/l` move by geometry everywhere (`nav.rs`'s `layout_rects`), over
  the rectangles of every visible pane and every visible strip -- a band
  along the top of its group. From a group's top pane `k` reaches its
  strip, `j` from the strip goes back into the shown tab, and `h/l` from
  a strip go to what is nearest at its height: a pane beside the group
  running the full height, or another group's strip.
- `Tab`/`Shift+Tab` in the mode go round the tabs of the group the
  cursor is in, else of the group that last had focus.
- `Enter` on a strip gives focus to the active pane of the tab the group
  shows. `x` on a strip closes that tab.

## What operations act on

Splits and the New Tab command act on the node the cursor is on.

| Operation | Cursor on a pane | Cursor on a strip |
|---|---|---|
| Split Right / Left / Down / Up | beside the pane | beside the whole group |
| New Tab | a tab in the nearest group around the pane; on a pane under no group, that pane's place becomes a group, the pane its first tab | a tab in that group |
| Close Tab | the shown tab of the nearest group | the shown tab of that group |
| Close Pane | the pane, unless it is its tab's last | -- |

Tab operations with the cursor on a pane under no group act on the group
that last had focus.

Everything else that opens a tab -- the command line's `horizon new`,
the list of sessions' "terminal here", a board thread -- opens it in the
group the cursor is in, else the one that last had focus, and with no
group at all in a new one right of the rest. Only the New Tab command
makes a group of a pane.

A session no pane shows (going to it from the list of sessions, a
detached one attached) is opened beside the active pane of the shown tab
of the group that last had focus -- not beside the pane that asked,
which may be the list itself.

Groups are made only from a pane under no group, so a group inside a
tab of another group does not arise from these operations.

## Reordering

The views are reordered without changing the tree's shape: two of them
swap places (board #94). What changes the shape is a split, or a pane
moved into a new tab.

- In workspace mode, Shift and a direction (`H` `J` `K` `L`, or Shift and
  an arrow) swap what the cursor is on with what the cursor would move
  to in that direction; the cursor goes with it, and the mode stays. On
  a pane, the pane moves; on a strip, the group moves as a whole.
- A group goes in no tab of another group: swapped with what is in a
  group, it swaps with that group as a whole. A pane swaps into a tab
  freely, and is that tab's active pane.
- Nothing is swapped with what it is in (a pane and its own group's
  strip), and nothing at an edge.
- Move to New Tab moves the pane the cursor is on into a new tab of the
  group it is in -- a pane in no group goes to the group a new tab would
  -- shown, with the focus. A pane alone in its tab already stays.

With the pointer (`src/workspace/moving.rs`), a view is taken by its
tab's label, which takes the tab's active pane, or by its tag (below).
Dropped on another view, the two swap; dropped on a group's strip, it is a new tab
of that group -- the same operations the keys and Move to New Tab carry
out. While it moves, the view or strip it would go to is marked and its
title goes beside the pointer. A press that does not go a few pixels is
no move, so a click on a tab's label is still the click.

## Shares

A split's parts share it by their weights. The pointer changes them by
dragging the boundary between two parts. In workspace mode the keys do
(`crates/horizon-workspace/src/resize.rs`), on the innermost split
around what the cursor is on along the key's axis -- a split in a tab is
nearer than the one its group is in: `<` and `>` across, `-` and `+`
down, give the cursor's part a step (5% of the split) less or more, from
the part after it, or before it where it is the last; `=` makes the parts
of the innermost split around it, along either axis, even. Neither way
leaves a part less than a tenth of what it shares with the other. The
mode stays.

## Persistence

`workspace.json` is version 2: the root node with the groups inside it,
focus, and the group that last had focus. A file of an older version is
written over (`src/restore.rs`): the tabs and splits it held are lost
once, and the sessions the runtimes still run come back detached. One of
a newer version is a newer build's, and is kept.

## Drawing

The arrangement (`src/workspace/arrangement.rs`) draws the whole tree: a
tab group is its strip above its shown tab, so a pane beside a group
runs the window's full height. The boxes are built again whenever the
tree's shape changes; the panes and the strips are kept and moved into
the new boxes, a strip let go of once its group is gone.

What is drawn is chosen so that what holds what reads from the drawing
(board #95, each point chosen by the owner from pictures of it):

- A tab group's shown tab, and a pane in no group, are each a card: a
  border and rounded corners, no shadow, on a ground of the cards' own
  color. Cards are apart by a gap; the panes of a split in a tab are
  apart by a line, so a split in a tab reads apart from one between
  cards (`in-tab`).
- A group's strip is above its card, with no frame of its own. The tabs
  share its width; those not shown are their words, a faint line between
  two of them, and the shown one is outlined.
- A pane's tab label names it where the pane is all of its tab. Any
  other pane -- one of a split tab, one in no group -- has a tag in its
  top right corner, clear of a terminal's text, which begins at the
  left: its name and the grip it is taken by. The tag shows while the
  pointer is over the pane or in workspace mode, so the pane with the
  focus does not repeat its tab's label.
- The pane with the focus has its border in the accent. In workspace
  mode every pane is veiled and the cursor's is tinted with the accent;
  on a strip, its shown tab is.
- A sheet over the workspace (the palette, a chooser, a dialog) is framed
  as the cards are, what is behind it dimmed.

What a tag shows by -- whether it names its pane, workspace mode, a view
moving -- is a class on the tag itself. A class an element around it
gains does not reach its style in every case: a tag left with the style
it had while a view moved, after the move, was not hit by the pointer.

## Layouts

What a person wants kept where -- the list of sessions on the left, say --
is theirs to say, not the workspace's: the configuration names layouts,
trees to put the workspace in (`[layouts.<name>]`, board #87).

- A layout is a tree of **views**, **splits** with weighted children, and
  **tab groups**, with at most one place for **the workspace as it is**
  (`workspace = true`). It holds no session: a view is made as `horizon
  new <name> <arguments>` makes it, and `terminal` or `agent` is refused.
- `Apply Layout…` puts the workspace in one, chosen by name; and
  `startup_layout` names the one it is put in when Horizon starts, once the
  extensions are read (a layout's views may be theirs), on the workspace
  as it was restored. `Reload Config` does not put it in again.
- Putting it in (`Workspace::arrange`): a pane already showing a view the
  layout has -- the same plugin on the same state -- is moved into place
  rather than made again, so putting the workspace in the same layout
  again, or starting with one a restored workspace already has, changes
  nothing. The workspace as it is goes in its place; a layout with none
  closes the panes the workspace had, their sessions left detached. A
  place with nothing to hold -- an empty workspace -- is left out.
- Focus stays where it was while that pane is on screen; else it goes to
  the first pane on screen.
- The configuration keeps a layout as the file writes it, and reads it
  when it is used (`crates/horizon-config/src/layouts.rs`): one that is not
  well formed is refused then, saying where, and leaves the rest of the
  file read.

## The command line

The `workspace_snapshot` an agent asks its host for gives the tree, and
names a tab by its group -- the group's place among the tree's groups,
outermost first -- and its index in that group.

## Open questions

1. **Moving panes** between groups and in and out of them belongs to
   rearranging panes, which is decided apart.
2. **Nested groups**: nothing makes one today. If moving panes comes to
   make one, how its strip inside another group's tab reads is to be
   drawn and looked at.
