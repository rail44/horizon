use super::PaneId;

/// A tab's layout tree: an N-ary tiling tree (see
/// `docs/recursive-layout-design.md`). `Split` holds a variable-length list
/// of weighted children rather than a fixed pair, so a row of N panes stays
/// at depth 1 -- nesting appears only where a horizontal and a vertical
/// split cross. The tree is kept in canonical form (no single-child
/// `Split`, no child `Split` sharing its parent's axis) by the layout
/// operations in `super::layout`, not by this type itself.
#[derive(Clone, Debug)]
pub(crate) enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: Vec<LayoutChild>,
    },
}

/// One child of a `Split`, weighted like a flex-grow factor: siblings
/// divide their container's extent proportionally to their own weight
/// against the sum of all siblings' weights (no constraint solver -- see
/// `docs/recursive-layout-design.md`'s Sizing decision).
#[derive(Clone, Debug)]
pub(crate) struct LayoutChild {
    pub(crate) node: LayoutNode,
    pub(crate) weight: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitAxis {
    Horizontal,
    // Not constructed by any production caller yet -- every split site
    // still passes `Horizontal` (docs/recursive-layout-design.md's slice
    // 2 exposes a vertical placement verb in the UI). The layout tree and
    // its operations are already vertical-capable and exercised by
    // `layout::tests`.
    #[allow(dead_code)]
    Vertical,
}
