# Workspace layout: tab groups as a kind of container

Status: proposed. Board #86 holds how it was arrived at and what is still
open; nothing here is implemented yet.

## Problem

The workspace is a list of tabs, each holding a tree of splits
(`crates/horizon-workspace/src/types/entity.rs`): tabs ⊃ split tree, one
level. Nothing can be shown that does not belong to a tab, so a view one
wants kept open whatever tab is shown -- the list of sessions on the
left, say -- has to be opened again in every tab, and switching tabs
hides it. Opening a session from a list pane also splits the list pane
itself, since a new pane always splits the active pane of the active tab
(`operations.rs`'s `split_pane_in_tab`).

## Shape

One tree for the whole workspace, whose nodes are of three kinds:

- a **pane** (a leaf, as today);
- a **split**: an axis and weighted children, as today
  (`docs/recursive-layout-design.md`);
- a **tab group**: a list of tabs and which one is shown, each tab a
  subtree of its own.

```
LayoutNode = Pane(PaneId)
           | Split { axis, children: [ { node, weight } ] }
           | Tabs(TabGroup)

TabGroup   = { id, tabs: [Tab], shown: TabId }
Tab        = { id, root: LayoutNode, active: PaneId }
```

Today's workspace is the tree whose root is one tab group. The list of
sessions kept on the left is

```
Split(horizontal, [ Pane(list), Tabs[ tab 1, tab 2, … ] ])
```

and two groups of tabs side by side, or tabs on one side only, are the
same thing again. "Outside the tabs" is not a place of its own: it is
whatever is not under a tab group.

### Invariants

The tree is kept canonical, as the split tree is now:

- no split with one child, no split child sharing its parent's axis
  (as today);
- a tab group that is not the root has at least one tab: one whose last
  tab closes is removed from its parent, which is flattened again;
- the root may be a tab group with no tabs: the empty workspace, the
  implicit command surface it is today. Panes outside it stay.

A tab group with one tab keeps its strip; a group is something one made,
and does not dissolve into its parent.

## Focus and the cursor

- **Focus** (where keys go) is one pane of the workspace, under a tab
  group or not. Each tab still remembers its own active pane, which is
  where focus goes when that tab is shown again.
- **The workspace-mode cursor** is a pane, or the strip of a tab group.
  The strip is where a tab group as a whole is pointed at.
- `h/j/k/l` move by geometry everywhere, as decided for the split tree
  (`docs/recursive-layout-design.md`): the rectangles are every visible
  pane's and every visible strip's. From a group's top pane `k` reaches
  its strip, `j` from the strip goes back into the shown tab, and `h/l`
  from a strip go to what is nearest at its height -- a pane beside the
  group, or another group's strip.
- Which tab a group shows is chosen with `Tab`/`Shift+Tab` in the mode,
  in the group the cursor is in (today they cycle the workspace's tabs).
  `h/j/k/l` never change meaning on a strip.

## What operations act on

Every operation acts on the node the cursor is on; nothing is phrased
"beside the tabs".

| Operation | Cursor on a pane | Cursor on a strip |
|---|---|---|
| Split Right / Left / Down / Up | beside the pane, as today | beside the whole group |
| New Tab | a tab in the nearest group around the pane | a tab in that group |
| Close Tab | the shown tab of the nearest group | the shown tab of that group |
| Close Pane | the pane; an emptied tab closes, an emptied group goes | -- |

Split Left and Split Up are new (today only Right and Down exist); they
are what puts a pane on the left of a group.

## Persistence

`workspace.json` goes to version 2: the root node, with tab groups
inside it, and focus. A version 1 file is not read and is written over:
the tabs and splits it held are lost once, and the sessions the runtimes
still run come back detached, as any session the workspace did not know
does (`src/restore.rs`).

## Drawing

The arrangement (`src/workspace/arrangement.rs`) draws the whole tree,
not the active tab's: a tab group is its strip above its shown tab. The
strip leaves the shell's top edge (`src/workspace/mod.rs`) and is drawn
by the group it belongs to, so a pane beside the root group runs the
window's full height.

## Open questions

1. **Where a session no pane shows is opened.** Today it splits the
   active pane. With the list outside any group, that is the list again.
   Proposed: into the shown tab of the tab group last focused, which the
   workspace keeps.
2. **How a second tab group is made.** Splitting beside a group makes a
   plain pane. Proposed: New Tab with the cursor on a pane under no
   group turns that pane's place into a group, the pane its first tab.
3. **Nested strips.** A group inside a tab of another group draws a
   strip inside it. Whether that reads well wants drawing and looking at.
4. **The command line and its snapshot.** `workspace_snapshot` and
   `horizon tab …` speak of tabs by their index in one list; with more
   than one group an index is ambiguous. Proposed: the snapshot gives the
   tree, and a tab is named by its group and index.
5. **Moving panes** between groups and in and out of them belongs to
   rearranging panes, which is decided apart.
