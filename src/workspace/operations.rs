use super::types::{
    LayoutNode, Pane, PaneId, PaneKind, SessionKind, SplitAxis, Tab, TabId, Workspace,
    WorkspaceSession,
};
use crate::session::SessionId;

impl Workspace {
    pub(crate) fn mvp() -> Self {
        let session_id = SessionId::new();
        let terminal = Pane::new(PaneKind::Terminal, Some(session_id));
        let active = terminal.id;
        let tab = Tab {
            id: TabId::new(),
            root: LayoutNode::Pane(active),
            active,
        };

        Self {
            active_tab: tab.id,
            tabs: vec![tab],
            panes: vec![terminal],
            sessions: vec![WorkspaceSession::new(session_id, SessionKind::Terminal, 1)],
            next_terminal_display_number: 2,
            next_agent_display_number: 1,
            workspace_mode_cursor: None,
        }
    }

    pub(crate) fn open_tab(&mut self, kind: PaneKind, session_id: Option<SessionId>) -> PaneId {
        self.ensure_session(kind, session_id);
        let pane = Pane::new(kind, session_id);
        let pane_id = pane.id;
        let tab = Tab {
            id: TabId::new(),
            root: LayoutNode::Pane(pane_id),
            active: pane_id,
        };
        self.active_tab = tab.id;
        self.tabs.push(tab);
        self.panes.push(pane);
        pane_id
    }

    pub(crate) fn open_tab_with_new_session(&mut self, kind: PaneKind) -> SessionId {
        let session_id = SessionId::new();
        self.open_tab(kind, Some(session_id));
        session_id
    }

    pub(crate) fn split_active(&mut self, kind: PaneKind, session_id: Option<SessionId>) -> PaneId {
        self.ensure_session(kind, session_id);
        let pane = Pane::new(kind, session_id);
        let pane_id = pane.id;
        if let Some(tab) = self.active_tab_mut() {
            let old_root = tab.root.clone();
            tab.root = LayoutNode::Split {
                axis: SplitAxis::Horizontal,
                ratio: 0.5,
                first: Box::new(old_root),
                second: Box::new(LayoutNode::Pane(pane_id)),
            };
            tab.active = pane_id;
        }
        self.panes.push(pane);
        pane_id
    }

    pub(crate) fn split_active_with_new_session(&mut self) -> Option<(PaneKind, SessionId)> {
        let kind = self.visible_pane_kind(self.active_visible_index())?;
        let session_id = SessionId::new();
        self.split_active(kind, Some(session_id));
        Some((kind, session_id))
    }

    pub(crate) fn attach_existing_session_to_split(
        &mut self,
        session_id: SessionId,
    ) -> Option<PaneId> {
        let kind = self.session_pane_kind(session_id)?;
        Some(self.split_active(kind, Some(session_id)))
    }

    pub(crate) fn activate_tab_index(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        self.active_tab = tab.id;
        true
    }

    pub(crate) fn activate_pane_index(&mut self, tab_index: usize, pane_index: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_index) else {
            return false;
        };
        let Some(pane_id) = tab.root.pane_ids().get(pane_index).copied() else {
            return false;
        };
        let tab_id = tab.id;

        self.active_tab = tab_id;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.active = pane_id;
            return true;
        }

        false
    }

    pub(crate) fn close_tab_index(&mut self, index: usize) -> Vec<SessionId> {
        if self.tabs.len() <= 1 {
            return Vec::new();
        }

        let Some(tab) = self.tabs.get(index).cloned() else {
            return Vec::new();
        };

        let pane_ids = tab.root.pane_ids();
        let mut session_ids = Vec::new();
        self.panes.retain(|pane| {
            if pane_ids.contains(&pane.id) {
                if let Some(session_id) = pane.session_id {
                    if !session_ids.contains(&session_id) {
                        session_ids.push(session_id);
                    }
                }
                false
            } else {
                true
            }
        });

        let closed_active_tab = tab.id == self.active_tab;
        self.tabs.remove(index);
        if closed_active_tab {
            let next_index = index.min(self.tabs.len().saturating_sub(1));
            self.active_tab = self.tabs[next_index].id;
        }

        session_ids
    }

    pub(crate) fn activate_visible_pane(&mut self, index: usize) -> bool {
        let Some(pane_id) = self.visible_pane_id(index) else {
            return false;
        };
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.active = pane_id;
        true
    }

    pub(crate) fn detach_pane(&mut self, pane_id: PaneId) -> Option<SessionId> {
        let session_id = self
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .and_then(|pane| pane.session_id);

        self.panes.retain(|pane| pane.id != pane_id);
        let mut empty_tabs = Vec::new();
        for tab in &mut self.tabs {
            if let Some(root) = tab.root.without_pane(pane_id) {
                tab.root = root;
            } else {
                empty_tabs.push(tab.id);
                continue;
            }
            if tab.active == pane_id {
                tab.active = tab.root.first_pane().unwrap_or(pane_id);
            }
        }
        self.tabs.retain(|tab| !empty_tabs.contains(&tab.id));
        if !self.tabs.iter().any(|tab| tab.id == self.active_tab) {
            if let Some(tab) = self.tabs.first() {
                self.active_tab = tab.id;
            }
        }

        session_id
    }

    pub(crate) fn close_visible_pane(&mut self, index: usize) -> Option<SessionId> {
        if self.visible_pane_ids().len() <= 1 {
            return None;
        }

        let pane_id = self.visible_pane_id(index)?;
        self.detach_pane(pane_id)
    }

    pub(crate) fn close_active_pane(&mut self) -> Option<SessionId> {
        self.close_visible_pane(self.active_visible_index())
    }

    pub(crate) fn close_active_tab(&mut self) -> Vec<SessionId> {
        self.close_tab_index(self.active_tab_index())
    }

    pub(crate) fn focus_next(&mut self) {
        let visible = self.visible_pane_ids();
        if visible.is_empty() {
            return;
        }
        let active = self
            .active_tab()
            .map(|tab| tab.active)
            .unwrap_or(visible[0]);
        let current = visible.iter().position(|pane| *pane == active).unwrap_or(0);
        let next = visible[(current + 1) % visible.len()];
        if let Some(tab) = self.active_tab_mut() {
            tab.active = next;
        }
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == self.active_tab)
    }
}
