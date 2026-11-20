//! Reordering the workspace's views (`docs/workspace-layout-design.md`):
//! two of them swapped, each in the other's place, and a pane moved into
//! a new tab. A swap leaves the tree's shape as it was; what changes its
//! shape is a split, or a pane moved into a tab.

use std::collections::HashMap;

use super::mode::{Cursor, Direction};
use super::nav::{layout_rects, nearest_in_direction};
use super::types::{LayoutNode, NodeKey, PaneId, SplitAxis, Tab, TabGroupId, TabId, Workspace};

impl LayoutNode {
    /// The node `key` names, if one here does.
    fn node(&self, key: NodeKey) -> Option<&Self> {
        if self.is(key) {
            return Some(self);
        }
        match self {
            Self::Pane(_) => None,
            Self::Split { children, .. } => children.iter().find_map(|child| child.node.node(key)),
            Self::Tabs(group) => group.tabs.iter().find_map(|tab| tab.root.node(key)),
        }
    }

    /// Puts `new` in place of the node `key` names. `false` when none here
    /// is.
    fn put(&mut self, key: NodeKey, new: Self) -> bool {
        if self.is(key) {
            *self = new;
            return true;
        }
        match self {
            Self::Pane(_) => false,
            Self::Split { children, .. } => children
                .iter_mut()
                .any(|child| child.node.put(key, new.clone())),
            Self::Tabs(group) => group
                .tabs
                .iter_mut()
                .any(|tab| tab.root.put(key, new.clone())),
        }
    }

    /// The pane that stands for this node where one tab's active pane was:
    /// for a group, the active pane of the tab it shows.
    fn representative(&self) -> Option<PaneId> {
        match self {
            Self::Pane(pane) => Some(*pane),
            Self::Split { .. } => self.first_pane(),
            Self::Tabs(group) => group.shown_tab().map(|tab| tab.active),
        }
    }

    /// Calls `each` on every tab here, however deep.
    fn each_tab_mut(&mut self, each: &mut impl FnMut(&mut Tab)) {
        match self {
            Self::Pane(_) => {}
            Self::Split { children, .. } => {
                for child in children {
                    child.node.each_tab_mut(each);
                }
            }
            Self::Tabs(group) => {
                for tab in &mut group.tabs {
                    each(tab);
                    tab.root.each_tab_mut(each);
                }
            }
        }
    }
}

/// A tab the pane the cursor is on can be moved to.
#[derive(Clone, Debug, PartialEq)]
pub struct Destination {
    pub tab: TabId,
    pub title: String,
    /// The place of the tab's group among the groups on screen.
    pub group: usize,
}

impl LayoutNode {
    /// The group the tab `tab` is in, if one here has it.
    fn group_with_tab(&self, tab: TabId) -> Option<TabGroupId> {
        match self {
            Self::Pane(_) => None,
            Self::Split { children, .. } => {
                (children.iter()).find_map(|child| child.node.group_with_tab(tab))
            }
            Self::Tabs(group) => match group.tabs.iter().any(|of| of.id == tab) {
                true => Some(group.id),
                false => group.tabs.iter().find_map(|of| of.root.group_with_tab(tab)),
            },
        }
    }
}

impl Cursor {
    /// The node the cursor is on: a pane, or a group as a whole.
    pub(crate) fn key(self) -> NodeKey {
        match self {
            Self::Pane(pane) => NodeKey::Pane(pane),
            Self::Strip(group) => NodeKey::Group(group),
        }
    }
}

