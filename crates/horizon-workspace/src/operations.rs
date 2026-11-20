use super::types::{
    LayoutChild, LayoutNode, NodeKey, Pane, PaneId, PluginPane, SessionKind, SplitAxis, Tab,
    TabGroup, TabGroupId, TabId, Workspace, WorkspaceSession,
};
use crate::SessionId;

/// What a new pane holds: a session of a kind, shown with its kind's
/// plugin, or a plugin's view of no session.
#[derive(Clone, Debug)]
pub(crate) enum Content {
    Session(SessionKind, SessionId),
    View(PluginPane),
}

impl Workspace {
    /// A pane on `content`, its session registered when the workspace did
    /// not have it.
    fn new_pane(&mut self, content: Content) -> Pane {
        match content {
            Content::Session(kind, session_id) => {
                self.ensure_session(kind, session_id);
                Pane::new(kind.plugin(), Some(session_id))
            }
            Content::View(plugin) => Pane::new(plugin, None),
        }
    }
}

/// What [`Workspace::close_cursor_pane_or_tab`] actually did -- the shell
/// mirrors the pane-close vs. tab-close lifecycle off this, and the
/// payloads let tests pin the exact detached sessions.
#[derive(Debug, PartialEq, Eq)]
pub enum CloseCursorOutcome {
    /// Detached the cursor pane; its tab stays open. `Some` only when the
    /// pane carried a session -- a session-less view pane yields `None`.
    ClosedPane(Option<SessionId>),
    /// The cursor pane was its tab's last pane, or the cursor was on a
    /// group's strip, so the tab closed instead; carries the tab's
    /// now-detached sessions.
    ClosedTab(Vec<SessionId>),
    /// Nothing to act on: no cursor to resolve (an empty workspace).
    Noop,
}

/// Where a new pane goes beside what the cursor is on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    pub(crate) fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Horizontal,
            Self::Up | Self::Down => SplitAxis::Vertical,
        }
    }

    pub(crate) fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}

impl From<SplitAxis> for Side {
    /// A split's axis as the side it has always put a new pane on: right
    /// of a horizontal split, below a vertical one.
    fn from(axis: SplitAxis) -> Self {
        match axis {
            SplitAxis::Horizontal => Self::Right,
            SplitAxis::Vertical => Self::Down,
        }
    }
}

impl Workspace {
    pub fn mvp() -> Self {
        let session_id = SessionId::new();
        let terminal = Pane::new(SessionKind::Terminal.plugin(), Some(session_id));
        let active = terminal.id;
        let group = TabGroup::of(LayoutNode::Pane(active), active);

        Self {
            last_group: Some(group.id),
            root: LayoutNode::Tabs(group),
            panes: vec![terminal],
            sessions: vec![WorkspaceSession::new(session_id, SessionKind::Terminal, 1)],
            focus: Some(active),
            next_terminal_display_number: 2,
            next_agent_display_number: 1,
            workspace_mode: Default::default(),
        }
    }

    /// The group a new tab goes in. At the cursor (the New Tab command):
    /// the group the cursor is on or in, and on a pane under no group,
    /// that pane's place made a group, the pane its first tab
    /// (`docs/workspace-layout-design.md`). Otherwise -- a tab opened by
    /// anything else -- the group the cursor is on or in, else the one that
    /// last had focus; with no group at all, a new one right of the rest.
    pub(crate) fn group_for_new_tab(&mut self, at_cursor: bool) -> TabGroupId {
        if let Some(group) = self.cursor_group() {
            return group;
        }
        if at_cursor && let Some(pane) = self.cursor_pane_id() {
            let group = TabGroup::of(LayoutNode::Pane(pane), pane);
            let id = group.id;
            if self.root.replace_pane(pane, group) {
                return id;
            }
        }
        if let Some(group) = self.last_group() {
            return group;
        }
        let group = TabGroup::empty();
        let id = group.id;
        self.root = match std::mem::replace(&mut self.root, LayoutNode::Tabs(group.clone())) {
            // (An empty workspace's root is the group a tab goes in.)
            LayoutNode::Tabs(empty) if empty.tabs.is_empty() => LayoutNode::Tabs(group),
            // (Right of the whole of what was there.)
            rest => {
                let mut root = LayoutNode::Split {
                    axis: SplitAxis::Horizontal,
                    children: vec![
                        LayoutChild {
                            node: rest,
                            weight: 1.0,
                        },
                        LayoutChild {
                            node: LayoutNode::Tabs(group),
                            weight: 1.0,
                        },
                    ],
                };
                root.flatten();
                root
            }
        };
        id
    }

