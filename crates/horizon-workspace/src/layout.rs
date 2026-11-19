use super::types::{
    LayoutChild, LayoutNode, NodeKey, PaneId, SplitAxis, Tab, TabGroup, TabGroupId, TabId,
    Workspace,
};

/// One child in a `Split`'s rendering plan (see [`split_render_plan`]):
/// `anchor` is the stable identity a recursive renderer would key that
/// child's own nested view by across re-renders. Test-only: a description
/// of the tree's rendering shape, decoupled from any UI framework.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderChild {
    pub(crate) anchor: NodeKey,
    pub(crate) weight: f32,
}

/// The immediate rendering plan for one `Split` level -- its axis and, per
/// child, the identity/weight pair a renderer would need to key and size
/// that child's slot. `None` for anything but a split.
#[cfg(test)]
pub(crate) fn split_render_plan(node: &LayoutNode) -> Option<(SplitAxis, Vec<RenderChild>)> {
    match node {
        LayoutNode::Split { axis, children } => Some((
            *axis,
            children
                .iter()
                .map(|child| RenderChild {
                    anchor: child.node.first_key(),
                    weight: child.weight,
                })
                .collect(),
        )),
        _ => None,
    }
}

impl Workspace {
    /// Updates one rendered split from pixel sizes reported by the view.
    /// `split_anchor` plus the immediate child anchors identifies the split:
    /// a first-child anchor alone is insufficient because the first nested
    /// split shares that anchor with each of its ancestors.
    pub fn set_split_weights(
        &mut self,
        split_anchor: NodeKey,
        child_anchors: &[NodeKey],
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
        self.root
            .set_split_weights(split_anchor, child_anchors, sizes, total)
    }
}

impl TabGroup {
    /// The tab the group shows, if it has any.
    pub fn shown_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == self.shown)
    }

    /// The index of the tab the group shows.
    pub fn shown_index(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == self.shown)
    }

    /// A group whose one tab is `root`, with `active` its active pane.
    pub(crate) fn of(root: LayoutNode, active: PaneId) -> Self {
        let tab = Tab {
            id: TabId::new(),
            root,
            active,
        };
        Self {
            id: TabGroupId::new(),
            shown: tab.id,
            tabs: vec![tab],
        }
    }

    /// The root group of an empty workspace.
    pub(crate) fn empty() -> Self {
        Self {
            id: TabGroupId::new(),
            tabs: Vec::new(),
            shown: TabId::new(),
        }
    }

    /// Shows the tab at `index` beside the one that was there when the
    /// shown tab is gone: the one that took its place, or the last.
    fn show_neighbor_of(&mut self, index: usize) {
        if let Some(tab) = self.tabs.get(index.min(self.tabs.len().saturating_sub(1))) {
            self.shown = tab.id;
        }
    }
}

