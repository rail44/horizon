# Split Resize — Weight-Native, Component-Free

Status: owner-approved 2026-07-19; not yet implemented. Companion to
`recursive-layout-design.md` (which owns the tree/weight model this doc
renders); this doc owns how split sizes reach the screen and how drag
resize feeds back.

## Problem

Two symptoms, one root cause:

- **Second same-axis split collapses a pane.** After splitting twice in
  the same direction, the last pane in the group renders at ~95 px while
  the model says every child weighs 1/3.
- **Closing a middle pane shifts sizes onto the wrong panes.**

Root cause, traced into gpui-component's `resizable` module (pinned rev
`0775df3`, `crates/ui/src/resizable/`): the module's caller-facing
contract is *absolute pixels only*. `ResizableState` stores
`sizes: Vec<Pixels>` keyed by the group's element id, so it survives
re-renders — including structural ones. Horizon injects model weights
via `resizable_panel().flex_basis(basis)`, a style the panel's own doc
explicitly reserves ("driven by `ResizableState`, not by the caller"):
it only takes effect while the state has no recorded size
(`panel.rs:300-323`). The window between "state created" and "first
prepaint records real pixels" is the *only* time weights reach the
layout — which is why the first split looks right and nothing after it
does:

- On a same-axis split (2→3 children, same group id),
  `sync_panels_count` (`mod.rs:124-151`) keeps the two recorded pixel
  sizes and seeds the appended slot at `PANEL_MIN_SIZE` (100 px);
  `adjust_to_container_size` (`mod.rs:307-328`) then rescales
  `[960, 960, 100]` to the container and writes `size = Some(..)` into
  *every* panel — from that point the weight-derived basis is dead.
  Sizes are index-mapped, so the 100 px seed always lands on the last
  pane regardless of where the model inserted the new child.
- On close, `sync_panels_count` truncates the tail, whichever pane
  actually left.

Ratio maintenance on *window* resize is the same pixel bookkeeping:
a prepaint hook detects container change and rescales recorded pixels
proportionally (`panel.rs:159-173` → `adjust_to_container_size`). The
"ratio" was never first-class; structural changes are simply the one
path with no model→state resync.

## Decision

Stop rendering splits through `ResizablePanelGroup` entirely. Render
the split branch of `render_node` as a plain flex row/column and own
the resize handles. Model weights (`horizon-workspace`) are the single
source of truth; data flows one way:

- **Layout**: each child gets `flex_basis(0)` + `flex_grow(weight)` —
  the idiom the Floem shell used
  (`floem-shell-final:src/workspace/view/layout_tree.rs`). Zero basis
  makes the whole container leftover space, and flex distributes
  leftover ∝ grow, so 100% of the extent divides by weight ratio at any
  container size. Window resize proportionality falls out of flex; no
  bookkeeping, no container-change hook.
  *Trap, learned the hard way*: the fraction-basis idiom the
  gpui-component-era code carried (`flex_basis(weight/total × 1000 px)`
  + `flex_grow_1`) does **not** render ratios — grow factors tied at 1
  split the real leftover *evenly*, diluting any ratio toward equal as
  the container grows past the 1000 px reference (3:1 renders as
  ~0.63:0.37 at 1920 px). The GPUI migration picked it up to fit
  `resizable_panel`'s API, and `ResizableState`'s pixel overwrite
  masked it within a few frames.
- **Drag**: a handle between children `i` and `i+1` converts the mouse
  delta once, px → weight (`Δw = Δpx / effective_px × total_weight`),
  and updates the model per move. Pixels are never stored.
  `effective_px` is the container's measured length minus the handles'
  fixed footprint (`(n−1) × hit-area px`; handles are
  `flex_grow_0`/`flex_shrink_0`, outside the grow distribution) — using
  the raw length would make the handle lag the mouse and miss the floor
  by the handles' total width.
- **Minimum size**: a display-side floor (`min_w`/`min_h`, replacing
  the component's `PANEL_MIN_SIZE` role). When the *window* shrinks,
  flex clamps the display and weights stay untouched, so ratios restore
  exactly when it grows back — better than the component's destructive
  rescale.

gpui-component itself remains a dependency (palette/list, TitleBar,
etc.); its own internal uses of `resizable` (table columns, sheets) are
unaffected. Only Horizon's split rendering leaves it.

## Drag semantics (owner-decided 2026-07-19)

- **Pairwise.** A drag moves weight strictly between the two adjacent
  children; other siblings are untouched. The drag hard-stops when
  either neighbor would fall below the display floor (converted to
  weight at the current container size). This is the tmux/i3 mental
  model, chosen over gpui-component's cascade (which squeezes
  successive panes once a neighbor bottoms out) for locality and
  predictability.