    pub(crate) fn open_tab(&mut self, content: Content) -> PaneId {
        self.open_tab_in(content, false)
    }

    fn open_tab_in(&mut self, content: Content, at_cursor: bool) -> PaneId {
        let group_id = self.group_for_new_tab(at_cursor);
        let pane = self.new_pane(content);
        let pane_id = pane.id;
        self.panes.push(pane);
        let tab = Tab {
            id: TabId::new(),
            root: LayoutNode::Pane(pane_id),
            active: pane_id,
        };
        if let Some(group) = self.root.group_mut(group_id) {
            group.shown = tab.id;
            group.tabs.push(tab);
        }
        self.note_focus(pane_id);
        pane_id
    }

    /// `activate` controls whether the new tab is shown and takes focus
    /// -- the control plane's `activate=false` default
    /// (`docs/cli-control-plane-design.md`'s "activate rides on creating/
    /// attaching operations" decision): the tab and its pane are always
    /// created either way, so the session exists and is queryable and
    /// attachable immediately; `activate: false` only puts back what was
    /// shown and focused before.
    pub fn open_tab_with_new_session_activated(
        &mut self,
        kind: SessionKind,
        activate: bool,
    ) -> SessionId {
        let session_id = SessionId::new();
        self.keeping_focus_unless(activate, |workspace| {
            workspace.open_tab(Content::Session(kind, session_id));
        });
        session_id
    }

    /// The New Tab command's: a tab over a new session of `kind`, in the
    /// group the cursor is on or in, or -- on a pane under no group --
    /// in a group made in that pane's place.
    pub fn new_tab_at_cursor_with_new_session(&mut self, kind: SessionKind) -> SessionId {
        let session_id = SessionId::new();
        self.open_tab_in(Content::Session(kind, session_id), true);
        session_id
    }

    /// [`Self::new_tab_at_cursor_with_new_session`] for a session-less
    /// view.
    pub fn new_tab_at_cursor_with_view(&mut self, plugin: PluginPane) -> PaneId {
        self.open_tab_in(Content::View(plugin), true)
    }

    /// [`Self::open_tab_with_new_session_activated`] for a session-less
    /// view: same `activate` semantics, but there is no session id to
    /// return, so this returns the new pane's id.
    pub fn open_tab_with_view_activated(&mut self, plugin: PluginPane, activate: bool) -> PaneId {
        let mut pane_id = None;
        self.keeping_focus_unless(activate, |workspace| {
            pane_id = Some(workspace.open_tab(Content::View(plugin)));
        });
        pane_id.expect("a pane was made")
    }

    /// Runs `change`, then, unless `activate`, puts back what each group
    /// showed and where focus was.
    fn keeping_focus_unless(&mut self, activate: bool, change: impl FnOnce(&mut Self)) {
        if activate {
            change(self);
            return;
        }
        let shown: Vec<(TabGroupId, TabId)> = (self.root.groups().into_iter())
            .map(|group| (group.id, group.shown))
            .collect();
        let (focus, last_group) = (self.focus, self.last_group);
        change(self);
        for (group, tab) in shown {
            if let Some(group) = self.root.group_mut(group)
                && group.tabs.iter().any(|kept| kept.id == tab)
            {
                group.shown = tab;
            }
        }
        if focus.is_some_and(|pane| self.root.visible_pane_ids().contains(&pane)) {
            self.focus = focus;
            self.last_group = last_group;
        }
    }

