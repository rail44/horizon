use super::mode::Cursor;
use super::types::{
    NodeKey, Pane, PaneId, SessionKind, SessionSummary, Tab, TabGroup, TabGroupId, TabId,
    TabSummary, Workspace, WorkspaceSession,
};
use crate::SessionId;

impl Workspace {
    /// The pane that shows `session_id`, wherever it is; none for a
    /// detached session.
    pub(crate) fn pane_of_session(&self, session_id: SessionId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|pane| pane.session_id == Some(session_id))
            .map(|pane| pane.id)
    }

    pub(crate) fn visible_pane_id(&self, index: usize) -> Option<PaneId> {
        self.visible_pane_ids().get(index).copied()
    }

    /// The visible-index counterpart's `PaneId`-keyed equivalent
    /// (`docs/recursive-layout-design.md`'s slice 2): `workspace::view`'s
    /// recursive renderer builds each pane's view directly off its
    /// `PaneId`, so it needs these rather than the index-based accessors
    /// above (which stay as they are for every other caller -- the
    /// control plane, palette, and workspace-mode's own flat cursor index
    /// are all untouched by this slice).
    pub fn pane_plugin(&self, pane_id: PaneId) -> Option<&crate::PluginPane> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| &pane.plugin)
    }

    /// The session a pane shows, if it shows one.
    pub fn pane_session_id(&self, pane_id: PaneId) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .and_then(|pane| pane.session_id)
    }

    /// The session `pane` shows, when it is one of `kind`.
    fn pane_session_of(&self, pane: &Pane, kind: SessionKind) -> Option<SessionId> {
        let session = pane.session_id?;
        (self.session_kind(session) == Some(kind)).then_some(session)
    }

    /// Every pane of the tab that holds `pane_id`, in layout order. What a
    /// pane-local decision reads when it has to look at its tab rather than
    /// at itself -- the board's list finding the thread pane beside it, a
    /// thread finding its list. Empty when no tab holds `pane_id`.
    pub fn tab_panes(&self, pane_id: PaneId) -> Vec<PaneId> {
        (self.root.tabs_around(pane_id).last())
            .and_then(|(_, tab)| self.root.tab(*tab))
            .map(|tab| tab.root.pane_ids())
            .unwrap_or_default()
    }

    pub fn terminal_session_id(&self, pane_id: PaneId) -> Option<SessionId> {
        let pane = self.panes.iter().find(|pane| pane.id == pane_id)?;
        self.pane_session_of(pane, SessionKind::Terminal)
    }

    pub fn agent_session_id(&self, pane_id: PaneId) -> Option<SessionId> {
        let pane = self.panes.iter().find(|pane| pane.id == pane_id)?;
        self.pane_session_of(pane, SessionKind::Agent)
    }

    /// Test-only now: no production caller remains (2026-07-18 visibility
    /// audit); kept as a small test fixture helper.
    #[cfg(test)]
    pub(crate) fn pane_title_for(&self, pane_id: PaneId) -> Option<String> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| self.pane_title(pane))
    }

    pub fn is_active_pane(&self, pane_id: PaneId) -> bool {
        self.focus == Some(pane_id)
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
        match self.workspace_mode.cursor() {
            Some(Cursor::Pane(pane_id)) if self.visible_pane_ids().contains(&pane_id) => {
                Some(pane_id)
            }
            // (A group as a whole is no pane.)
            Some(Cursor::Strip(group)) if self.root.group(group).is_some() => None,
            _ => self.focus,
        }
    }

    /// The tab group whose strip the cursor is on, if it is on one.
    pub fn cursor_strip(&self) -> Option<TabGroupId> {
        match self.workspace_mode.cursor() {
            Some(Cursor::Strip(group)) if self.root.group(group).is_some() => Some(group),
            _ => None,
        }
    }

    /// The tab group the cursor is on or in: the one whose strip it is on,
    /// or the innermost around the pane it is on. None for a pane under no
    /// group.
    pub fn cursor_group(&self) -> Option<TabGroupId> {
        if let Some(group) = self.cursor_strip() {
            return Some(group);
        }
        let pane = self.cursor_pane_id()?;
        self.root.tabs_around(pane).last().map(|(group, _)| *group)
    }

    /// The node the cursor is on, as operations that put something beside
    /// it name it.
    pub(crate) fn cursor_node(&self) -> Option<NodeKey> {
        match self.cursor_strip() {
            Some(group) => Some(NodeKey::Group(group)),
            None => self.cursor_pane_id().map(NodeKey::Pane),
        }
    }

    /// The tab group that last had focus, while it is there; else the
    /// first on screen.
    pub(crate) fn last_group(&self) -> Option<TabGroupId> {
        (self.last_group)
            .filter(|group| self.root.group(*group).is_some())
            .or_else(|| self.root.visible_groups().first().map(|group| group.id))
    }

    /// The tab group tab operations act on: the one the cursor is on or
    /// in, else the one that last had focus.
    pub fn current_group(&self) -> Option<TabGroupId> {
        self.cursor_group().or_else(|| self.last_group())
    }

    /// The tab groups on screen, outermost first.
    pub fn tab_groups(&self) -> Vec<&TabGroup> {
        self.root.visible_groups()
    }

    /// The tab group `id`, wherever it is.
    pub fn tab_group(&self, id: TabGroupId) -> Option<&TabGroup> {
        self.root.group(id)
    }

    /// The workspace's tree.
    pub fn layout(&self) -> &super::types::LayoutNode {
        &self.root
    }

    /// Test-only now: `workspace::mode`'s cursor is `PaneId`-keyed directly
    /// (`docs/recursive-layout-design.md`'s slice 4), so no production call
    /// site needs to resolve a `PaneId` back to a visible index anymore --
    /// the GPUI shell's pane click handler (`src/workspace/render.rs`, its last
    /// production caller) now targets `activate_pane` by `PaneId` directly.
    /// Kept as a small test fixture helper.
    #[cfg(test)]
    pub fn visible_index_of(&self, pane_id: PaneId) -> Option<usize> {
        self.visible_pane_ids().iter().position(|id| *id == pane_id)
    }

    /// Every pane that exists anywhere in the workspace, across every tab
    /// -- not just the active tab's visible ones. Used to prune per-pane UI
    /// state keyed by `PaneId` (`src/workspace/session_lifecycle.rs`) once
    /// a pane is gone for good, regardless of which close/terminate path
    /// removed it.
    pub fn all_pane_ids(&self) -> std::collections::HashSet<PaneId> {
        self.panes.iter().map(|pane| pane.id).collect()
    }

    pub fn active_terminal_session_id(&self) -> Option<SessionId> {
        self.terminal_session_id(self.focus?)
    }

    /// What a session is doing, while that is known.
    pub fn session_activity(
        &self,
        session_id: SessionId,
    ) -> Option<&super::types::SessionActivity> {
        self.session(session_id)?.activity.as_ref()
    }

    pub fn active_session_id(&self) -> Option<SessionId> {
        self.pane_session_id(self.focus?)
    }

    /// The session attached to the pane the workspace-mode cursor sits on
    /// -- the free-floating cursor while the mode is active, or simply the
    /// focused pane otherwise (see [`cursor_pane_id`]). This is the
    /// kind-agnostic "operation target" counterpart of
    /// [`active_session_id`]: `terminate_active_session` resolves through
    /// it so that terminating inside workspace mode hits the pane under the
    /// cursor rather than whichever pane still holds keyboard focus,
    /// mirroring how the agent commands (`active_agent_session`) already
    /// resolve through [`cursor_pane_id`] instead of the focused pane.
    ///
    /// [`cursor_pane_id`]: Self::cursor_pane_id
    /// [`active_session_id`]: Self::active_session_id
    pub fn cursor_session_id(&self) -> Option<SessionId> {
        let pane_id = self.cursor_pane_id()?;
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .and_then(|pane| pane.session_id)
    }

    /// Test-only now: the GPUI shell's recursive renderer
    /// (`src/workspace/render.rs`, `docs/recursive-layout-design.md`'s slice 2)
    /// resolves a terminal pane's session by `PaneId`
    /// (`terminal_session_id`) rather than a visible index, which was
    /// this method's last production caller.
    #[cfg(test)]
    pub fn visible_terminal_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.terminal_session_id(pane_id)
    }

    /// Test-only now: the GPUI shell's recursive renderer
    /// (`src/workspace/render.rs`, `docs/recursive-layout-design.md`'s slice 2)
    /// resolves an agent pane's session by `PaneId` (`agent_session_id`)
    /// rather than a visible index, which was this method's last
    /// production caller.
    #[cfg(test)]
    pub fn visible_agent_session_id(&self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.agent_session_id(pane_id)
    }

    /// Test-only now: the workspace overview's header text
    /// (`ws.session_count()` alongside `detached_session_count()`) was its
    /// last production caller and is gone
    /// (the retired plan `docs/plans/application-ui/01-session-manager.md`,
    /// removed in a8839fc1 -- session
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

    /// The pane showing `session_id`, wherever it is; `None` for a
    /// detached session.
    pub fn pane_location_for_session(&self, session_id: SessionId) -> Option<PaneId> {
        self.pane_of_session(session_id)
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
                directory: session.directory.clone(),
                parent_session_id: session.parent_session_id,
                activity: session.activity.clone(),
            })
            .collect()
    }

    pub fn tab_summaries(&self) -> Vec<TabSummary> {
        match self.current_group() {
            Some(group) => self.group_tab_summaries(group),
            None => Vec::new(),
        }
    }

    /// The tabs of `group`, as its strip shows them.
    pub fn group_tab_summaries(&self, group: TabGroupId) -> Vec<TabSummary> {
        let Some(group) = self.root.group(group) else {
            return Vec::new();
        };
        group
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| TabSummary {
                index,
                title: self.tab_title(tab),
                active: tab.id == group.shown,
                pane_count: tab.root.pane_ids().len(),
                active_session_id: self.tab_session_id(tab),
                activity: (self.tab_session_id(tab))
                    .and_then(|session| self.session(session))
                    .and_then(|session| session.activity.clone()),
            })
            .collect()
    }

    pub fn tab_count(&self) -> usize {
        (self.current_group())
            .and_then(|group| self.root.group(group))
            .map_or(0, |group| group.tabs.len())
    }

    pub fn visible_panes(&self) -> Vec<&Pane> {
        let visible = self.visible_pane_ids();
        visible
            .iter()
            .filter_map(|id| self.panes.iter().find(|pane| pane.id == *id))
            .collect()
    }

    pub(crate) fn active_visible_index(&self) -> usize {
        self.visible_pane_ids()
            .iter()
            .position(|pane| Some(*pane) == self.focus)
            .unwrap_or(0)
    }

    pub fn active_tab_index(&self) -> usize {
        (self.current_group())
            .and_then(|group| self.root.group(group))
            .and_then(|group| group.shown_index())
            .unwrap_or(0)
    }

    pub(crate) fn active_title(&self) -> String {
        self.panes
            .iter()
            .find(|pane| Some(pane.id) == self.focus)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "none".to_string())
    }

    /// Test-only now: no production caller resolves a pane's title by
    /// visible index anymore. `pane_title_for` is the `PaneId`-keyed
    /// counterpart (`docs/recursive-layout-design.md`'s slice 2), though
    /// it too currently has no production caller of its own -- the
    /// `workspace_snapshot` payload's pane titles go through
    /// `pane_summaries`/`pane_title(&Pane)` instead
    /// (`crates/horizon-workspace/src/snapshot.rs`).
    #[cfg(test)]
    pub fn visible_pane_title(&self, index: usize) -> Option<String> {
        self.visible_panes()
            .get(index)
            .map(|pane| self.pane_title(pane))
    }

    pub(crate) fn visible_pane_ids(&self) -> Vec<PaneId> {
        self.root.visible_pane_ids()
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.root.group(self.current_group()?)?.shown_tab()
    }

    /// What the pane `pane` is called, as its tab would be: its session's
    /// title, or its view's.
    pub fn pane_label(&self, pane: PaneId) -> Option<String> {
        let pane = self.panes.iter().find(|candidate| candidate.id == pane)?;
        Some(self.pane_title(pane))
    }

    /// Whether `pane` is all its tab has: its tab's label stands for it.
    pub fn pane_is_its_tab(&self, pane: PaneId) -> bool {
        (self.root.tabs_around(pane).last())
            .and_then(|(_, tab)| self.root.tab(*tab))
            .is_some_and(|tab| tab.root == crate::types::LayoutNode::Pane(pane))
    }

    /// The active pane of the tab at `index` of `group`.
    pub fn tab_active_pane(&self, group: TabGroupId, index: usize) -> Option<PaneId> {
        Some(self.root.group(group)?.tabs.get(index)?.active)
    }

    /// The tab `pane` is in, the innermost where there are more.
    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.root.tabs_around(pane).last().map(|(_, tab)| *tab)
    }

    /// The tab at `index` in `group`'s strip.
    pub fn tab_in(&self, group: TabGroupId, index: usize) -> Option<TabId> {
        Some(self.root.group(group)?.tabs.get(index)?.id)
    }

    pub(crate) fn tab_title(&self, tab: &Tab) -> String {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .map(|pane| self.pane_title(pane))
            .unwrap_or_else(|| "Empty".to_string())
    }

    pub(crate) fn tab_session_id(&self, tab: &Tab) -> Option<SessionId> {
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active)
            .and_then(|pane| pane.session_id)
    }

    pub(crate) fn pane_title(&self, pane: &Pane) -> String {
        pane.session_id
            .and_then(|session_id| self.session(session_id))
            .map(|session| session.title.clone())
            .unwrap_or_else(|| pane.title())
    }

    /// The kind of a session the workspace has, whether a pane shows it
    /// or not.
    pub fn session_kind(&self, session_id: SessionId) -> Option<SessionKind> {
        self.session(session_id).map(|session| session.kind)
    }

    pub(crate) fn session(&self, session_id: SessionId) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    /// The directory `session_id` is known to be confined to / was spawned
    /// in, when known -- `None` for a session nothing has recorded one for
    /// yet (every terminal session today, plus a resumed agent session; see
    /// `WorkspaceSession::directory`'s doc comment) or an unknown
    /// session id. The read side of `docs/session-relationship-design.md`
    /// decision 4a's "open a terminal in any session's directory" command --
    /// `CommandId::OpenTerminalInSessionDirectory`'s enablement and target
    /// cwd both resolve through this for the active session.
    pub fn session_directory(&self, session_id: SessionId) -> Option<&std::path::Path> {
        self.session(session_id)?.directory.as_deref()
    }
}
