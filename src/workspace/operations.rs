use super::types::{
    LayoutNode, Pane, PaneId, PaneKind, PaneSummary, SessionKind, SessionSummary, SplitAxis, Tab,
    TabId, TabSummary, Workspace, WorkspaceSession,
};
use crate::session::SessionId;

impl Workspace {
    pub fn mvp() -> Self {
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
        }
    }

    pub fn open_tab(&mut self, kind: PaneKind, session_id: Option<SessionId>) -> PaneId {
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

    pub fn split_active(&mut self, kind: PaneKind, session_id: Option<SessionId>) -> PaneId {
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

    pub fn split_active_with_new_session(&mut self) -> Option<(PaneKind, SessionId)> {
        let kind = self.visible_pane_kind(self.active_visible_index())?;
        let session_id = SessionId::new();
        self.split_active(kind, Some(session_id));
        Some((kind, session_id))
    }

    pub fn attach_session_to_new_tab(&mut self, session_id: SessionId) -> PaneId {
        self.open_tab(PaneKind::Terminal, Some(session_id))
    }

    pub fn attach_session_to_split(&mut self, session_id: SessionId) -> PaneId {
        self.split_active(PaneKind::Terminal, Some(session_id))
    }

    pub fn attach_existing_session_to_split(&mut self, session_id: SessionId) -> Option<PaneId> {
        let kind = self.session_pane_kind(session_id)?;
        Some(self.split_active(kind, Some(session_id)))
    }

    pub fn activate_tab_index(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        self.active_tab = tab.id;
        true
    }

    pub fn activate_pane_index(&mut self, tab_index: usize, pane_index: usize) -> bool {
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

    pub fn close_tab_index(&mut self, index: usize) -> Vec<SessionId> {
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

    pub fn terminate_session(&mut self, session_id: SessionId) -> bool {
        let Some(_) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return false;
        };

        let pane_ids: Vec<_> = self
            .panes
            .iter()
            .filter(|pane| pane.session_id == Some(session_id))
            .map(|pane| pane.id)
            .collect();

        self.sessions.retain(|session| session.id != session_id);
        for pane_id in pane_ids {
            self.detach_pane(pane_id);
        }

        true
    }

    pub fn terminate_active_session(&mut self) -> Option<SessionId> {
        let session_id = self.active_session_id()?;
        self.terminate_session(session_id).then_some(session_id)
    }

    pub fn activate_visible_pane(&mut self, index: usize) -> bool {
        let Some(pane_id) = self.visible_pane_id(index) else {
            return false;
        };
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.active = pane_id;
        true
    }

    pub fn detach_pane(&mut self, pane_id: PaneId) -> Option<SessionId> {
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

    pub fn close_visible_pane(&mut self, index: usize) -> Option<SessionId> {
        if self.visible_pane_ids().len() <= 1 {
            return None;
        }

        let pane_id = self.visible_pane_id(index)?;
        self.detach_pane(pane_id)
    }

    pub fn session_is_referenced(&self, session_id: SessionId) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.session_id == Some(session_id))
    }

    pub fn focus_next(&mut self) {
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

    pub fn visible_pane_id(&self, index: usize) -> Option<PaneId> {
        self.visible_pane_ids().get(index).copied()
    }

    pub fn active_terminal_session_id(&self) -> Option<SessionId> {
        let active = self.active_tab()?.active;
        self.panes
            .iter()
            .find(|pane| pane.id == active && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    pub fn active_session_id(&self) -> Option<SessionId> {
        let active = self.active_tab()?.active;
        self.panes
            .iter()
            .find(|pane| pane.id == active)
            .and_then(|pane| pane.session_id)
    }

    pub fn visible_terminal_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    pub fn visible_agent_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Agent)
            .and_then(|pane| pane.session_id)
    }

    pub fn terminal_session_ids(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|session| session.kind == SessionKind::Terminal)
            .map(|session| session.id)
            .collect()
    }

    pub fn agent_session_ids(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|session| session.kind == SessionKind::Agent)
            .map(|session| session.id)
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn detached_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| !self.session_is_referenced(session.id))
            .count()
    }

    pub fn detached_session_summaries(&self) -> Vec<SessionSummary> {
        self.session_summaries()
            .into_iter()
            .filter(|session| !session.attached)
            .collect()
    }

    pub fn session_summaries(&self) -> Vec<SessionSummary> {
        self.sessions
            .iter()
            .map(|session| SessionSummary {
                id: session.id,
                kind: session.kind,
                display_number: session.display_number,
                title: session.title.clone(),
                attached: self.session_is_referenced(session.id),
            })
            .collect()
    }

    pub fn tab_summaries(&self) -> Vec<TabSummary> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| TabSummary {
                index,
                title: self.tab_title(tab),
                active: tab.id == self.active_tab,
                pane_count: tab.root.pane_ids().len(),
                active_session_id: self.tab_session_id(tab),
            })
            .collect()
    }

    pub fn pane_summaries(&self) -> Vec<PaneSummary> {
        self.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_index, tab)| {
                tab.root.pane_ids().into_iter().enumerate().filter_map(
                    move |(pane_index, pane_id)| {
                        self.panes
                            .iter()
                            .find(|pane| pane.id == pane_id)
                            .map(|pane| PaneSummary {
                                tab_index,
                                pane_index,
                                title: self.pane_title(pane),
                                kind: pane.kind,
                                active: tab.active == pane_id,
                                tab_active: tab.id == self.active_tab,
                            })
                    },
                )
            })
            .collect()
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn visible_panes(&self) -> Vec<&Pane> {
        let visible = self.visible_pane_ids();
        visible
            .iter()
            .filter_map(|id| self.panes.iter().find(|pane| pane.id == *id))
            .collect()
    }

    pub fn visible_pane_kind(&self, index: usize) -> Option<PaneKind> {
        self.visible_panes().get(index).map(|pane| pane.kind)
    }

    pub fn active_pane_is(&self, kind: PaneKind) -> bool {
        self.visible_pane_kind(self.active_visible_index()) == Some(kind)
    }

    pub fn active_visible_pane_is(&self, index: usize, kind: PaneKind) -> bool {
        self.active_visible_index() == index && self.visible_pane_kind(index) == Some(kind)
    }

    pub fn active_pane_accepts_text_input(&self) -> bool {
        matches!(
            self.visible_pane_kind(self.active_visible_index()),
            Some(PaneKind::Terminal | PaneKind::Agent)
        )
    }

    pub fn active_visible_pane_accepts_text_input(&self, index: usize) -> bool {
        self.active_visible_index() == index
            && matches!(
                self.visible_pane_kind(index),
                Some(PaneKind::Terminal | PaneKind::Agent)
            )
    }

    pub fn active_visible_index(&self) -> usize {
        let active = self.active_tab().map(|tab| tab.active);
        self.visible_pane_ids()
            .iter()
            .position(|pane| Some(*pane) == active)
            .unwrap_or(0)
    }

    pub fn active_tab_index(&self) -> usize {
        self.tabs
            .iter()
            .position(|tab| tab.id == self.active_tab)
            .unwrap_or(0)
    }

    pub fn active_title(&self) -> String {
        let active = self.active_tab().map(|tab| tab.active);
        self.panes
            .iter()
            .find(|pane| Some(pane.id) == active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "none".to_string())
    }

    pub fn visible_pane_title(&self, index: usize) -> Option<String> {
        self.visible_panes()
            .get(index)
            .map(|pane| self.pane_title(pane))
    }

    fn visible_pane_ids(&self) -> Vec<PaneId> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };

        tab.root.pane_ids()
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == self.active_tab)
    }

    fn tab_title(&self, tab: &Tab) -> String {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "Empty".to_string())
    }

    fn tab_session_id(&self, tab: &Tab) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .and_then(|pane| pane.session_id)
    }

    fn pane_title(&self, pane: &Pane) -> String {
        pane.session_id
            .and_then(|session_id| self.session(session_id))
            .map(|session| session.title.clone())
            .unwrap_or_else(|| pane.title())
    }

    fn ensure_session(&mut self, kind: PaneKind, session_id: Option<SessionId>) {
        let Some(session_id) = session_id else {
            return;
        };
        if self.sessions.iter().any(|session| session.id == session_id) {
            return;
        }

        let session_kind = SessionKind::from(kind);
        let display_number = self.allocate_session_display_number(session_kind);
        self.sessions.push(WorkspaceSession::new(
            session_id,
            session_kind,
            display_number,
        ));
    }

    fn session_pane_kind(&self, session_id: SessionId) -> Option<PaneKind> {
        self.session(session_id)
            .map(|session| PaneKind::from(session.kind))
    }

    fn session(&self, session_id: SessionId) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    fn allocate_session_display_number(&mut self, kind: SessionKind) -> usize {
        let next = match kind {
            SessionKind::Terminal => &mut self.next_terminal_display_number,
            SessionKind::Agent => &mut self.next_agent_display_number,
        };
        let display_number = *next;
        *next += 1;
        display_number
    }
}