    /// Test-only fixture: splits the focused pane with an explicit
    /// session id.
    #[cfg(test)]
    pub fn split_active(&mut self, kind: SessionKind, session_id: SessionId) -> PaneId {
        let target = self.focus.map(NodeKey::Pane);
        self.split_node(
            target,
            Content::Session(kind, session_id),
            true,
            Side::Right,
        )
    }

    /// Puts a new pane on `content` beside `target` (the focused pane when
    /// `None`) on `side`, honoring `activate` for whether it takes focus.
    /// A target that is gone, or none at all, opens the pane as a tab.
    fn split_node(
        &mut self,
        target: Option<NodeKey>,
        content: Content,
        activate: bool,
        side: Side,
    ) -> PaneId {
        let target = target.filter(|key| self.root.contains(*key));
        let Some(target) = target else {
            let mut pane_id = None;
            self.keeping_focus_unless(activate, |workspace| {
                pane_id = Some(workspace.open_tab(content));
            });
            return pane_id.expect("a pane was made");
        };
        let pane = self.new_pane(content);
        let pane_id = pane.id;
        self.panes.push(pane);
        self.root.split_beside(
            target,
            LayoutNode::Pane(pane_id),
            side.axis(),
            side.before(),
        );
        self.root.flatten();
        if activate {
            self.reveal_pane(pane_id);
        }
        pane_id
    }

    /// Test-only fixture: "split with a new session of the focused pane's
    /// own kind".
    #[cfg(test)]
    pub fn split_active_with_new_session(&mut self) -> Option<(SessionKind, SessionId)> {
        let kind = self.session_kind(self.active_session_id()?)?;
        let session_id = SessionId::new();
        self.split_active(kind, session_id);
        Some((kind, session_id))
    }

    /// Splits beside `target_session_id`'s own pane (wherever it is, not
    /// necessarily the focused one) with a brand-new `kind` session --
    /// the control plane's `--split <session-id>` placement
    /// (`docs/cli-control-plane-design.md`'s "Placement vocabulary"
    /// decision). `None`, spawning nothing, when no pane shows
    /// `target_session_id`.
    pub fn split_session_with_new_session(
        &mut self,
        target_session_id: SessionId,
        kind: SessionKind,
        axis: SplitAxis,
        activate: bool,
    ) -> Option<SessionId> {
        let target = self.pane_of_session(target_session_id)?;
        let session_id = SessionId::new();
        self.split_node(
            Some(NodeKey::Pane(target)),
            Content::Session(kind, session_id),
            activate,
            Side::from(axis),
        );
        Some(session_id)
    }

    /// Puts a new session-less view beside what the cursor is on: a pane,
    /// or a whole tab group when the cursor is on its strip
    /// (`docs/workspace-layout-design.md`).
    pub fn split_cursor_with_view(&mut self, plugin: PluginPane, side: Side) -> PaneId {
        let target = self.cursor_node();
        self.split_node(target, Content::View(plugin), true, side)
    }

    /// [`Self::split_cursor_with_view`] for a new session of `kind`.
    pub fn split_cursor_with_new_session(
        &mut self,
        kind: SessionKind,
        side: Side,
        activate: bool,
    ) -> SessionId {
        let target = self.cursor_node();
        let session_id = SessionId::new();
        self.split_node(target, Content::Session(kind, session_id), activate, side);
        session_id
    }

    /// The pane-axis form of [`Self::split_cursor_with_view`], kept for the
    /// view chooser's two split placements.
    pub fn split_active_tab_with_view(&mut self, plugin: PluginPane, axis: SplitAxis) -> PaneId {
        self.split_cursor_with_view(plugin, Side::from(axis))
    }

    /// [`Self::split_session_with_new_session`] for a session-less view:
    /// returns the new pane's id. `None`, spawning nothing, when no pane
    /// shows `target_session_id`.
    pub fn split_session_with_view(
        &mut self,
        target_session_id: SessionId,
        plugin: PluginPane,
        axis: SplitAxis,
        activate: bool,
    ) -> Option<PaneId> {
        let target = self.pane_of_session(target_session_id)?;
        Some(self.split_node(
            Some(NodeKey::Pane(target)),
            Content::View(plugin),
            activate,
            Side::from(axis),
        ))
    }

