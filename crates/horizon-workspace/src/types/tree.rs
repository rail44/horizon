use super::{PaneId, TabGroupId, TabId};

/// The workspace's tree (see `docs/workspace-layout-design.md`): panes,
/// splits, and tab groups, each tab of a group a tree of its own. A split
/// holds a variable-length list of weighted children rather than a fixed
/// pair, so a row of N panes stays at depth 1 -- nesting appears only
/// where a horizontal and a vertical split cross, or where a tab group
/// is (`docs/recursive-layout-design.md`). The tree is kept in canonical
/// form by the layout operations in `super::layout`, not by these types.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: Vec<LayoutChild>,
    },
    Tabs(TabGroup),
}

/// One child of a `Split`, weighted like a flex-grow factor: siblings
/// divide their container's extent proportionally to their own weight
/// against the sum of all siblings' weights (no constraint solver -- see
/// `docs/recursive-layout-design.md`'s Sizing decision).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutChild {
    pub node: LayoutNode,
    pub weight: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// Tabs, one of them shown. Only the root may have none: the empty
/// workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct TabGroup {
    pub id: TabGroupId,
    pub tabs: Vec<Tab>,
    pub shown: TabId,
}

/// A tab: a tree of its own, and the pane in it focus goes to when the
/// tab is shown again.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub root: LayoutNode,
    pub active: PaneId,
}

/// A node of the tree by what stays the same about it: a pane by its
/// id, a tab group by its id. A split is named by its first child's key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NodeKey {
    Pane(PaneId),
    Group(TabGroupId),
}
