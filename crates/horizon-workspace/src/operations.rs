use super::types::{
    LayoutNode, Pane, PaneId, PaneKind, SessionKind, SplitAxis, Tab, TabId, ViewKind, Workspace,
    WorkspaceSession,
};
use crate::SessionId;

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
            workspace_mode_cursor: None,
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

    /// `activate` controls whether the new tab becomes the workspace's
    /// active one -- the control plane's `activate=false` default
    /// (`docs/cli-control-plane-design.md`'s "activate rides on creating/
    /// attaching operations" decision): the tab and its pane are always
    /// created and pushed either way, so the session exists and is
    /// queryable/attachable immediately; `activate: false` only means
    /// [`Self::active_tab`] is restored to whatever it was before this
    /// call, leaving the caller's own focus-follow (`workspace::
    /// request_active_pane_focus`) with nothing to move. See
    /// `WorkspaceShell::create_session`/`external_new_session` in
    /// `src/workspace.rs`, its callers (`activate: true` for a human
    /// surface's dive, `false` for the control plane's default).
    pub fn open_tab_with_new_session_activated(
        &mut self,
        kind: PaneKind,
        activate: bool,
    ) -> SessionId {
        let previous_active_tab = self.active_tab;
        let session_id = SessionId::new();
        self.open_tab(kind, Some(session_id));
        if !activate {
            self.active_tab = previous_active_tab;
        }
        session_id
    }

    /// Test-only now: its last production caller was
    /// `split_active_with_new_session` (also `#[cfg(test)]` now -- see its
    /// doc comment), retired by `docs/roadmap.md`'s "Placement-first session
    /// creation". Kept as a test fixture helper: it's the common
    /// "split the active tab with an explicit session id" shape most tests
    /// build their fixtures with.
    #[cfg(test)]
    pub fn split_active(&mut self, kind: PaneKind, session_id: Option<SessionId>) -> PaneId {
        self.split_tab(
            self.active_tab,
            kind,
            session_id,
            true,
            SplitAxis::Horizontal,
        )
    }

    /// Splits `tab_id` (any tab, not necessarily the active one) with a new
    /// `kind` pane at that tab's currently-focused pane, honoring `activate`
    /// for whether that tab becomes active and its new pane takes tab-local
    /// focus -- the shared worker behind [`Self::split_active`] (`tab_id ==
    /// self.active_tab`, `activate: true`, a no-op change in that case) and
    /// [`Self::attach_existing_session_to_split_activated`]. A `tab_id`
    /// that no longer exists is silently a no-op for the layout mutation
    /// (defensive; every call site resolves `tab_id` from a live tab
    /// immediately beforehand, so this never actually happens today) but
    /// still returns the new pane's id and registers its session, matching
    /// `split_active`'s prior behavior of never failing outright.
    fn split_tab(
        &mut self,
        tab_id: TabId,
        kind: PaneKind,
        session_id: Option<SessionId>,
        activate: bool,
        axis: SplitAxis,
    ) -> PaneId {
        self.split_pane_in_tab(tab_id, None, kind, session_id, activate, axis)
    }

    /// Splits `tab_id` with a new `kind` pane at `target_pane_id` (the
    /// tab's currently-focused pane when `None`), honoring `activate` and
    /// `axis` per [`Self::split_tab`]'s doc comment. Shared by
    /// [`Self::split_tab`] (target: the tab's own focus) and
    /// [`Self::split_session_with_new_session`] (an explicit target pane,
    /// which may not be the tab's currently-focused one).
    fn split_pane_in_tab(
        &mut self,
        tab_id: TabId,
        target_pane_id: Option<PaneId>,
        kind: PaneKind,
        session_id: Option<SessionId>,
        activate: bool,
        axis: SplitAxis,
    ) -> PaneId {
        self.ensure_session(kind, session_id);
        let pane = Pane::new(kind, session_id);
        let pane_id = pane.id;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            let target = target_pane_id.unwrap_or(tab.active);
            tab.root.split_pane(target, pane_id, axis);
            tab.root.flatten();
            if activate {
                tab.active = pane_id;
            }
        }
        self.panes.push(pane);
        if activate {
            self.active_tab = tab_id;
        }
        pane_id
    }

    /// Test-only now: its last production caller was the pre-GPUI shell's
    /// split-active-pane action, retired by `docs/roadmap.md`'s
    /// "Placement-first session creation" -- `WorkspaceShell::create_session`
    /// (`src/workspace.rs`) drives `split_session_with_new_session` (an
    /// explicit target session, not "whatever kind the active pane happens
    /// to be") for every remaining split caller. Kept as a small
    /// workspace-level test fixture helper (`workspace::tests`) since
    /// re-deriving "split with a new session of the active pane's own kind"
    /// at each call site would be more code than this one method.
    #[cfg(test)]
    pub fn split_active_with_new_session(&mut self) -> Option<(PaneKind, SessionId)> {
        let kind = self.visible_pane_kind(self.active_visible_index())?;
        let session_id = SessionId::new();
        self.split_active(kind, Some(session_id));
        Some((kind, session_id))
    }

    /// Splits the tab that currently hosts `target_session_id`'s pane (any
    /// tab, not just the active one) with a brand-new `kind` session,
    /// honoring `activate` exactly like [`Self::split_tab`] -- the control
    /// plane's `--split <session-id>` placement
    /// (`docs/cli-control-plane-design.md`'s "Placement vocabulary"
    /// decision) and the palette's `Split Right…`/`Split Down…` verbs
    /// (`docs/recursive-layout-design.md`'s slice 3), which pick `axis`.
    /// Splits at `target_session_id`'s own pane (not the tab's
    /// focus, which may be a different pane), so the new pane lands next to
    /// the requested session regardless of what else is focused in that
    /// tab. Returns `None`, spawning nothing, when `target_session_id` isn't
    /// referenced by any pane (the CLI's "target session not found" error
    /// case -- callers are expected to have already surfaced that as an
    /// error before reaching here, see `app::external_commands::
    /// dispatch_invoke`'s pre-check; this is defense in depth, not the
    /// primary error path).
    pub fn split_session_with_new_session(
        &mut self,
        target_session_id: SessionId,
        kind: PaneKind,
        axis: SplitAxis,
        activate: bool,
    ) -> Option<SessionId> {
        let target_pane_id = self
            .panes
            .iter()
            .find(|pane| pane.session_id == Some(target_session_id))
            .map(|pane| pane.id)?;
        let tab_id = self
            .tabs
            .iter()
            .find(|tab| tab.root.pane_ids().contains(&target_pane_id))
            .map(|tab| tab.id)?;
        let session_id = SessionId::new();
        self.split_pane_in_tab(
            tab_id,
            Some(target_pane_id),
            kind,
            Some(session_id),
            activate,
            axis,
        );
        Some(session_id)
    }

    /// Splits the active tab at its currently-focused pane with a new
    /// session-less view pane (`docs/theme-settings-view-design.md`'s
    /// "first session-less first-party view") -- the view chooser's split
    /// placements for a first-party view. Unlike
    /// [`Self::split_session_with_new_session`], there is no session id to
    /// anchor the split via `active_session_id`, so this always targets the
    /// active tab's own focus directly; that's equivalent in practice since
    /// the shell calls this immediately after the chooser closes, with
    /// nothing else able to move focus meanwhile.
    pub fn split_active_tab_with_view(&mut self, kind: ViewKind, axis: SplitAxis) -> PaneId {
        self.split_tab(self.active_tab, PaneKind::View(kind), None, true, axis)
    }

    /// Attaches a detached `session_id` as a split in the active tab,
    /// honoring `activate` for whether that tab becomes active and its new
    /// pane takes tab-local focus -- see [`Self::split_tab`]'s doc comment
    /// for what `activate: false` leaves untouched (human surfaces always
    /// pass `true`, per `docs/workspace-mode-design.md`'s Amended
    /// second-round decision 1; the control plane's `attach` external
    /// command defaults `false`). Reattaching always targets the currently
    /// active tab (unlike [`Self::split_session_with_new_session`]'s
    /// explicit-target lookup): there is no other tab to place a *detached*
    /// session's pane next to. `None` if `session_id` isn't a session this
    /// workspace knows about at all.
    pub fn attach_existing_session_to_split_activated(
        &mut self,
        session_id: SessionId,
        activate: bool,
    ) -> Option<PaneId> {
        let kind = self.session_pane_kind(session_id)?;
        Some(self.split_tab(
            self.active_tab,
            kind,
            Some(session_id),
            activate,
            SplitAxis::Horizontal,
        ))
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

    /// Test-only now: `workspace::mode`'s commit path (its last production
    /// caller) is `PaneId`-keyed since `docs/recursive-layout-design.md`'s
    /// slice 4 and uses [`Self::activate_pane`] instead. Kept as a test
    /// fixture helper -- "activate pane N by its visible index" is a common
    /// setup shape across this crate's tests unrelated to workspace mode.
    #[cfg(test)]
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

    /// The `PaneId`-targeting counterpart of [`Self::activate_visible_
    /// pane`] -- `workspace::mode`'s cursor is `PaneId`-keyed
    /// (`docs/recursive-layout-design.md`'s slice 4), so committing it
    /// needs to activate by id directly rather than re-deriving a visible
    /// index first. `false`, a no-op, if `pane_id` isn't currently visible
    /// in the active tab.
    pub fn activate_pane(&mut self, pane_id: PaneId) -> bool {
        if !self.visible_pane_ids().contains(&pane_id) {
            return false;
        }
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
            if let Some(mut root) = tab.root.without_pane(pane_id) {
                root.flatten();
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

    /// `PaneId`-targeted counterpart to `close_visible_pane` -- the pane
    /// header's close button (`workspace::view::pane`) already knows the
    /// exact pane it means, per `docs/recursive-layout-design.md`'s slice 2
    /// (the recursive renderer builds panes by `PaneId`, not visible
    /// index). Same last-pane-in-the-tab guard.
    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<SessionId> {
        if self.visible_pane_ids().len() <= 1 {
            return None;
        }

        self.detach_pane(pane_id)
    }

    pub fn close_active_pane(&mut self) -> Option<SessionId> {
        self.close_visible_pane(self.active_visible_index())
    }

    pub fn close_active_tab(&mut self) -> Vec<SessionId> {
        self.close_tab_index(self.active_tab_index())
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

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == self.active_tab)
    }
}