    /// Puts a detached `session_id` in a pane: beside the active pane of
    /// the shown tab of the group that last had focus -- not beside the
    /// focused pane, which may be under no group, the list of sessions it
    /// was chosen from (`docs/workspace-layout-design.md`). With no group,
    /// beside the focused pane. `None` if the workspace does not know
    /// `session_id`.
    pub fn attach_existing_session_to_split_activated(
        &mut self,
        session_id: SessionId,
        activate: bool,
    ) -> Option<PaneId> {
        let kind = self.session_kind(session_id)?;
        let target = (self.last_group())
            .and_then(|group| self.root.group(group))
            .and_then(|group| group.shown_tab())
            .map(|tab| NodeKey::Pane(tab.active))
            .or(self.focus.map(NodeKey::Pane));
        Some(self.split_node(
            target,
            Content::Session(kind, session_id),
            activate,
            Side::Right,
        ))
    }

    /// Brings a session in front of its owner: its pane shown and focused,
    /// or, for a session no pane shows, a pane of it in the group that last
    /// had focus. Workspace mode ends with it. `false` for a session the
    /// workspace does not have, which changes nothing.
    pub fn show_session(&mut self, session_id: SessionId) -> bool {
        match self.pane_of_session(session_id) {
            Some(pane) => {
                self.reveal_pane(pane);
            }
            None => {
                if (self.attach_existing_session_to_split_activated(session_id, true)).is_none() {
                    return false;
                }
            }
        }
        self.exit_workspace_mode();
        true
    }

    /// Shows the tab at `index` of the group the cursor is on or in, and
    /// gives focus to its active pane.
    pub fn activate_tab_index(&mut self, index: usize) -> bool {
        match self.current_group() {
            Some(group) => self.activate_tab_in(group, index),
            None => false,
        }
    }

    /// Shows the tab at `index` of `group`, and gives focus to its active
    /// pane.
    pub fn activate_tab_in(&mut self, group: TabGroupId, index: usize) -> bool {
        let Some(found) = self.root.group_mut(group) else {
            return false;
        };
        let Some(tab) = found.tabs.get(index) else {
            return false;
        };
        found.shown = tab.id;
        let active = tab.active;
        self.reveal_pane(active);
        true
    }

    /// Shows the tab `pane` is in (the innermost, where there are more),
    /// with the focus where it is. `false` where it is shown already, or
    /// the pane is in no tab.
    pub fn show_tab_of(&mut self, pane: PaneId) -> bool {
        let Some(&(group, tab)) = self.root.tabs_around(pane).last() else {
            return false;
        };
        let Some(found) = self.root.group_mut(group) else {
            return false;
        };
        if found.shown == tab {
            return false;
        }
        found.shown = tab;
        true
    }

    /// Closes the tab at `index` of the group the cursor is on or in.
    /// Closing the workspace's last tab is allowed: it leaves a valid,
    /// persistable, empty workspace.
    pub fn close_tab_index(&mut self, index: usize) -> Vec<SessionId> {
        match self.current_group() {
            Some(group) => self.close_tab_in(group, index),
            None => Vec::new(),
        }
    }

    /// Closes the tab at `index` of `group`, with every pane in it; the
    /// group shows the tab that took its place, and a group left with no
    /// tab goes (but the root, which is the empty workspace then).
    pub fn close_tab_in(&mut self, group: TabGroupId, index: usize) -> Vec<SessionId> {
        let Some(tab) = (self.root.group(group)).and_then(|group| group.tabs.get(index)) else {
            return Vec::new();
        };
        let tab_id = tab.id;
        let pane_ids = tab.root.pane_ids();
        let mut session_ids = Vec::new();
        self.panes.retain(|pane| {
            if pane_ids.contains(&pane.id) {
                if let Some(session_id) = pane.session_id
                    && !session_ids.contains(&session_id)
                {
                    session_ids.push(session_id);
                }
                false
            } else {
                true
            }
        });
        self.root = match self.root.without_tab(tab_id) {
            Some(mut root) => {
                root.flatten();
                root
            }
            None => LayoutNode::Tabs(TabGroup {
                id: group,
                ..TabGroup::empty()
            }),
        };
        self.settle_focus(Some(group));
        session_ids
    }