impl Workspace {
    /// Puts the view `a` names -- a pane, or a tab group as a whole -- in
    /// the place of the one `b` names, and that one in `a`'s. A group goes
    /// in no tab of another group: one swapped with what is in a group is
    /// swapped with that group as a whole. `false`, a no-op, when either is
    /// not in the tree, they are the same, or one is in the other (a pane
    /// and the group it is in). Focus, and the workspace-mode cursor, stay
    /// on what they were on, which has moved.
    pub fn swap_nodes(&mut self, a: NodeKey, b: NodeKey) -> bool {
        let (a, b) = match (a, b) {
            (NodeKey::Group(_), _) => (a, self.outermost(b)),
            (_, NodeKey::Group(_)) => (self.outermost(a), b),
            _ => (a, b),
        };
        if a == b {
            return false;
        }
        let (Some(first), Some(second)) = (self.root.node(a).cloned(), self.root.node(b).cloned())
        else {
            return false;
        };
        if first.contains(b) || second.contains(a) {
            return false;
        }
        // A tab whose active pane moved away has the one that took its
        // place active instead.
        let mut took_place = HashMap::new();
        if let (NodeKey::Pane(moved), Some(instead)) = (a, second.representative()) {
            took_place.insert(moved, instead);
        }
        if let (NodeKey::Pane(moved), Some(instead)) = (b, first.representative()) {
            took_place.insert(moved, instead);
        }
        let held = NodeKey::Pane(PaneId::new());
        let NodeKey::Pane(holder) = held else {
            unreachable!("a pane's key");
        };
        self.root.put(a, LayoutNode::Pane(holder));
        self.root.put(b, first);
        self.root.put(held, second);
        self.root.each_tab_mut(&mut |tab| {
            if let Some(instead) = took_place.get(&tab.active) {
                tab.active = *instead;
            }
            if !tab.root.pane_ids().contains(&tab.active)
                && let Some(first) = tab.root.first_pane()
            {
                tab.active = first;
            }
        });
        if let Some(focus) = self.focus {
            self.note_focus(focus);
        }
        true
    }

    /// The outermost group `key` is in, as a whole; `key` itself where it
    /// is in none.
    fn outermost(&self, key: NodeKey) -> NodeKey {
        let NodeKey::Pane(pane) = key else {
            return key;
        };
        match self.root.tabs_around(pane).first() {
            Some((group, _)) => NodeKey::Group(*group),
            None => key,
        }
    }

    /// Shift and a direction in workspace mode: swaps what the cursor is on
    /// with what the cursor would move to in `direction`
    /// ([`Self::move_cursor`]). The cursor stays on what it was on, so it
    /// goes with it. `false` at an edge, or where the one is in the other.
    pub fn swap_cursor(&mut self, direction: Direction) -> bool {
        let Some(current) = self.workspace_mode.cursor() else {
            return false;
        };
        let rects = layout_rects(&self.root);
        let Some(next) = nearest_in_direction(&rects, current, direction) else {
            return false;
        };
        self.swap_nodes(current.key(), next.key())
    }

    /// Moves the pane `pane` into a new tab of `group`, last of its tabs,
    /// shown and with focus. `false`, a no-op, when either is unknown, or
    /// the pane is alone in a tab of `group` already (it would be the same
    /// tab), or `group` goes with the pane (the pane is all it has).
    pub fn move_pane_to_new_tab(&mut self, pane: PaneId, group: TabGroupId) -> bool {
        if !self.root.contains(NodeKey::Pane(pane)) || self.root.group(group).is_none() {
            return false;
        }
        let alone_in_a_tab_of_it = (self.root.tabs_around(pane).last())
            .filter(|(around, _)| *around == group)
            .and_then(|(_, tab)| self.root.tab(*tab))
            .is_some_and(|tab| tab.root == LayoutNode::Pane(pane));
        if alone_in_a_tab_of_it {
            return false;
        }
        let Some(mut rest) = self.root.without_pane(pane) else {
            return false;
        };
        if rest.group(group).is_none() {
            return false;
        }
        rest.flatten();
        self.root = rest;
        let tab = Tab {
            id: TabId::new(),
            root: LayoutNode::Pane(pane),
            active: pane,
        };
        if let Some(group) = self.root.group_mut(group) {
            group.shown = tab.id;
            group.tabs.push(tab);
        }
        self.note_focus(pane);
        true
    }

