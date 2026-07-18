use super::types::{LayoutChild, LayoutNode, PaneId, SplitAxis, TabId, Workspace};

/// One child in a `Split`'s rendering plan (see [`split_render_plan`]):
/// `anchor` is the stable identity a recursive renderer would key that
/// child's own nested view by across re-renders -- the child's own pane id
/// for a leaf, or its subtree's leftmost pane id otherwise (see
/// `LayoutNode::first_pane`). Two sibling subtrees can never share an
/// anchor: every `PaneId` in a tree is unique, so a subtree's leftmost leaf
/// uniquely identifies it among its siblings.
///
/// Test-only now: no production caller remains, since the gpui shell renders
/// splits off `resizable`'s own primitives rather than this plan
/// (2026-07-18 visibility audit). Kept as a unit-tested description of the
/// tree's rendering shape, decoupled from any UI framework.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderChild {
    pub(crate) anchor: PaneId,
    pub(crate) weight: f32,
}

/// The immediate rendering plan for one `Split` level -- its axis and, per
/// child, the identity/weight pair a renderer would need to key and size
/// that child's slot. `None` for a bare leaf (nothing to stack). See
/// [`RenderChild`]'s doc comment for why this is test-only now.
#[cfg(test)]
pub(crate) fn split_render_plan(node: &LayoutNode) -> Option<(SplitAxis, Vec<RenderChild>)> {
    match node {
        LayoutNode::Pane(_) => None,
        LayoutNode::Split { axis, children } => Some((
            *axis,
            children
                .iter()
                .map(|child| RenderChild {
                    anchor: child
                        .node
                        .first_pane()
                        .expect("a canonical Split's child always contains at least one pane"),
                    weight: child.weight,
                })
                .collect(),
        )),
    }
}

impl Workspace {
    /// Updates one rendered split from pixel sizes reported by the view.
    /// `split_anchor` plus the immediate child anchors identifies the split:
    /// a first-pane anchor alone is insufficient because the first nested
    /// split shares that anchor with each of its ancestors.
    pub fn set_split_weights(
        &mut self,
        tab_id: TabId,
        split_anchor: PaneId,
        child_anchors: &[PaneId],
        sizes: &[f32],
    ) -> bool {
        if sizes.len() != child_anchors.len()
            || sizes.len() < 2
            || sizes.iter().any(|size| !size.is_finite() || *size <= 0.0)
        {
            return false;
        }
        let total: f32 = sizes.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return false;
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return false;
        };
        tab.root
            .set_split_weights(split_anchor, child_anchors, sizes, total)
    }
}

impl LayoutNode {
    fn set_split_weights(
        &mut self,
        split_anchor: PaneId,
        expected_child_anchors: &[PaneId],
        sizes: &[f32],
        total: f32,
    ) -> bool {
        let Self::Split { children, .. } = self else {
            return false;
        };
        let own_anchor = children.first().and_then(|child| child.node.first_pane());
        let child_anchors: Vec<_> = children
            .iter()
            .filter_map(|child| child.node.first_pane())
            .collect();
        if own_anchor == Some(split_anchor) && child_anchors == expected_child_anchors {
            for (child, size) in children.iter_mut().zip(sizes) {
                child.weight = *size / total;
            }
            return true;
        }
        children.iter_mut().any(|child| {
            child
                .node
                .set_split_weights(split_anchor, expected_child_anchors, sizes, total)
        })
    }

    pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|child| child.node.pane_ids())
                .collect(),
        }
    }

    pub fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Pane(pane_id) => Some(*pane_id),
            Self::Split { children, .. } => {
                children.iter().find_map(|child| child.node.first_pane())
            }
        }
    }

    /// Removes `pane_id`, collapsing any `Split` left with a single child
    /// into that child directly (the N-ary generalization of the prior
    /// binary `(Some(only), None) => Some(only)` fold) and `None` once a
    /// `Split` loses all of its children. Does not by itself re-splice a
    /// same-axis child into its parent -- callers apply `flatten` after
    /// this for that (see the module doc on `docs/recursive-layout-
    /// design.md`'s shallow-nesting invariant); a same-axis nesting can
    /// only arise here if the tree already violated the invariant going in,
    /// which none of this module's own mutations do.
    pub(crate) fn without_pane(&self, pane_id: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) if *id == pane_id => None,
            Self::Pane(id) => Some(Self::Pane(*id)),
            Self::Split { axis, children } => {
                let remaining: Vec<LayoutChild> = children
                    .iter()
                    .filter_map(|child| {
                        child.node.without_pane(pane_id).map(|node| LayoutChild {
                            node,
                            weight: child.weight,
                        })
                    })
                    .collect();
                match remaining.len() {
                    0 => None,
                    1 => Some(remaining.into_iter().next().expect("len == 1").node),
                    _ => Some(Self::Split {
                        axis: *axis,
                        children: remaining,
                    }),
                }
            }
        }
    }

    /// Splits `target` in `axis`, inserting `new_pane_id` -- the shallow-
    /// nesting invariant's insert mechanism (`docs/recursive-layout-
    /// design.md`): absorbs into `target`'s parent container when it
    /// already has this axis (no new node), otherwise wraps `target` in a
    /// place `Split{axis, [target, new_pane_id]}` (the only place depth
    /// grows). The new pane is given the same weight as `target` when
    /// absorbed, and both children are weighted equally (1.0) when
    /// wrapped. Returns `false` (no-op) if `target` is not found in this
    /// subtree. `target` being this node's own bare root (no parent
    /// `Split` to absorb into or wrap within) is handled here too, since a
    /// leaf can always be wrapped in place via `*self = ...`.
    pub(crate) fn split_pane(
        &mut self,
        target: PaneId,
        new_pane_id: PaneId,
        axis: SplitAxis,
    ) -> bool {
        if matches!(self, Self::Pane(id) if *id == target) {
            *self = Self::Split {
                axis,
                children: vec![
                    LayoutChild {
                        node: Self::Pane(target),
                        weight: 1.0,
                    },
                    LayoutChild {
                        node: Self::Pane(new_pane_id),
                        weight: 1.0,
                    },
                ],
            };
            return true;
        }
        self.insert_pane(target, new_pane_id, axis)
    }

    fn insert_pane(&mut self, target: PaneId, new_pane_id: PaneId, axis: SplitAxis) -> bool {
        let Self::Split {
            axis: node_axis,
            children,
        } = self
        else {
            return false;
        };
        if let Some(index) = children
            .iter()
            .position(|child| matches!(child.node, Self::Pane(id) if id == target))
        {
            if *node_axis == axis {
                let weight = children[index].weight;
                children.insert(
                    index + 1,
                    LayoutChild {
                        node: Self::Pane(new_pane_id),
                        weight,
                    },
                );
            } else {
                children[index].node = Self::Split {
                    axis,
                    children: vec![
                        LayoutChild {
                            node: Self::Pane(target),
                            weight: 1.0,
                        },
                        LayoutChild {
                            node: Self::Pane(new_pane_id),
                            weight: 1.0,
                        },
                    ],
                };
            }
            return true;
        }
        children
            .iter_mut()
            .any(|child| child.node.insert_pane(target, new_pane_id, axis))
    }

    /// Normalizes the tree to the shallow-nesting invariant (`docs/
    /// recursive-layout-design.md`): no `Split` child shares its parent's
    /// axis (spliced into the parent, weights rescaled to preserve the
    /// child's overall share) and no `Split` has a single child (replaced
    /// by that child). A single bottom-up pass suffices: children are
    /// normalized first, so by the time this level splices/collapses, any
    /// nested same-axis run has already been pulled up as far as it can go
    /// below this level.
    pub fn flatten(&mut self) {
        let Self::Split { axis, children } = self else {
            return;
        };
        for child in children.iter_mut() {
            child.node.flatten();
        }

        let mut spliced = Vec::with_capacity(children.len());
        for child in children.drain(..) {
            match child.node {
                Self::Split {
                    axis: child_axis,
                    children: grandchildren,
                } if child_axis == *axis => {
                    let grand_total: f32 = grandchildren.iter().map(|g| g.weight).sum();
                    for grandchild in grandchildren {
                        let weight = if grand_total > 0.0 {
                            grandchild.weight / grand_total * child.weight
                        } else {
                            child.weight
                        };
                        spliced.push(LayoutChild {
                            node: grandchild.node,
                            weight,
                        });
                    }
                }
                other => spliced.push(LayoutChild {
                    node: other,
                    weight: child.weight,
                }),
            }
        }
        *children = spliced;

        if children.len() == 1 {
            let only = children.remove(0);
            *self = only.node;
        }
    }

    /// Test-only invariant checks (`docs/recursive-layout-design.md`'s
    /// shallow-nesting invariant): no `Split` has a single child, and no
    /// `Split` child shares its parent's axis.
    #[cfg(test)]
    pub fn is_canonical(&self) -> bool {
        self.is_canonical_under(None)
    }

    #[cfg(test)]
    fn is_canonical_under(&self, parent_axis: Option<SplitAxis>) -> bool {
        match self {
            Self::Pane(_) => true,
            Self::Split { axis, children } => {
                if children.len() < 2 || parent_axis == Some(*axis) {
                    return false;
                }
                children
                    .iter()
                    .all(|child| child.node.is_canonical_under(Some(*axis)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneKind, SessionId};

    #[test]
    fn set_split_weights_normalizes_view_sizes() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        let tab_id = workspace.active_tab;
        let root = &workspace.tabs[0].root;
        let anchors: Vec<_> = match root {
            LayoutNode::Split { children, .. } => children
                .iter()
                .map(|child| child.node.first_pane().expect("pane"))
                .collect(),
            LayoutNode::Pane(_) => panic!("expected split"),
        };

        assert!(workspace.set_split_weights(tab_id, anchors[0], &anchors, &[300.0, 100.0]));
        let weights: Vec<_> = match &workspace.tabs[0].root {
            LayoutNode::Split { children, .. } => {
                children.iter().map(|child| child.weight).collect()
            }
            LayoutNode::Pane(_) => panic!("expected split"),
        };
        assert_eq!(weights, vec![0.75, 0.25]);
    }

    #[test]
    fn set_split_weights_uses_child_anchors_to_disambiguate_nested_splits() {
        let mut workspace = Workspace::mvp();
        let first = workspace.active_tab().expect("tab").active;
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        workspace.activate_pane(first);
        workspace.split_session_with_new_session(
            workspace.active_session_id().expect("session"),
            PaneKind::Agent,
            SplitAxis::Vertical,
            true,
        );
        let tab_id = workspace.active_tab;
        let root = &workspace.tabs[0].root;
        let (nested_anchor, nested_children) = match root {
            LayoutNode::Split { children, .. } => match &children[0].node {
                LayoutNode::Split { children, .. } => (
                    children[0].node.first_pane().expect("anchor"),
                    children
                        .iter()
                        .map(|child| child.node.first_pane().expect("child anchor"))
                        .collect::<Vec<_>>(),
                ),
                LayoutNode::Pane(_) => panic!("expected nested split"),
            },
            LayoutNode::Pane(_) => panic!("expected root split"),
        };

        assert!(workspace.set_split_weights(tab_id, nested_anchor, &nested_children, &[1.0, 3.0]));
        let nested_weights = match &workspace.tabs[0].root {
            LayoutNode::Split { children, .. } => match &children[0].node {
                LayoutNode::Split { children, .. } => children
                    .iter()
                    .map(|child| child.weight)
                    .collect::<Vec<_>>(),
                LayoutNode::Pane(_) => panic!("expected nested split"),
            },
            LayoutNode::Pane(_) => panic!("expected root split"),
        };
        assert_eq!(nested_weights, vec![0.25, 0.75]);
    }

    #[test]
    fn set_split_weights_rejects_invalid_measurements_without_mutating() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        let tab_id = workspace.active_tab;
        let anchors = workspace.tabs[0].root.pane_ids();
        let before = weights(&workspace.tabs[0].root);
        assert!(!workspace.set_split_weights(tab_id, anchors[0], &anchors, &[1.0]));
        assert!(!workspace.set_split_weights(tab_id, anchors[0], &anchors, &[f32::NAN, 1.0]));
        assert_eq!(weights(&workspace.tabs[0].root), before);
    }

    fn weights(node: &LayoutNode) -> Vec<f32> {
        match node {
            LayoutNode::Pane(_) => Vec::new(),
            LayoutNode::Split { children, .. } => {
                children.iter().map(|child| child.weight).collect()
            }
        }
    }

    #[test]
    fn splitting_a_bare_root_wraps_it() {
        let a = PaneId::new();
        let b = PaneId::new();
        let mut root = LayoutNode::Pane(a);

        assert!(root.split_pane(a, b, SplitAxis::Horizontal));

        assert_eq!(root.pane_ids(), vec![a, b]);
        assert!(root.is_canonical());
        assert_eq!(weights(&root), vec![1.0, 1.0]);
    }

    #[test]
    fn a_row_of_panes_stays_at_depth_one() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Pane(a);

        assert!(root.split_pane(a, b, SplitAxis::Horizontal));
        // b's parent is already a Horizontal split -- this must absorb, not
        // wrap b in a new nested Split.
        assert!(root.split_pane(b, c, SplitAxis::Horizontal));

        assert_eq!(root.pane_ids(), vec![a, b, c]);
        assert!(root.is_canonical());
        match &root {
            LayoutNode::Split { children, .. } => assert_eq!(children.len(), 3),
            LayoutNode::Pane(_) => panic!("expected a single flat Split"),
        }
    }

    #[test]
    fn absorbed_sibling_takes_the_targets_weight() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        // Give `a` a distinct weight so the absorb path's weight source is
        // unambiguous -- it must copy `a`'s weight, not `b`'s or a fresh
        // default.
        if let LayoutNode::Split { children, .. } = &mut root {
            children[0].weight = 3.0;
        }

        assert!(root.split_pane(a, c, SplitAxis::Horizontal));

        // Inserted immediately after the target (`a`), not appended at the
        // end of the container.
        assert_eq!(root.pane_ids(), vec![a, c, b]);
        assert_eq!(weights(&root), vec![3.0, 3.0, 1.0]);
    }

    #[test]
    fn a_vertical_split_nests_only_at_the_crossing() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let d = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        root.split_pane(b, c, SplitAxis::Horizontal);

        // b's parent is Horizontal; splitting b vertically must wrap just
        // b, not touch the row's other siblings.
        assert!(root.split_pane(b, d, SplitAxis::Vertical));

        assert_eq!(root.pane_ids(), vec![a, b, d, c]);
        assert!(root.is_canonical());
        match &root {
            LayoutNode::Split {
                axis: SplitAxis::Horizontal,
                children,
            } => {
                assert_eq!(children.len(), 3);
                match &children[1].node {
                    LayoutNode::Split {
                        axis: SplitAxis::Vertical,
                        children,
                    } => {
                        assert_eq!(children.len(), 2);
                    }
                    other => panic!("expected b wrapped in a Vertical split, got {other:?}"),
                }
            }
            other => panic!("expected the row to stay a Horizontal split, got {other:?}"),
        }
    }

    #[test]
    fn split_pane_on_an_unknown_target_is_a_no_op() {
        let a = PaneId::new();
        let unknown = PaneId::new();
        let new_pane = PaneId::new();
        let mut root = LayoutNode::Pane(a);

        assert!(!root.split_pane(unknown, new_pane, SplitAxis::Horizontal));
        assert_eq!(root.pane_ids(), vec![a]);
    }

    #[test]
    fn without_pane_collapses_a_two_child_split_to_the_remaining_leaf() {
        let a = PaneId::new();
        let b = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);

        let result = root.without_pane(b).expect("a remains");

        assert!(matches!(result, LayoutNode::Pane(id) if id == a));
    }

    #[test]
    fn without_pane_removing_the_last_pane_yields_none() {
        let a = PaneId::new();
        let root = LayoutNode::Pane(a);

        assert!(root.without_pane(a).is_none());
    }

    #[test]
    fn without_pane_shrinks_a_row_without_denesting() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        root.split_pane(b, c, SplitAxis::Horizontal);

        let result = root.without_pane(b).expect("a and c remain");

        assert_eq!(result.pane_ids(), vec![a, c]);
        assert!(result.is_canonical());
    }

    #[test]
    fn flatten_splices_a_same_axis_child_and_rescales_weights() {
        // Deliberately construct a tree that violates the invariant --
        // `split_pane`/`without_pane` never produce this shape themselves,
        // but `flatten` must still repair it defensively.
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(a),
                    weight: 3.0,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Horizontal,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(b),
                                weight: 1.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(c),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 4.0,
                },
            ],
        };

        root.flatten();

        assert_eq!(root.pane_ids(), vec![a, b, c]);
        assert!(root.is_canonical());
        assert_eq!(weights(&root), vec![3.0, 2.0, 2.0]);
    }

    #[test]
    fn flatten_collapses_a_single_child_split() {
        let a = PaneId::new();
        let b = PaneId::new();
        let mut root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![LayoutChild {
                node: LayoutNode::Split {
                    axis: SplitAxis::Vertical,
                    children: vec![
                        LayoutChild {
                            node: LayoutNode::Pane(a),
                            weight: 1.0,
                        },
                        LayoutChild {
                            node: LayoutNode::Pane(b),
                            weight: 1.0,
                        },
                    ],
                },
                weight: 1.0,
            }],
        };

        root.flatten();

        assert!(matches!(
            &root,
            LayoutNode::Split { axis: SplitAxis::Vertical, children } if children.len() == 2
        ));
        assert_eq!(root.pane_ids(), vec![a, b]);
    }

    #[test]
    fn is_canonical_rejects_a_single_child_split() {
        let a = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![LayoutChild {
                node: LayoutNode::Pane(a),
                weight: 1.0,
            }],
        };

        assert!(!root.is_canonical());
    }

    #[test]
    fn is_canonical_rejects_same_axis_nesting() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(a),
                    weight: 1.0,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Horizontal,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(b),
                                weight: 1.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(c),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 1.0,
                },
            ],
        };

        assert!(!root.is_canonical());
    }

    #[test]
    fn split_render_plan_is_none_for_a_bare_leaf() {
        let a = PaneId::new();
        let root = LayoutNode::Pane(a);

        assert_eq!(split_render_plan(&root), None);
    }

    #[test]
    fn split_render_plan_reports_axis_and_leaf_anchors_with_weights() {
        let a = PaneId::new();
        let b = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        if let LayoutNode::Split { children, .. } = &mut root {
            children[0].weight = 3.0;
        }

        let (axis, plan) = split_render_plan(&root).expect("root is a Split");

        assert_eq!(axis, SplitAxis::Horizontal);
        assert_eq!(
            plan,
            vec![
                RenderChild {
                    anchor: a,
                    weight: 3.0
                },
                RenderChild {
                    anchor: b,
                    weight: 1.0
                },
            ]
        );
    }

    #[test]
    fn split_render_plan_anchors_a_nested_subtree_by_its_leftmost_leaf() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        root.split_pane(b, c, SplitAxis::Horizontal);
        // Wrap `b` in a Vertical split with a new pane `d` -- `b`'s slot in
        // the top-level row becomes a subtree, anchored by its own leftmost
        // leaf (still `b`, since it's the wrapped pair's first child).
        let d = PaneId::new();
        root.split_pane(b, d, SplitAxis::Vertical);

        let (axis, plan) = split_render_plan(&root).expect("root is a Split");

        assert_eq!(axis, SplitAxis::Horizontal);
        assert_eq!(
            plan.iter().map(|child| child.anchor).collect::<Vec<_>>(),
            vec![a, b, c],
            "the nested subtree's anchor is its own leftmost leaf (b), not d"
        );
    }

    #[test]
    fn first_pane_descends_to_the_leftmost_leaf() {
        let a = PaneId::new();
        let b = PaneId::new();
        let c = PaneId::new();
        let mut root = LayoutNode::Pane(a);
        root.split_pane(a, b, SplitAxis::Horizontal);
        root.split_pane(a, c, SplitAxis::Vertical);

        assert_eq!(root.first_pane(), Some(a));
    }
}
