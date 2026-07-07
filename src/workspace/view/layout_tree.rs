//! Recursive rendering of a tab's layout tree
//! (`docs/recursive-layout-design.md`'s slice 2): mirrors `LayoutNode`'s
//! shape directly, replacing the old fixed 4-slot `h_stack`. A `Split`
//! becomes a nested `h_stack`/`v_stack` -- axis-generic: `Horizontal` rows,
//! `Vertical` columns -- with each child weighted via `flex_grow`; a `Pane`
//! becomes `pane::pane_view`.
//!
//! Every position in the tree is driven by a small resolver closure
//! (`NodeResolver`) that re-locates its own live node fresh on every
//! reactive pass, rather than a one-time snapshot -- so a position that
//! transitions between a leaf and a subtree, the one shape change that can
//! happen *without* its own identity (`PaneId`) changing (e.g. a tab's very
//! first split: the root goes from `Pane` to `Split` in place, see
//! `LayoutNode::split_pane`'s wrap case), still re-renders correctly.
//! That transition is gated by `create_memo` (equality-checked, so it only
//! fires when the position's own leaf/split shape actually flips) feeding a
//! `dyn_container`. Below it, `dyn_stack` keys each child by its
//! `RenderChild::anchor` (`workspace::layout::split_render_plan`), so an
//! ordinary add/remove/reorder (any other split or close) only touches the
//! views actually affected -- every other pane's local UI state (approval
//! focus, the cancel-turn latch, ...) survives undisturbed.

use std::rc::Rc;

use floem::prelude::*;
use floem::reactive::create_memo;

use crate::ui::theme;
use crate::workspace::layout::{split_render_plan, RenderChild};
use crate::workspace::types::{LayoutNode, SplitAxis};
use crate::workspace::{PaneId, Workspace};

use super::pane::{self, PaneViewState};

/// Locates the live node occupying one position in the tree, re-derived
/// fresh on every call rather than cached -- see this module's doc comment
/// for why.
type NodeResolver = Rc<dyn Fn() -> Option<LayoutNode>>;

pub(super) fn layout_tree_view(pane_state: PaneViewState) -> impl IntoView {
    let workspace = pane_state.control_input.command.workspace();
    let resolver: NodeResolver =
        Rc::new(move || workspace.with(|ws| ws.active_tab().map(|tab| tab.root.clone())));
    layout_position_view(pane_state, workspace, resolver)
}

/// One position in the tree: renders as a plain pane when the live node
/// there is a `Pane`, or as a nested split container when it's a `Split`.
fn layout_position_view(
    pane_state: PaneViewState,
    workspace: RwSignal<Workspace>,
    resolver: NodeResolver,
) -> impl IntoView {
    let is_split = {
        let resolver = resolver.clone();
        create_memo(move |_| matches!(resolver(), Some(LayoutNode::Split { .. })))
    };
    dyn_container(
        move || is_split.get(),
        move |is_split| {
            if is_split {
                split_view(pane_state.clone(), workspace, resolver.clone()).into_any()
            } else {
                match resolver() {
                    Some(LayoutNode::Pane(pane_id)) => {
                        pane::pane_view(pane_state.clone(), pane_id).into_any()
                    }
                    // The position vanished between the memo tick that
                    // chose this branch and now (its pane just closed) --
                    // the parent `dyn_stack` drops this view on its own
                    // next diff pass; render nothing in the meantime
                    // rather than panic.
                    _ => empty().into_any(),
                }
            }
        },
    )
}

/// A `Split` position: an axis-generic `h_stack`/`v_stack` of its current
/// children, each keyed by its own stable anchor and weighted via
/// `flex_grow`. The `gap`/`padding`/`background` combination is the
/// classic "background peeks through the gap" divider trick -- moved here
/// (from the single top-level `h_stack` the pre-slice-2 fixed layout used)
/// so nested splits get the same divider lines between their own children.
fn split_view(
    pane_state: PaneViewState,
    workspace: RwSignal<Workspace>,
    resolver: NodeResolver,
) -> impl IntoView {
    let each_resolver = resolver.clone();
    let stack = dyn_stack(
        move || {
            each_resolver()
                .as_ref()
                .and_then(split_render_plan)
                .map(|(_, children)| children)
                .unwrap_or_default()
        },
        |child: &RenderChild| child.anchor,
        {
            let resolver = resolver.clone();
            move |child: RenderChild| {
                let child_resolver = child_node_resolver(resolver.clone(), child.anchor);
                layout_position_view(pane_state.clone(), workspace, child_resolver).style(
                    move |s| {
                        s.flex_basis(0.0)
                            .flex_grow(child.weight)
                            .min_width(0.0)
                            .min_height(0.0)
                    },
                )
            }
        },
    );

    let axis_resolver = resolver;
    stack.style(move |s| {
        let axis = axis_resolver()
            .as_ref()
            .and_then(split_render_plan)
            .map(|(axis, _)| axis)
            .unwrap_or(SplitAxis::Horizontal);
        let s = match axis {
            SplitAxis::Horizontal => s.flex_row(),
            SplitAxis::Vertical => s.flex_col(),
        };
        s.width_full()
            .height_full()
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .gap(1)
            .padding(1)
            .background(theme::border_subtle())
    })
}

/// Builds the resolver for the child anchored at `anchor` within whatever
/// `parent` currently resolves to -- chaining resolvers this way (rather
/// than caching some stable id for a `Split` node, which `LayoutNode`
/// doesn't have) is what lets each position re-locate itself fresh on every
/// reactive pass, regardless of how its ancestors have reshuffled around
/// it.
fn child_node_resolver(parent: NodeResolver, anchor: PaneId) -> NodeResolver {
    Rc::new(move || match parent()? {
        LayoutNode::Split { children, .. } => children
            .into_iter()
            .find(|child| child.node.first_pane() == Some(anchor))
            .map(|child| child.node),
        LayoutNode::Pane(_) => None,
    })
}