    /// Test-only fixture: activates pane N by its visible index.
    #[cfg(test)]
    pub fn activate_visible_pane(&mut self, index: usize) -> bool {
        let Some(pane_id) = self.visible_pane_id(index) else {
            return false;
        };
        self.activate_pane(pane_id)
    }

    /// Gives focus to `pane_id`, which must be on screen. `false`, a
    /// no-op, otherwise ([`Self::reveal_pane`] shows it first).
    pub fn activate_pane(&mut self, pane_id: PaneId) -> bool {
        if !self.visible_pane_ids().contains(&pane_id) {
            return false;
        }
        self.note_focus(pane_id);
        true
    }

    /// Shows `pane_id` -- every group around it showing the tab it is in
    /// -- and gives it focus. `false` for a pane the workspace does not
    /// have.
    pub fn reveal_pane(&mut self, pane_id: PaneId) -> bool {
        let around = self.root.tabs_around(pane_id);
        if around.is_empty() && !self.root.pane_ids().contains(&pane_id) {
            return false;
        }
        for (group, tab) in around {
            if let Some(group) = self.root.group_mut(group) {
                group.shown = tab;
            }
        }
        self.note_focus(pane_id);
        true
    }

    /// Focus is `pane_id`: the tab it is in remembers it, and the group
    /// around it is the one that last had focus.
    pub(crate) fn note_focus(&mut self, pane_id: PaneId) {
        self.focus = Some(pane_id);
        if let Some(&(group, tab)) = self.root.tabs_around(pane_id).last() {
            if let Some(tab) = self.root.tab_mut(tab) {
                tab.active = pane_id;
            }
            self.last_group = Some(group);
        }
    }

    /// After panes or tabs went: focus on a pane still on screen -- the
    /// active pane of `preferring`'s shown tab when there is one, else of
    /// the group that last had focus, else the first pane on screen.
    fn settle_focus(&mut self, preferring: Option<TabGroupId>) {
        let visible = self.root.visible_pane_ids();
        if self.focus.is_some_and(|pane| visible.contains(&pane)) && preferring.is_none() {
            return;
        }
        let shown_active = |workspace: &Self, group: Option<TabGroupId>| {
            group
                .and_then(|group| workspace.root.group(group))
                .and_then(|group| group.shown_tab())
                .map(|tab| tab.active)
                .filter(|pane| visible.contains(pane))
        };
        let next = shown_active(self, preferring)
            .or_else(|| shown_active(self, self.last_group))
            .or_else(|| self.focus.filter(|pane| visible.contains(pane)))
            .or_else(|| visible.first().copied());
        match next {
            Some(pane) => self.note_focus(pane),
            None => self.focus = None,
        }
        if self
            .last_group
            .is_some_and(|group| self.root.group(group).is_none())
        {
            self.last_group = self.root.groups().first().map(|group| group.id);
        }
    }

