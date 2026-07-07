# Recursive Layout — N-ary Tiling

Status: owner-approved 2026-07-07; implemented in slices (this doc records
the whole design; slice status tracked in the roadmap).

## Problem

A tab's panes are a layout tree, but three things are missing/wrong:
(a) `SplitAxis` has only `Horizontal` — no vertical axis, so workspace-
mode `j`/`k` are no-ops; (b) the tree is *binary*, and `split_tab`
wraps the whole tab root on every split, so panes only ever "append to
the right" (a binary tree's characteristic wart: N-in-a-row is forced
into asymmetric nesting); (c) rendering ignores the tree entirely — a
fixed 4-slot equal-width flex row (`MAX_VISIBLE_PANES = 4`), and `ratio`
is never read.

## Decisions

Reached with the owner by reasoning from premises (binary-tree doubt →
three independent axes below → the parent/child that Horizon cares about
is *session* parentage, which lives in the session model, so the layout
need not carry grouping → the only remaining reason to prefer a tree is
pure spatial expressiveness, specifically "place a pane beside an
existing one at its size" → N-ary tree, kept shallow).

Three independent axes, decided separately:

1. **Topology — N-ary tree.** A `Split` holds an axis and a *variable-
   length* list of weighted children (i3/sway shape), not a fixed
   `first`/`second` pair. This is chosen *for* its shallowness (below);
   it is the reason N-ary beats binary here.
2. **Sizing — weight (ratio), no solver.** Each child carries a weight;
   siblings divide their container's extent by weight. Floem's Taffy
   (`flex_grow`/`flex_basis`) does the arithmetic. A constraint solver
   (Cassowary lineage) is deferred until a real trigger appears (cross-
   tree size linking, aspect-ratio-locked viewers, ancestor-spanning
   drag resize) — none exist yet.
3. **Focus navigation — geometric.** Direction moves (`hjkl`) resolve to
   the nearest pane in that direction by rectangle geometry (bspwm
   style), NOT by tree traversal (i3's tree-structural resolution is the
   source of its well-known "focus direction is confusing" wart). This
   axis is independent of topology. [slice 4]

## The shallow-nesting invariant (the core of this slice)

The tree is kept in canonical form so nesting appears *only where a
horizontal and a vertical split cross*:

- **No single-child `Split`** (meaningless — it is just its child).
- **No `Split` child whose axis equals its parent's** (redundant —
  splice it into the parent).

Consequences: the depth along any root→leaf path equals the number of
axis alternations on it; a row of N panes is depth 1 regardless of N.

Maintained by two mechanisms:

- **Insert absorbs same-axis:** splitting pane P in axis A, when P's
  parent container already has axis A, inserts the new pane as P's
  sibling in that container (no new node). When P is the tab root, or
  P's parent has the perpendicular axis, P is wrapped in a new `Split{A,
  [P, new]}` at P's position — the single place depth grows.
- **Flatten after removal/mutation:** collapsing a leg (existing
  behavior) plus splicing any same-axis child into its parent and
  dropping single-child splits, applied after close and after any tree
  mutation that could introduce redundancy.

## Grouping is session-side (out of scope for layout)

"Operate on a parent and everything under it" (move/close a group) is,
for Horizon, about *session* parentage (an agent and the views it
spawned), not spatial nesting. That relation belongs in the session
model, independent of layout topology; the layout tree deliberately does
not encode it. (This is what let us pick N-ary purely on spatial-
expressiveness grounds.)

## Slices

1. **N-ary model + shallow invariant (headless).** Type change, layout
   ops (pane list, leg removal, split-at-pane absorb/wrap, flatten,
   invariant), `split_tab` splits at the focused pane honoring an axis
   param — callers still pass Horizontal, so no view/CLI/command change
   and no visible behavior change yet. Fully unit-tested. [this slice]
2. **Recursive rendering + de-cap.** Replace the fixed 4-slot render with a
   recursive nested `h_stack`/`v_stack` mirroring the tree, weighted by
   each child's weight; make the per-pane fixed `[_; 4]` arrays (drafts,
   focus requests) `PaneId`-keyed and dynamic; remove `MAX_VISIBLE_PANES`.
   The vertical axis is not yet reachable from any UI surface at this
   point -- every split site still passes `Horizontal` -- but the
   renderer is already axis-generic, so slice 3's new vertical splits
   render correctly with no further rendering change.
3. **Vertical entry.** Expose the vertical axis in the UI: retire
   `Split Pane…` for two placement verbs `Split Right…` (horizontal) /
   `Split Down…` (vertical), each opening the same kind/role chooser;
   thread an axis onto `CommandInvocation::CreateSession` down to
   `Workspace::split_session_with_new_session`. Vertical creation and
   correct rendering land together (rendering already shipped in slice 2)
   so there is never a "vertical renders as horizontal" intermediate.
4. **2-D geometric navigation.** `move_cursor` resolves `hjkl` to the
   nearest pane in that direction using rectangles from a pure domain
   function (tree + viewport → per-pane rects); `j`/`k` stop being
   no-ops.

Deferred to a later roadmap item: interactive resize (border drag /
keyboard) mutating weights; letting the CLI request a vertical split
(`--vertical`).
