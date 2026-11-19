//! Putting the workspace in a layout (`docs/workspace-layout-design.md`): a
//! tree of views, splits and tab groups with at most one place for the
//! workspace as it is.

use super::types::{LayoutChild, LayoutNode, Pane, PaneId, PluginPane, SplitAxis, Tab, TabGroup};
use super::types::{TabGroupId, TabId, Workspace};
use crate::SessionId;

/// A layout, its views made: what [`Workspace::arrange`] puts the
/// workspace in.
#[derive(Clone, Debug, PartialEq)]
pub enum Arrangement {
    /// A view of no session. A pane already showing the same view -- the
    /// same plugin on the same state -- is moved here rather than another
    /// made.
    View(PluginPane),
    /// Where the workspace as it is goes.
    Workspace,
    Split {
        axis: SplitAxis,
        /// Each child with its weight.
        children: Vec<(f32, Arrangement)>,
    },
    Tabs(Vec<Arrangement>),
}

impl Workspace {
    /// Puts the workspace in `arrangement`: its views where it says, and
    /// the workspace as it is in its place for it. With no such place,
    /// the panes the workspace had are closed, and the sessions they
    /// showed -- left detached, not ended -- are what comes back. Focus
    /// stays where it was when that pane is still on screen, and goes to
    /// the first pane on screen otherwise. Workspace mode ends.
    pub fn arrange(&mut self, arrangement: &Arrangement) -> Vec<SessionId> {
        // The panes already showing a view the arrangement has are taken
        // out, to be put where it says.
        let mut taken = Vec::new();
        let mut rest = Some(self.root.clone());
        for view in arrangement.views() {
            let reused = (self.panes.iter())
                .find(|pane| {
                    pane.session_id.is_none() && pane.plugin == *view && !taken.contains(&pane.id)
                })
                .map(|pane| pane.id);
            match reused {
                Some(pane) => {
                    rest = rest.and_then(|node| node.without_pane(pane));
                    taken.push(pane);
                }
                None => {
                    let pane = Pane::new(view.clone(), None);
                    taken.push(pane.id);
                    self.panes.push(pane);
                }
            }
        }
        // (An empty root group is no workspace to put anywhere.)
        let rest =
            rest.filter(|rest| !matches!(rest, LayoutNode::Tabs(group) if group.tabs.is_empty()));
        let slot = arrangement.has_slot();
        let mut closed = Vec::new();
        if !slot && let Some(rest) = &rest {
            let gone = rest.pane_ids();
            self.panes.retain(|pane| {
                if !gone.contains(&pane.id) {
                    return true;
                }
                if let Some(session) = pane.session_id
                    && !closed.contains(&session)
                {
                    closed.push(session);
                }
                false
            });
        }
        let mut views = taken.into_iter();
        let mut rest = if slot { rest } else { None };
        let built = arrangement.build(&mut views, &mut rest);
        self.root = match built {
            Some(mut root) => {
                root.flatten();
                root
            }
            None => LayoutNode::Tabs(TabGroup::empty()),
        };
        let visible = self.root.visible_pane_ids();
        match self.focus.filter(|focus| visible.contains(focus)) {
            Some(focus) => self.note_focus(focus),
            None => match visible.first() {
                Some(first) => self.note_focus(*first),
                None => self.focus = None,
            },
        }
        if (self.last_group).is_some_and(|group| self.root.group(group).is_none()) {
            self.last_group = self.root.groups().first().map(|group| group.id);
        }
        self.exit_workspace_mode();
        closed
    }
}

