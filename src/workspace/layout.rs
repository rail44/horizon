use super::types::{LayoutChild, LayoutNode, PaneId, SplitAxis};

impl LayoutNode {
    pub(super) fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|child| child.node.pane_ids())
                .collect(),
        }
    }

    pub(super) fn first_pane(&self) -> Option<PaneId> {
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
    pub(super) fn without_pane(&self, pane_id: PaneId) -> Option<Self> {
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
    pub(super) fn split_pane(
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
    pub(super) fn flatten(&mut self) {
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
    pub(super) fn is_canonical(&self) -> bool {
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