- **Live reflow.** Weights update per mouse-move (`cx.notify` each
  move), so panes — including terminal PTY sizes — reflow continuously
  during the drag, matching current behavior and cost. Persistence
  (`persist_workspace`) fires on mouse-up only, matching the current
  `on_resize` cadence. *Preview-commit* (guide line during drag, single
  reflow on release, sparing terminals the SIGWINCH stream) is recorded
  as something to **evaluate** if live reflow proves painful in
  practice — noted, not pre-approved.

## Alternatives rejected

- **Generation-keyed state reset** (bump the group id on structural
  change to mint a fresh `ResizableState`): one line, but keeps *two*
  undocumented dependencies — the reserved `flex_basis` gap and the
  `== PANEL_MIN_SIZE` fresh-seed marker in `update_panel_size` — either
  of which a rev bump can silently close.
- **Own the state + imperative sync** (`with_state` at every split,
  push weight×container pixels on structural change): upstream's own
  Dock shape (`stack_panel.rs` drives `state.insert_panel(px, ix)`),
  but the API Dock uses is `pub(crate)`; the public surface is
  `resize_panel(ix, px)`, whose drag-style redistribution makes "set
  all panes to these ratios" awkward. Keeps dual truth and a hand-
  maintained list of sync points.
- **Fork the resizable module weight-native**: cheap to start (~1000
  lines, thin deps) but a genuine weight-native rewrite replaces
  `ResizableState`'s core anyway — it converges to this decision with
  extra vendored code. Only the handle rendering would be reused, and
  the handle is the part Horizon already fights (see below).
- **Wait for/propose upstream ratio support**: upstream tried ratios
  (#833, 2025-05) and reverted within days (#840, #842). The revert
  reasons cut the other way for Horizon: #840's driver was *fixed-px
  sidebars* ("we just only want center dock to change" — Horizon's
  splits are all "center"), and #842's "conversions back and forth
  internally … prone to problems" is an indictment of dual
  pixel/ratio representation, i.e. of exactly what this decision
  removes. Posting upstream is out of scope per standing policy.

## Consequences

- `src/workspace/render.rs`: the `LayoutNode::Split` arm drops
  `h_resizable`/`v_resizable`/`resizable_panel`, the `on_resize`
  closure, and the `SPLIT_BOUNDARY_INSET_PX` inset workaround **and its
  root-cause essay** — the workaround existed because the component's
  handle is an absolutely-positioned child overlapping the previous
  pane's edge with unreachable z-order. An owned handle is an explicit
  element between children; pane borders and handle no longer contend.
- Handle needs, now Horizon's to render: hairline + comfortable hit
  area (component precedent: 1 px line, 4 px padding), hover/drag
  highlight, axis-appropriate cursor.
- `WorkspaceShell::set_split_weights` path stays as the drag pipeline's
  write target (its anchor-based split addressing and normalization are
  unchanged); only the caller moves from `on_resize` to the owned drag
  handler. Existing `set_split_weights_*` tests remain valid; the
  pairwise clamp math should land as a pure, colocated-tested function.
- `scripts/check-workspace-restore.sh` and workspace persistence are
  untouched (weights were always the persisted truth; restore already
  displays correctly today — until the next structural change).

## Deferred — recorded, not implemented

- **`SizePolicy` enum.** Future fixed-size panes (settings-driven side
  views etc.) extend the child's sizing from bare `weight: f32` to
  `Weighted(f32) | Fixed(Pixels)`; `Fixed` maps to `flex_none` + px
  basis, and the #840 sidebar semantic falls out of flex declaratively
  instead of via bookkeeping. Decided in advance for mixed boundaries:
  dragging a handle adjacent to a `Fixed` pane edits that pane's px and
  *preserves its policy kind* (rejected: unpin-on-drag, which silently
  discards a configured policy; inert handles, which kill direct
  manipulation). Until a real trigger appears, only the extensibility
  is kept in mind — the initial implementation ships `Weighted`-only
  with a plain `f32`.
- **Fixed-sum overflow.** `flex_none` panes exceeding the container
  don't shrink; whether `Fixed` gets a floored shrink or the window
  gets a minimum is decided when `Fixed` lands.
- **Preview-commit drag feedback.** See Drag semantics above.