impl Arrangement {
    /// The views, in the order the tree is built in.
    fn views(&self) -> Vec<&PluginPane> {
        match self {
            Self::View(plugin) => vec![plugin],
            Self::Workspace => Vec::new(),
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|(_, child)| child.views())
                .collect(),
            Self::Tabs(tabs) => tabs.iter().flat_map(Self::views).collect(),
        }
    }

    fn has_slot(&self) -> bool {
        match self {
            Self::View(_) => false,
            Self::Workspace => true,
            Self::Split { children, .. } => children.iter().any(|(_, child)| child.has_slot()),
            Self::Tabs(tabs) => tabs.iter().any(Self::has_slot),
        }
    }

    /// The tree, each view's pane taken from `views` in order and the
    /// workspace as it is from `rest`. What has nothing in it is left out:
    /// a place for the workspace when there is none, a split's child or a
    /// tab with nothing, a split or a group of nothing.
    fn build(
        &self,
        views: &mut impl Iterator<Item = PaneId>,
        rest: &mut Option<LayoutNode>,
    ) -> Option<LayoutNode> {
        match self {
            Self::View(_) => views.next().map(LayoutNode::Pane),
            Self::Workspace => rest.take(),
            Self::Split { axis, children } => {
                let children: Vec<LayoutChild> = (children.iter())
                    .filter_map(|(weight, child)| {
                        child.build(views, rest).map(|node| LayoutChild {
                            node,
                            weight: *weight,
                        })
                    })
                    .collect();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next().map(|child| child.node),
                    _ => Some(LayoutNode::Split {
                        axis: *axis,
                        children,
                    }),
                }
            }
            Self::Tabs(tabs) => {
                let tabs: Vec<Tab> = (tabs.iter())
                    .filter_map(|tab| {
                        let root = tab.build(views, rest)?;
                        let active = root.first_pane()?;
                        Some(Tab {
                            id: TabId::new(),
                            root,
                            active,
                        })
                    })
                    .collect();
                let shown = tabs.first()?.id;
                Some(LayoutNode::Tabs(TabGroup {
                    id: TabGroupId::new(),
                    tabs,
                    shown,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionKind;
    use crate::types::NodeKey;

    fn view(name: &str) -> PluginPane {
        PluginPane {
            name: name.to_string(),
            title: name.to_string(),
            state: serde_json::Value::Null,
        }
    }

    fn sidebar() -> Arrangement {
        Arrangement::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                (1.0, Arrangement::View(view("sessions"))),
                (3.0, Arrangement::Workspace),
            ],
        }
    }

    /// The pane showing `name`, if one does.
    fn pane_of(workspace: &Workspace, name: &str) -> Option<PaneId> {
        (workspace.panes.iter())
            .find(|pane| pane.plugin.name == name)
            .map(|pane| pane.id)
    }

    #[test]
    fn the_workspace_goes_in_its_place_beside_the_views() {
        let mut workspace = Workspace::mvp();
        let terminal = workspace.focus.expect("a pane");
        let group = workspace.tab_groups()[0].id;

        assert!(workspace.arrange(&sidebar()).is_empty());

        let list = pane_of(&workspace, "sessions").expect("the list");
        match workspace.layout() {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children[0].node.first_key(), NodeKey::Pane(list));
                assert_eq!(children[0].weight, 1.0);
                assert_eq!(children[1].node.first_key(), NodeKey::Group(group));
                assert_eq!(children[1].weight, 3.0);
            }
            _ => panic!("expected the list beside the group"),
        }
        assert!(workspace.layout().is_canonical());
        // Focus stays where it was.
        assert!(workspace.is_active_pane(terminal));
    }

    #[test]
    fn a_view_already_shown_is_moved_not_made_again() {
        let mut workspace = Workspace::mvp();
        workspace.arrange(&sidebar());
        let list = pane_of(&workspace, "sessions").expect("the list");

        workspace.arrange(&sidebar());

        assert_eq!(
            workspace
                .panes
                .iter()
                .filter(|pane| pane.plugin.name == "sessions")
                .count(),
            1
        );
        assert_eq!(pane_of(&workspace, "sessions"), Some(list));
        match workspace.layout() {
            LayoutNode::Split { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!("expected the list beside the group"),
        }
    }

    #[test]
    fn a_view_shown_in_a_tab_is_taken_out_of_it() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab_with_view_activated(view("sessions"), true);
        let list = pane_of(&workspace, "sessions").expect("the list");
        assert_eq!(workspace.tab_count(), 2);

        workspace.arrange(&sidebar());

        // Its tab had nothing else, and went.
        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(pane_of(&workspace, "sessions"), Some(list));
        assert!(workspace.visible_pane_ids().contains(&list));
        assert!(workspace.layout().is_canonical());
    }

    #[test]
    fn with_no_place_for_the_workspace_its_panes_close_and_their_sessions_stay() {
        let mut workspace = Workspace::mvp();
        let terminal = workspace.active_session_id().expect("a session");
        let arrangement = Arrangement::Tabs(vec![
            Arrangement::View(view("wiki")),
            Arrangement::View(view("board-list")),
        ]);

        let closed = workspace.arrange(&arrangement);

        assert_eq!(closed, vec![terminal]);
        assert_eq!(workspace.detached_session_count(), 1);
        assert_eq!(
            workspace.session_kind(terminal),
            Some(SessionKind::Terminal)
        );
        assert_eq!(workspace.tab_count(), 2);
        // Focus goes to the first pane on screen: the first tab's.
        assert_eq!(workspace.focus, pane_of(&workspace, "wiki"));
    }

    #[test]
    fn an_empty_workspace_put_in_a_layout_is_its_views() {
        let mut workspace = Workspace::mvp();
        workspace.close_active_tab();
        assert!(workspace.visible_pane_ids().is_empty());

        workspace.arrange(&sidebar());

        // The place for the workspace had nothing to hold, and is left out.
        let list = pane_of(&workspace, "sessions").expect("the list");
        assert_eq!(workspace.layout().pane_ids(), vec![list]);
        assert!(workspace.is_active_pane(list));
    }
}