    /// Moves the pane `pane` into the tab `tab`, beside that tab's active
    /// pane (after it, across), the tab shown and the pane with focus.
    /// `false`, a no-op, when either is unknown, or the pane is in that
    /// tab already.
    pub fn move_pane_to_tab(&mut self, pane: PaneId, tab: TabId) -> bool {
        if !self.root.contains(NodeKey::Pane(pane))
            || self
                .root
                .tabs_around(pane)
                .iter()
                .any(|(_, around)| *around == tab)
        {
            return false;
        }
        let Some(mut rest) = self.root.without_pane(pane) else {
            return false;
        };
        rest.flatten();
        let (Some(group), Some(beside)) = (
            rest.group_with_tab(tab),
            rest.tab(tab).map(|tab| tab.active),
        ) else {
            return false;
        };
        if !rest.split_beside(
            NodeKey::Pane(beside),
            LayoutNode::Pane(pane),
            SplitAxis::Horizontal,
            false,
        ) {
            return false;
        }
        rest.flatten();
        self.root = rest;
        if let Some(group) = self.root.group_mut(group) {
            group.shown = tab;
        }
        self.note_focus(pane);
        true
    }

    /// Where the pane the cursor is on can be moved to with
    /// [`Self::move_cursor_pane_to_tab`]: every tab on screen but its own,
    /// in the order of their groups, outermost first, and of their strips.
    pub fn tabs_to_move_to(&self) -> Vec<Destination> {
        let Some(pane) = self.cursor_pane_id() else {
            return Vec::new();
        };
        let own = self.root.tabs_around(pane).last().map(|(_, tab)| *tab);
        let groups = self.root.visible_groups();
        (groups.iter().enumerate())
            .flat_map(|(place, group)| {
                (group.tabs.iter())
                    .filter(move |tab| Some(tab.id) != own)
                    .map(move |tab| Destination {
                        tab: tab.id,
                        title: self.tab_title(tab),
                        group: place,
                    })
            })
            .collect()
    }

    /// The Move to Tab command, to a tab of [`Self::tabs_to_move_to`]'s:
    /// the pane the cursor is on (focus, out of workspace mode) into it.
    pub fn move_cursor_pane_to_tab(&mut self, tab: TabId) -> bool {
        let Some(pane) = self.cursor_pane_id() else {
            return false;
        };
        self.move_pane_to_tab(pane, tab)
    }

    /// The Move to New Tab command: the pane the cursor is on (focus, out
    /// of workspace mode) into a new tab of the group it is in; a pane in
    /// no group goes to the group a new tab would ([`Self::open_tab`]'s).
    /// `false` where there is no such pane, or nothing would change.
    pub fn move_cursor_pane_to_new_tab(&mut self) -> bool {
        let Some(pane) = self.cursor_pane_id() else {
            return false;
        };
        let group = match self.root.tabs_around(pane).last() {
            Some((group, _)) => *group,
            None => self.group_for_new_tab(false),
        };
        self.move_pane_to_new_tab(pane, group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::WorkspaceMode;
    use crate::types::{LayoutChild, Pane, PluginPane, SplitAxis, TabGroup};

    /// A view pane of the workspace's, not yet anywhere in its tree.
    fn pane(workspace: &mut Workspace) -> PaneId {
        let pane = Pane::new(
            PluginPane {
                name: "view".to_string(),
                title: "view".to_string(),
                state: serde_json::Value::Null,
            },
            None,
        );
        let id = pane.id;
        workspace.panes.push(pane);
        id
    }

    /// A row of `left`, a group of two tabs, and `right`: the panes, the
    /// group, and the group's two tabs' panes.
    fn row() -> (Workspace, PaneId, PaneId, TabGroupId, PaneId, PaneId) {
        let mut workspace = Workspace::mvp();
        let (left, right) = (pane(&mut workspace), pane(&mut workspace));
        let (first, second) = (pane(&mut workspace), pane(&mut workspace));
        let mut group = TabGroup::of(LayoutNode::Pane(first), first);
        let second_tab = Tab {
            id: TabId::new(),
            root: LayoutNode::Pane(second),
            active: second,
        };
        group.tabs.push(second_tab);
        let group_id = group.id;
        let child = |node| LayoutChild { node, weight: 1.0 };
        workspace.root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                child(LayoutNode::Pane(left)),
                child(LayoutNode::Tabs(group)),
                child(LayoutNode::Pane(right)),
            ],
        };
        workspace.note_focus(left);
        (workspace, left, right, group_id, first, second)
    }

