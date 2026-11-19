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
the new boxes, a strip let go of once its group is gone. A strip is
marked while the cursor is on it.

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