impl LayoutNode {
    fn set_split_weights(
        &mut self,
        split_anchor: NodeKey,
        expected_child_anchors: &[NodeKey],
        sizes: &[f32],
        total: f32,
    ) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Tabs(group) => group.tabs.iter_mut().any(|tab| {
                tab.root
                    .set_split_weights(split_anchor, expected_child_anchors, sizes, total)
            }),
            Self::Split { children, .. } => {
                let own_anchor = children.first().map(|child| child.node.first_key());
                let child_anchors: Vec<_> = children
                    .iter()
                    .map(|child| child.node.first_key())
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
        }
    }

    /// The key this node is known by: a pane's, a group's, or a split's
    /// first child's.
    pub fn first_key(&self) -> NodeKey {
        match self {
            Self::Pane(pane_id) => NodeKey::Pane(*pane_id),
            Self::Tabs(group) => NodeKey::Group(group.id),
            Self::Split { children, .. } => children
                .first()
                .map(|child| child.node.first_key())
                .expect("a canonical split has children"),
        }
    }

    /// Every pane in this subtree, every tab of every group included, in
    /// pre-order.
    pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|child| child.node.pane_ids())
                .collect(),
            Self::Tabs(group) => group
                .tabs
                .iter()
                .flat_map(|tab| tab.root.pane_ids())
                .collect(),
        }
    }

    /// The panes on screen: each group's shown tab's only, in pre-order.
    pub(crate) fn visible_pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|child| child.node.visible_pane_ids())
                .collect(),
            Self::Tabs(group) => group
                .shown_tab()
                .map(|tab| tab.root.visible_pane_ids())
                .unwrap_or_default(),
        }
    }

    /// The first pane on screen in this subtree.
    pub fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Pane(pane_id) => Some(*pane_id),
            Self::Split { children, .. } => {
                children.iter().find_map(|child| child.node.first_pane())
            }
            Self::Tabs(group) => group.shown_tab().and_then(|tab| tab.root.first_pane()),
        }
    }

    /// Every tab group in this subtree, those in tabs not shown included,
    /// in pre-order.
    pub(crate) fn groups(&self) -> Vec<&TabGroup> {
        let mut found = Vec::new();
        self.collect_groups(&mut found);
        found
    }

    fn collect_groups<'a>(&'a self, found: &mut Vec<&'a TabGroup>) {
        match self {
            Self::Pane(_) => {}
            Self::Split { children, .. } => {
                for child in children {
                    child.node.collect_groups(found);
                }
            }
            Self::Tabs(group) => {
                found.push(group);
                for tab in &group.tabs {
                    tab.root.collect_groups(found);
                }
            }
        }
    }

    /// The tab groups on screen, in pre-order.
    pub(crate) fn visible_groups(&self) -> Vec<&TabGroup> {
        match self {
            Self::Pane(_) => Vec::new(),
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|child| child.node.visible_groups())
                .collect(),
            Self::Tabs(group) => std::iter::once(group)
                .chain(
                    group
                        .shown_tab()
                        .into_iter()
                        .flat_map(|tab| tab.root.visible_groups()),
                )
                .collect(),
        }
    }

    pub(crate) fn group(&self, id: TabGroupId) -> Option<&TabGroup> {
        self.groups().into_iter().find(|group| group.id == id)
    }

    pub(crate) fn group_mut(&mut self, id: TabGroupId) -> Option<&mut TabGroup> {
        match self {
            Self::Pane(_) => None,
            Self::Split { children, .. } => children
                .iter_mut()
                .find_map(|child| child.node.group_mut(id)),
            Self::Tabs(group) => {
                if group.id == id {
                    return Some(group);
                }
                group.tabs.iter_mut().find_map(|tab| tab.root.group_mut(id))
            }
        }
    }

    /// The groups and tabs `pane_id` is under, outermost first. Empty for
    /// a pane under no group, or one not in this subtree.
    pub(crate) fn tabs_around(&self, pane_id: PaneId) -> Vec<(TabGroupId, TabId)> {
        match self {
            Self::Pane(_) => Vec::new(),
            Self::Split { children, .. } => children
                .iter()
                .map(|child| child.node.tabs_around(pane_id))
                .find(|path| !path.is_empty())
                .unwrap_or_default(),
            Self::Tabs(group) => group
                .tabs
                .iter()
                .find(|tab| tab.root.pane_ids().contains(&pane_id))
                .map(|tab| {
                    let mut path = vec![(group.id, tab.id)];
                    path.extend(tab.root.tabs_around(pane_id));
                    path
                })
                .unwrap_or_default(),
        }
    }

    /// The tab `id`, wherever it is.
    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        match self {
            Self::Pane(_) => None,
            Self::Split { children, .. } => {
                children.iter_mut().find_map(|child| child.node.tab_mut(id))
            }
            Self::Tabs(group) => {
                if let Some(index) = group.tabs.iter().position(|tab| tab.id == id) {
                    return group.tabs.get_mut(index);
                }
                group.tabs.iter_mut().find_map(|tab| tab.root.tab_mut(id))
            }
        }
    }

    pub(crate) fn tab(&self, id: TabId) -> Option<&Tab> {
        match self {
            Self::Pane(_) => None,
            Self::Split { children, .. } => children.iter().find_map(|child| child.node.tab(id)),
            Self::Tabs(group) => group
                .tabs
                .iter()
                .find(|tab| tab.id == id)
                .or_else(|| group.tabs.iter().find_map(|tab| tab.root.tab(id))),
        }
    }

    /// Removes `pane_id`, collapsing any `Split` left with a single child
    /// into that child directly and `None` once a `Split` loses all of its
    /// children. A tab left with no pane is closed, and a group left with
    /// no tab is `None` too: the caller decides what an empty root is.
    /// Does not by itself re-splice a same-axis child into its parent --
    /// callers apply `flatten` after this for that.
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
            Self::Tabs(group) => {
                let shown_index = group.shown_index();
                let mut kept = Vec::with_capacity(group.tabs.len());
                for tab in &group.tabs {
                    let Some(root) = tab.root.without_pane(pane_id) else {
                        continue;
                    };
                    let active = match root.pane_ids().contains(&tab.active) {
                        true => tab.active,
                        false => root.first_pane().unwrap_or(tab.active),
                    };
                    kept.push(Tab {
                        id: tab.id,
                        root,
                        active,
                    });
                }
                if kept.is_empty() {
                    return None;
                }
                let mut group = TabGroup {
                    id: group.id,
                    tabs: kept,
                    shown: group.shown,
                };
                if group.shown_tab().is_none() {
                    group.show_neighbor_of(shown_index.unwrap_or(0));
                }
                Some(Self::Tabs(group))
            }
        }
    }

    /// Closes the tab `id` wherever it is, with every pane in it; the
    /// group shows the tab that took its place. A group left with no tab
    /// is `None`, as in [`Self::without_pane`].
    pub(crate) fn without_tab(&self, id: TabId) -> Option<Self> {
        match self {
            Self::Pane(pane_id) => Some(Self::Pane(*pane_id)),
            Self::Split { axis, children } => {
                let remaining: Vec<LayoutChild> = children
                    .iter()
                    .filter_map(|child| {
                        child.node.without_tab(id).map(|node| LayoutChild {
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
            Self::Tabs(group) => {
                let closed = group.tabs.iter().position(|tab| tab.id == id);
                let mut tabs = Vec::with_capacity(group.tabs.len());
                for tab in &group.tabs {
                    if tab.id == id {
                        continue;
                    }
                    let Some(root) = tab.root.without_tab(id) else {
                        continue;
                    };
                    let active = match root.pane_ids().contains(&tab.active) {
                        true => tab.active,
                        false => root.first_pane().unwrap_or(tab.active),
                    };
                    tabs.push(Tab {
                        id: tab.id,
                        root,
                        active,
                    });
                }
                if tabs.is_empty() {
                    return None;
                }
                let mut group = TabGroup {
                    id: group.id,
                    tabs,
                    shown: group.shown,
                };
                if group.shown_tab().is_none() {
                    group.show_neighbor_of(closed.unwrap_or(0));
                }
                Some(Self::Tabs(group))
            }
        }
    }

    /// Whether this node is the one `key` names.
    fn is(&self, key: NodeKey) -> bool {
        match (self, key) {
            (Self::Pane(id), NodeKey::Pane(wanted)) => *id == wanted,
            (Self::Tabs(group), NodeKey::Group(wanted)) => group.id == wanted,
            _ => false,
        }
    }

    /// Puts `new` beside the node `target` names, along `axis`, before it
    /// or after it -- the shallow-nesting invariant's insert mechanism
    /// (`docs/recursive-layout-design.md`): absorbs into the target's parent
    /// split when it already has this axis (no new node), otherwise wraps
    /// the target in a new split in its place (the only place depth grows).
    /// The new node takes the target's weight when absorbed; both are
    /// weighted equally (1.0) when wrapped. `false`, a no-op, when no node
    /// here is `target`. A target in a tab is placed within that tab.
    pub(crate) fn split_beside(
        &mut self,
        target: NodeKey,
        new: LayoutNode,
        axis: SplitAxis,
        before: bool,
    ) -> bool {
        if self.is(target) {
            let old = std::mem::replace(self, Self::Pane(PaneId::new()));
            let (first, second) = match before {
                true => (new, old),
                false => (old, new),
            };
            *self = Self::Split {
                axis,
                children: vec![
                    LayoutChild {
                        node: first,
                        weight: 1.0,
                    },
                    LayoutChild {
                        node: second,
                        weight: 1.0,
                    },
                ],
            };
            return true;
        }
        match self {
            Self::Pane(_) => false,
            Self::Tabs(group) => {
                // (Each tab is a tree of its own; `new` goes to one of them.)
                let mut new = Some(new);
                for tab in &mut group.tabs {
                    if tab.root.contains(target) {
                        return tab.root.split_beside(
                            target,
                            new.take().expect("placed once"),
                            axis,
                            before,
                        );
                    }
                }
                false
            }
            Self::Split {
                axis: node_axis,
                children,
            } => {
                if let Some(index) = children.iter().position(|child| child.node.is(target)) {
                    if *node_axis == axis {
                        let weight = children[index].weight;
                        let at = if before { index } else { index + 1 };
                        children.insert(at, LayoutChild { node: new, weight });
                        return true;
                    }
                    return children[index].node.split_beside(target, new, axis, before);
                }
                match children
                    .iter()
                    .position(|child| child.node.contains(target))
                {
                    Some(index) => children[index].node.split_beside(target, new, axis, before),
                    None => false,
                }
            }
        }
    }

    /// Whether the node `key` names is in this subtree.
    pub(crate) fn contains(&self, key: NodeKey) -> bool {
        if self.is(key) {
            return true;
        }
        match self {
            Self::Pane(_) => false,
            Self::Split { children, .. } => children.iter().any(|child| child.node.contains(key)),
            Self::Tabs(group) => group.tabs.iter().any(|tab| tab.root.contains(key)),
        }
    }

    /// Splits `target`'s pane in `axis`, putting `new_pane_id` after it.
    #[cfg(test)]
    pub(crate) fn split_pane(
        &mut self,
        target: PaneId,
        new_pane_id: PaneId,
        axis: SplitAxis,
    ) -> bool {
        self.split_beside(NodeKey::Pane(target), Self::Pane(new_pane_id), axis, false)
    }

    /// Puts `group` in place of the pane `pane_id`. `false` when the pane
    /// is not here.
    pub(crate) fn replace_pane(&mut self, pane_id: PaneId, group: TabGroup) -> bool {
        if matches!(self, Self::Pane(id) if *id == pane_id) {
            *self = Self::Tabs(group);
            return true;
        }
        match self {
            Self::Pane(_) => false,
            Self::Split { children, .. } => children
                .iter_mut()
                .any(|child| child.node.replace_pane(pane_id, group.clone())),
            Self::Tabs(tabs) => tabs
                .tabs
                .iter_mut()
                .any(|tab| tab.root.replace_pane(pane_id, group.clone())),
        }
    }

    /// Normalizes the tree to the shallow-nesting invariant (`docs/
    /// recursive-layout-design.md`): no `Split` child shares its parent's
    /// axis (spliced into the parent, weights rescaled to preserve the
    /// child's overall share) and no `Split` has a single child (replaced
    /// by that child). Each tab's tree is normalized on its own; a group is
    /// never spliced into anything.
    pub fn flatten(&mut self) {
        let (axis, children) = match self {
            Self::Pane(_) => return,
            Self::Tabs(group) => {
                for tab in &mut group.tabs {
                    tab.root.flatten();
                }
                return;
            }
            Self::Split { axis, children } => (axis, children),
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

    /// Test-only invariant checks: no `Split` has a single child, no
    /// `Split` child shares its parent's axis, and no group below the root
    /// is without a tab.
    #[cfg(test)]
    pub fn is_canonical(&self) -> bool {
        self.is_canonical_under(None, true)
    }

    #[cfg(test)]
    fn is_canonical_under(&self, parent_axis: Option<SplitAxis>, root: bool) -> bool {
        match self {
            Self::Pane(_) => true,
            Self::Tabs(group) => {
                (root || !group.tabs.is_empty())
                    && group
                        .tabs
                        .iter()
                        .all(|tab| tab.root.is_canonical_under(None, false))
            }
            Self::Split { axis, children } => {
                if children.len() < 2 || parent_axis == Some(*axis) {
                    return false;
                }
                children
                    .iter()
                    .all(|child| child.node.is_canonical_under(Some(*axis), false))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionId, SessionKind};

    /// The shown tab's tree of the workspace's one group.
    fn tree(workspace: &Workspace) -> &LayoutNode {
        &workspace.active_tab().expect("a tab").root
    }

    fn keys(node: &LayoutNode) -> Vec<NodeKey> {
        match node {
            LayoutNode::Split { children, .. } => children
                .iter()
                .map(|child| child.node.first_key())
                .collect(),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn set_split_weights_normalizes_view_sizes() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        let anchors = keys(tree(&workspace));

        assert!(workspace.set_split_weights(anchors[0], &anchors, &[300.0, 100.0]));
        assert_eq!(weights(tree(&workspace)), vec![0.75, 0.25]);
    }

    #[test]
    fn set_split_weights_uses_child_anchors_to_disambiguate_nested_splits() {
        let mut workspace = Workspace::mvp();
        let first = workspace.active_tab().expect("tab").active;
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        workspace.activate_pane(first);
        workspace.split_session_with_new_session(
            workspace.active_session_id().expect("session"),
            crate::SessionKind::Agent,
            SplitAxis::Vertical,
            true,
        );
        let nested = match tree(&workspace) {
            LayoutNode::Split { children, .. } => children[0].node.clone(),
            _ => panic!("expected root split"),
        };
        let nested_children = keys(&nested);

        assert!(workspace.set_split_weights(nested_children[0], &nested_children, &[1.0, 3.0]));
        let nested_weights = match tree(&workspace) {
            LayoutNode::Split { children, .. } => weights(&children[0].node),
            _ => panic!("expected root split"),
        };
        assert_eq!(nested_weights, vec![0.25, 0.75]);
    }

    #[test]
    fn set_split_weights_rejects_invalid_measurements_without_mutating() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        let anchors = keys(tree(&workspace));
        let before = weights(tree(&workspace));
        assert!(!workspace.set_split_weights(anchors[0], &anchors, &[1.0]));
        assert!(!workspace.set_split_weights(anchors[0], &anchors, &[f32::NAN, 1.0]));
        assert_eq!(weights(tree(&workspace)), before);
    }

    fn weights(node: &LayoutNode) -> Vec<f32> {
        match node {
            LayoutNode::Split { children, .. } => {
                children.iter().map(|child| child.weight).collect()
            }
            _ => Vec::new(),
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
            _ => panic!("expected a single flat Split"),
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

        std::assert_matches!(result, LayoutNode::Pane(id) if id == a);
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

        std::assert_matches!(
            &root,
            LayoutNode::Split { axis: SplitAxis::Vertical, children } if children.len() == 2);
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
                    anchor: NodeKey::Pane(a),
                    weight: 3.0
                },
                RenderChild {
                    anchor: NodeKey::Pane(b),
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
            [a, b, c].map(NodeKey::Pane).to_vec(),
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