    fn order(workspace: &Workspace) -> Vec<NodeKey> {
        match &workspace.root {
            LayoutNode::Split { children, .. } => children
                .iter()
                .map(|child| child.node.first_key())
                .collect(),
            other => vec![other.first_key()],
        }
    }

    #[test]
    fn a_pane_moves_into_another_tab_beside_its_active_pane() {
        let (mut workspace, left, right, group, first, second) = row();
        let tabs: Vec<TabId> = (workspace.root.group(group).unwrap().tabs.iter())
            .map(|tab| tab.id)
            .collect();
        // From `left`, every tab can be gone to; from `first`, its own is not.
        workspace.workspace_mode = WorkspaceMode::Commands {
            cursor: Some(Cursor::Pane(left)),
        };
        let to: Vec<TabId> = (workspace.tabs_to_move_to().iter())
            .map(|to| to.tab)
            .collect();
        assert_eq!(to, tabs);
        workspace.workspace_mode = WorkspaceMode::Commands {
            cursor: Some(Cursor::Pane(first)),
        };
        let to: Vec<TabId> = (workspace.tabs_to_move_to().iter())
            .map(|to| to.tab)
            .collect();
        assert_eq!(to, [tabs[1]]);

        // `left` into the tab not shown: beside its active pane, the tab
        // shown, the focus on it.
        assert!(workspace.move_pane_to_tab(left, tabs[1]));
        assert_eq!(
            order(&workspace),
            [NodeKey::Group(group), NodeKey::Pane(right)]
        );
        let shown = workspace.root.group(group).unwrap();
        assert_eq!(shown.shown, tabs[1]);
        let LayoutNode::Split { axis, children } = &shown.tabs[1].root else {
            panic!("a split in the tab");
        };
        assert_eq!(*axis, SplitAxis::Horizontal);
        let panes: Vec<NodeKey> = children
            .iter()
            .map(|child| child.node.first_key())
            .collect();
        assert_eq!(panes, [NodeKey::Pane(second), NodeKey::Pane(left)]);
        assert!(workspace.is_active_pane(left));
        assert!(workspace.root.is_canonical());
        // Into the tab it is in already: nothing.
        assert!(!workspace.move_pane_to_tab(left, tabs[1]));
    }

    #[test]
    fn two_panes_swap_places_and_the_tree_keeps_its_shape() {
        let (mut workspace, left, right, group, ..) = row();
        assert!(workspace.swap_nodes(NodeKey::Pane(left), NodeKey::Pane(right)));
        assert_eq!(
            order(&workspace),
            [
                NodeKey::Pane(right),
                NodeKey::Group(group),
                NodeKey::Pane(left)
            ]
        );
        assert!(workspace.root.is_canonical());
        // Focus stays on what it was on, which has moved.
        assert!(workspace.is_active_pane(left));
    }

    #[test]
    fn a_group_swaps_as_a_whole_and_a_pane_swaps_into_a_tab() {
        let (mut workspace, left, right, group, first, _) = row();
        assert!(workspace.swap_nodes(NodeKey::Group(group), NodeKey::Pane(right)));
        assert_eq!(
            order(&workspace),
            [
                NodeKey::Pane(left),
                NodeKey::Pane(right),
                NodeKey::Group(group)
            ]
        );
        // A pane swapped with one in a tab is in that tab, and is its
        // active pane; the other is where the pane was.
        assert!(workspace.swap_nodes(NodeKey::Pane(left), NodeKey::Pane(first)));
        assert_eq!(
            order(&workspace),
            [
                NodeKey::Pane(first),
                NodeKey::Pane(right),
                NodeKey::Group(group)
            ]
        );
        let shown = workspace
            .root
            .group(group)
            .and_then(TabGroup::shown_tab)
            .cloned();
        assert_eq!(
            shown.map(|tab| (tab.root, tab.active)),
            Some((LayoutNode::Pane(left), left))
        );
    }

