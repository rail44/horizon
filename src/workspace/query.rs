use super::types::{
    Pane, PaneId, PaneKind, PaneSummary, SessionSummary, Tab, TabSummary, Workspace,
    WorkspaceSession,
};
use crate::session::SessionId;

impl Workspace {
    pub(crate) fn visible_pane_id(&self, index: usize) -> Option<PaneId> {
        self.visible_pane_ids().get(index).copied()
    }

    pub(crate) fn active_terminal_session_id(&self) -> Option<SessionId> {
        let active = self.active_tab()?.active;
        self.panes
            .iter()
            .find(|pane| pane.id == active && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    pub(crate) fn active_session_id(&self) -> Option<SessionId> {
        let active = self.active_tab()?.active;
        self.panes
            .iter()
            .find(|pane| pane.id == active)
            .and_then(|pane| pane.session_id)
    }

    pub(crate) fn visible_terminal_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    pub(crate) fn visible_agent_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Agent)
            .and_then(|pane| pane.session_id)
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn detached_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| !self.session_is_referenced(session.id))
            .count()
    }

    pub(crate) fn detached_session_summaries(&self) -> Vec<SessionSummary> {
        self.session_summaries()
            .into_iter()
            .filter(|session| !session.attached)
            .collect()
    }

    pub(crate) fn session_summaries(&self) -> Vec<SessionSummary> {
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

    pub(crate) fn tab_summaries(&self) -> Vec<TabSummary> {
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

    pub(crate) fn pane_summaries(&self) -> Vec<PaneSummary> {
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

    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn visible_panes(&self) -> Vec<&Pane> {
        let visible = self.visible_pane_ids();
        visible
            .iter()
            .filter_map(|id| self.panes.iter().find(|pane| pane.id == *id))
            .collect()
    }

    pub(crate) fn visible_pane_kind(&self, index: usize) -> Option<PaneKind> {
        self.visible_panes().get(index).map(|pane| pane.kind)
    }

    pub(crate) fn active_pane_is(&self, kind: PaneKind) -> bool {
        self.visible_pane_kind(self.active_visible_index()) == Some(kind)
    }

    pub(crate) fn active_visible_pane_is(&self, index: usize, kind: PaneKind) -> bool {
        self.active_visible_index() == index && self.visible_pane_kind(index) == Some(kind)
    }

    pub(crate) fn active_pane_accepts_text_input(&self) -> bool {
        matches!(
            self.visible_pane_kind(self.active_visible_index()),
            Some(PaneKind::Terminal | PaneKind::Agent)
        )
    }

    pub(crate) fn active_visible_pane_accepts_text_input(&self, index: usize) -> bool {
        self.active_visible_index() == index
            && matches!(
                self.visible_pane_kind(index),
                Some(PaneKind::Terminal | PaneKind::Agent)
            )
    }

    pub(crate) fn active_visible_index(&self) -> usize {
        let active = self.active_tab().map(|tab| tab.active);
        self.visible_pane_ids()
            .iter()
            .position(|pane| Some(*pane) == active)
            .unwrap_or(0)
    }

    pub(crate) fn active_tab_index(&self) -> usize {
        self.tabs
            .iter()
            .position(|tab| tab.id == self.active_tab)
            .unwrap_or(0)
    }

    pub(crate) fn active_title(&self) -> String {
        let active = self.active_tab().map(|tab| tab.active);
        self.panes
            .iter()
            .find(|pane| Some(pane.id) == active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "none".to_string())
    }

    pub(crate) fn visible_pane_title(&self, index: usize) -> Option<String> {
        self.visible_panes()
            .get(index)
            .map(|pane| self.pane_title(pane))
    }

    pub(super) fn visible_pane_ids(&self) -> Vec<PaneId> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };

        tab.root.pane_ids()
    }

    pub(super) fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab)
    }

    pub(super) fn tab_title(&self, tab: &Tab) -> String {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "Empty".to_string())
    }

    pub(super) fn tab_session_id(&self, tab: &Tab) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .and_then(|pane| pane.session_id)
    }

    pub(super) fn pane_title(&self, pane: &Pane) -> String {
        pane.session_id
            .and_then(|session_id| self.session(session_id))
            .map(|session| session.title.clone())
            .unwrap_or_else(|| pane.title())
    }

    pub(super) fn session_pane_kind(&self, session_id: SessionId) -> Option<PaneKind> {
        self.session(session_id)
            .map(|session| PaneKind::from(session.kind))
    }

    pub(super) fn session(&self, session_id: SessionId) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }
}
