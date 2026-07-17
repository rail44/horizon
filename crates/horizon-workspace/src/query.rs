use super::types::{
    Pane, PaneId, PaneKind, PaneSummary, SessionSummary, Tab, TabSummary, Workspace,
    WorkspaceSession,
};
use crate::SessionId;

impl Workspace {
    pub fn visible_pane_id(&self, index: usize) -> Option<PaneId> {
        self.visible_pane_ids().get(index).copied()
    }

    /// The visible-index counterpart's `PaneId`-keyed equivalent
    /// (`docs/recursive-layout-design.md`'s slice 2): `workspace::view`'s
    /// recursive renderer builds each pane's view directly off its
    /// `PaneId`, so it needs these rather than the index-based accessors
    /// above (which stay as they are for every other caller -- the
    /// control plane, palette, and workspace-mode's own flat cursor index
    /// are all untouched by this slice).
    pub fn pane_kind(&self, pane_id: PaneId) -> Option<PaneKind> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| pane.kind)
    }

    pub fn terminal_session_id(&self, pane_id: PaneId) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    pub fn agent_session_id(&self, pane_id: PaneId) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Agent)
            .and_then(|pane| pane.session_id)
    }

    pub fn pane_title_for(&self, pane_id: PaneId) -> Option<String> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| self.pane_title(pane))
    }

    pub fn is_active_pane(&self, pane_id: PaneId) -> bool {
        self.active_tab().map(|tab| tab.active) == Some(pane_id)
    }

    /// The `PaneId` the workspace-mode cursor currently sits on: the free-
    /// floating cursor while the mode is active, or simply the focused
    /// pane otherwise -- mirroring `Workspace::is_workspace_mode_active`'s
    /// "cursor equals focus outside the mode" invariant (`docs/workspace-
    /// mode-design.md`). Falls back to the focused pane if the cursor's own
    /// `PaneId` stopped being visible out from under it (e.g. a
    /// non-creating palette command closed it while the mode was active) --
    /// defensive, since nothing in `workspace::mode` itself removes the
    /// cursor's pane.
    pub fn cursor_pane_id(&self) -> Option<PaneId> {
        let focus = self.active_tab().map(|tab| tab.active);
        match self.workspace_mode_cursor {
            Some(pane_id) if self.visible_pane_ids().contains(&pane_id) => Some(pane_id),
            _ => focus,
        }
    }

    /// Test-only now: `workspace::mode`'s cursor is `PaneId`-keyed directly
    /// (`docs/recursive-layout-design.md`'s slice 4), so no production call
    /// site needs to resolve a `PaneId` back to a visible index anymore --
    /// `workspace::view::pane`'s click handler (its last production caller)
    /// now targets `commit_workspace_mode_to`/`activate_pane` by `PaneId`
    /// directly. Kept as a small test fixture helper.
    #[cfg(test)]
    pub fn visible_index_of(&self, pane_id: PaneId) -> Option<usize> {
        self.visible_pane_ids().iter().position(|id| *id == pane_id)
    }

    /// Every pane that exists anywhere in the workspace, across every tab
    /// -- not just the active tab's visible ones. Used to prune per-pane UI
    /// state keyed by `PaneId` (`workspace::input::PaneKeyedSignals`) once
    /// a pane is gone for good, regardless of which close/terminate path
    /// removed it.
    pub fn all_pane_ids(&self) -> std::collections::HashSet<PaneId> {
        self.panes.iter().map(|pane| pane.id).collect()
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

    /// Test-only now: `workspace::view::pane`'s recursive renderer
    /// (`docs/recursive-layout-design.md`'s slice 2) resolves a terminal
    /// pane's session by `PaneId` (`terminal_session_id`) rather than a
    /// visible index, which was this method's last production caller.
    #[cfg(test)]
    pub fn visible_terminal_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Terminal)
            .and_then(|pane| pane.session_id)
    }

    /// Test-only now: `workspace::view::pane`'s recursive renderer
    /// (`docs/recursive-layout-design.md`'s slice 2) resolves an agent
    /// pane's session by `PaneId` (`agent_session_id`) rather than a
    /// visible index, which was this method's last production caller.
    #[cfg(test)]
    pub fn visible_agent_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id && pane.kind == PaneKind::Agent)
            .and_then(|pane| pane.session_id)
    }

    /// Test-only now: the workspace overview's header text
    /// (`ws.session_count()` alongside `detached_session_count()`) was its
    /// last production caller and is gone
    /// (`docs/plans/application-ui/01-session-manager.md` -- session
    /// management moved to its own modal, which derives the same count from
    /// `control_surface::session_manager_items` instead).
    #[cfg(test)]
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

    /// The `(tab_index, pane_index)` of the visible pane currently hosting
    /// `session_id`, if any -- lets the session manager modal
    /// (`control_surface::view::session_manager`) resolve an *attached*
    /// row straight to `CommandInvocation::ActivatePane` without a
    /// separate lookup table. `None` for a detached session (no pane
    /// references it).
    pub fn pane_location_for_session(&self, session_id: SessionId) -> Option<(usize, usize)> {
        self.tabs.iter().enumerate().find_map(|(tab_index, tab)| {
            tab.root
                .pane_ids()
                .into_iter()
                .position(|pane_id| {
                    self.panes
                        .iter()
                        .any(|pane| pane.id == pane_id && pane.session_id == Some(session_id))
                })
                .map(|pane_index| (tab_index, pane_index))
        })
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

    /// Test-only now: `workspace::view::pane`'s recursive renderer
    /// (`docs/recursive-layout-design.md`'s slice 2) titles a pane by
    /// `PaneId` (`pane_title_for`) rather than a visible index, which was
    /// this method's last production caller.
    #[cfg(test)]
    pub fn visible_pane_title(&self, index: usize) -> Option<String> {
        self.visible_panes()
            .get(index)
            .map(|pane| self.pane_title(pane))
    }

    pub fn visible_pane_ids(&self) -> Vec<PaneId> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };

        tab.root.pane_ids()
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab)
    }

    pub fn tab_title(&self, tab: &Tab) -> String {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "Empty".to_string())
    }

    pub fn tab_session_id(&self, tab: &Tab) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .and_then(|pane| pane.session_id)
    }

    pub fn pane_title(&self, pane: &Pane) -> String {
        pane.session_id
            .and_then(|session_id| self.session(session_id))
            .map(|session| session.title.clone())
            .unwrap_or_else(|| pane.title())
    }

    /// `pub(crate)`, not `pub(super)`, so `app::runtime::spawn_cwd` can
    /// resolve a spawn-source session's kind without going through the
    /// pane layer (a *detached* source session -- one with no pane at all
    /// -- still has a kind, just no `PaneId` to look one up from).
    pub fn session_pane_kind(&self, session_id: SessionId) -> Option<PaneKind> {
        self.session(session_id)
            .map(|session| PaneKind::from(session.kind))
    }

    pub fn session(&self, session_id: SessionId) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }
}