    #[test]
    fn a_group_swapped_with_what_is_in_another_group_swaps_with_that_group() {
        let (mut workspace, left, right, group, ..) = row();
        // `right` made a group of its own: the row is a pane and two groups.
        let other = TabGroup::of(LayoutNode::Pane(right), right);
        let other_id = other.id;
        workspace.root.replace_pane(right, other);
        assert!(workspace.swap_nodes(NodeKey::Group(group), NodeKey::Pane(right)));
        assert_eq!(
            order(&workspace),
            [
                NodeKey::Pane(left),
                NodeKey::Group(other_id),
                NodeKey::Group(group)
            ]
        );
        // No group is in a tab of another.
        let nested = (workspace.root.groups().into_iter())
            .any(|outer| outer.tabs.iter().any(|tab| !tab.root.groups().is_empty()));
        assert!(!nested);
    }

    #[test]
    fn what_is_in_the_other_or_not_here_is_not_swapped() {
        let (mut workspace, left, _, group, first, _) = row();
        let before = workspace.root.clone();
        assert!(!workspace.swap_nodes(NodeKey::Pane(first), NodeKey::Group(group)));
        assert!(!workspace.swap_nodes(NodeKey::Pane(left), NodeKey::Pane(left)));
        assert!(!workspace.swap_nodes(NodeKey::Pane(left), NodeKey::Pane(PaneId::new())));
        assert_eq!(workspace.root, before);
    }

    #[test]
    fn shift_and_a_direction_swap_with_what_the_cursor_would_move_to() {
        let (mut workspace, left, right, group, first, _) = row();
        workspace.workspace_mode = WorkspaceMode::Commands {
            cursor: Some(Cursor::Pane(left)),
        };
        // Right of `left` is the group's shown pane.
        assert!(workspace.swap_cursor(Direction::Right));
        assert_eq!(order(&workspace)[0], NodeKey::Pane(first));
        // The cursor went with it: on again, it goes on.
        assert_eq!(workspace.workspace_mode.cursor(), Some(Cursor::Pane(left)));
        assert!(!workspace.swap_cursor(Direction::Up));
        // On the strip, the group goes as a whole.
        workspace.workspace_mode = WorkspaceMode::Commands {
            cursor: Some(Cursor::Strip(group)),
        };
        assert!(workspace.swap_cursor(Direction::Right));
        assert_eq!(
            order(&workspace),
            [
                NodeKey::Pane(first),
                NodeKey::Pane(right),
                NodeKey::Group(group)
            ]
        );
        // At the edge, nothing.
        assert!(!workspace.swap_cursor(Direction::Right));
    }

    #[test]
    fn a_pane_moves_into_a_new_tab_of_a_group() {
        let (mut workspace, left, right, group, ..) = row();
        assert!(workspace.move_pane_to_new_tab(right, group));
        assert_eq!(
            order(&workspace),
            [NodeKey::Pane(left), NodeKey::Group(group)]
        );
        let tabs = &workspace.root.group(group).expect("the group").tabs;
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[2].root, LayoutNode::Pane(right));
        assert!(workspace.is_active_pane(right));
        assert!(workspace.root.is_canonical());
        // Alone in a tab of it already: the same tab.
        assert!(!workspace.move_pane_to_new_tab(right, group));
    }

    #[test]
    fn move_to_new_tab_takes_the_cursor_s_pane_to_the_group_it_is_in() {
        let (mut workspace, left, _, group, first, _) = row();
        // A pane split in a tab is the group's next tab.
        let beside = pane(&mut workspace);
        let tab = workspace
            .root
            .group(group)
            .and_then(TabGroup::shown_tab)
            .map(|tab| tab.id);
        if let Some(tab) = tab.and_then(|tab| workspace.root.tab_mut(tab)) {
            tab.root.split_pane(first, beside, SplitAxis::Vertical);
        }
        workspace.note_focus(beside);
        assert!(workspace.move_cursor_pane_to_new_tab());
        assert_eq!(
            workspace.root.group(group).expect("the group").tabs.len(),
            3
        );
        assert!(workspace.is_active_pane(beside));
        // A pane in no group goes to the group that last had focus.
        workspace.note_focus(left);
        assert!(workspace.move_cursor_pane_to_new_tab());
        assert_eq!(
            workspace.root.group(group).expect("the group").tabs.len(),
            4
        );
        assert_eq!(order(&workspace)[0], NodeKey::Group(group));
    }
}