    /// Points a view pane at `plugin`: which plugin it shows, and the
    /// state. `false`, a no-op, when `pane_id` is unknown or shows a
    /// session, whose plugin is its kind's.
    pub fn set_pane_plugin(&mut self, pane_id: PaneId, plugin: PluginPane) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return false;
        };
        if pane.session_id.is_some() {
            return false;
        }
        pane.plugin = plugin;
        true
    }

    pub(crate) fn detach_pane(&mut self, pane_id: PaneId) -> Option<SessionId> {
        let session_id = self
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .and_then(|pane| pane.session_id);
        let group = self
            .root
            .tabs_around(pane_id)
            .last()
            .map(|(group, _)| *group);

        self.panes.retain(|pane| pane.id != pane_id);
        self.root = match self.root.without_pane(pane_id) {
            Some(mut root) => {
                root.flatten();
                root
            }
            None => LayoutNode::Tabs(match &self.root {
                LayoutNode::Tabs(group) => TabGroup {
                    id: group.id,
                    ..TabGroup::empty()
                },
                _ => TabGroup::empty(),
            }),
        };
        let focused = self.focus == Some(pane_id);
        if focused {
            self.focus = None;
        }
        self.settle_focus(group.filter(|_| focused));
        session_id
    }

    pub(crate) fn close_visible_pane(&mut self, index: usize) -> Option<SessionId> {
        let pane_id = self.visible_pane_id(index)?;
        self.close_pane(pane_id)
    }

    /// Closes `pane_id`, unless it is the last pane of its tab: closing that
    /// pane is closing the tab, which a pane command does not do.
    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<SessionId> {
        if self.is_last_of_its_tab(pane_id) || self.visible_pane_ids().len() <= 1 {
            return None;
        }
        self.detach_pane(pane_id)
    }

    /// Whether `pane_id` is the only pane of the tab it is in.
    fn is_last_of_its_tab(&self, pane_id: PaneId) -> bool {
        (self.root.tabs_around(pane_id).last())
            .and_then(|(_, tab)| self.root.tab(*tab))
            .is_some_and(|tab| tab.root.pane_ids().len() == 1)
    }

    pub fn close_active_pane(&mut self) -> Option<SessionId> {
        self.close_visible_pane(self.active_visible_index())
    }

    /// Closes the pane the workspace-mode cursor points at (the focused
    /// pane outside the mode). Returns whether a pane was detached: the
    /// last pane of a tab is not, as [`Self::close_pane`].
    pub fn close_cursor_pane(&mut self) -> bool {
        let Some(pane_id) = self.cursor_pane_id() else {
            return false;
        };
        if self.is_last_of_its_tab(pane_id) || self.visible_pane_ids().len() <= 1 {
            return false;
        }
        self.detach_pane(pane_id);
        true
    }

    /// `x`'s model operation (`docs/workspace-mode-design.md`'s "commands
    /// act on the cursor"): closes the pane the cursor points at, or its
    /// tab when that pane is the tab's last one; on a group's strip,
    /// closes the tab the group shows. Only a missing cursor (an empty
    /// workspace) stays inert.
    pub fn close_cursor_pane_or_tab(&mut self) -> CloseCursorOutcome {
        if let Some(group) = self.cursor_strip() {
            let shown = (self.root.group(group)).and_then(|group| group.shown_index());
            return match shown {
                Some(index) => CloseCursorOutcome::ClosedTab(self.close_tab_in(group, index)),
                None => CloseCursorOutcome::Noop,
            };
        }
        let Some(pane_id) = self.cursor_pane_id() else {
            return CloseCursorOutcome::Noop;
        };
        if !self.is_last_of_its_tab(pane_id) {
            return CloseCursorOutcome::ClosedPane(self.detach_pane(pane_id));
        }
        let Some(&(group, tab)) = self.root.tabs_around(pane_id).last() else {
            return CloseCursorOutcome::ClosedPane(self.detach_pane(pane_id));
        };
        let index = (self.root.group(group))
            .and_then(|group| group.tabs.iter().position(|kept| kept.id == tab))
            .unwrap_or(0);
        CloseCursorOutcome::ClosedTab(self.close_tab_in(group, index))
    }

    pub fn close_active_tab(&mut self) -> Vec<SessionId> {
        self.close_tab_index(self.active_tab_index())
    }

    /// Focus moves to the next pane on screen, around the end.
    pub fn focus_next(&mut self) {
        let visible = self.visible_pane_ids();
        if visible.is_empty() {
            return;
        }
        let current = (self.focus)
            .and_then(|focus| visible.iter().position(|pane| *pane == focus))
            .unwrap_or(0);
        let next = visible[(current + 1) % visible.len()];
        self.note_focus(next);
    }
}
