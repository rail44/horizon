use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::terminal::TerminalFrame;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct PaneId(Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct SessionId(Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct TabId(Uuid);

#[derive(Clone, Debug)]
pub struct Workspace {
    tabs: Vec<Tab>,
    panes: Vec<Pane>,
    sessions: Vec<WorkspaceSession>,
    active_tab: TabId,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub root: LayoutNode,
    pub active: PaneId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabSummary {
    pub index: usize,
    pub title: String,
    pub active: bool,
    pub pane_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub kind: SessionKind,
    pub title: String,
    pub attached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSession {
    pub id: SessionId,
    pub kind: SessionKind,
    pub title: String,
}

#[derive(Clone, Debug)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub session_id: Option<SessionId>,
    pub output: String,
    pub terminal_frame: Option<TerminalFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneKind {
    Terminal,
    Agent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionKind {
    Terminal,
    Agent,
}

impl PaneId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl TabId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

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
            sessions: vec![WorkspaceSession::new(
                session_id,
                SessionKind::Terminal,
                "Terminal",
            )],
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

    pub fn update_terminal_output(&mut self, session_id: SessionId, output: String) {
        self.update_terminal_frame(session_id, TerminalFrame::from_text(output));
    }

    pub fn update_terminal_frame(&mut self, session_id: SessionId, frame: TerminalFrame) {
        for pane in self
            .panes
            .iter_mut()
            .filter(|pane| pane.session_id == Some(session_id) && pane.kind == PaneKind::Terminal)
        {
            pane.output.clone_from(&frame.text);
            pane.terminal_frame = Some(frame.clone());
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

    pub fn terminal_session_ids(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|session| session.kind == SessionKind::Terminal)
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
        self.sessions
            .iter()
            .filter(|session| !self.session_is_referenced(session.id))
            .map(|session| SessionSummary {
                id: session.id,
                kind: session.kind,
                title: session.title.clone(),
                attached: false,
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
            .map(Pane::title)
            .unwrap_or_else(|| "none".to_string())
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
            .map(Pane::title)
            .unwrap_or_else(|| "Empty".to_string())
    }

    fn ensure_session(&mut self, kind: PaneKind, session_id: Option<SessionId>) {
        let Some(session_id) = session_id else {
            return;
        };
        if self.sessions.iter().any(|session| session.id == session_id) {
            return;
        }

        self.sessions.push(WorkspaceSession::new(
            session_id,
            SessionKind::from(kind),
            session_title(kind),
        ));
    }

    fn session_pane_kind(&self, session_id: SessionId) -> Option<PaneKind> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| PaneKind::from(session.kind))
    }
}

impl WorkspaceSession {
    fn new(id: SessionId, kind: SessionKind, title: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            title: title.into(),
        }
    }
}

impl From<PaneKind> for SessionKind {
    fn from(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Terminal => Self::Terminal,
            PaneKind::Agent => Self::Agent,
        }
    }
}

impl From<SessionKind> for PaneKind {
    fn from(kind: SessionKind) -> Self {
        match kind {
            SessionKind::Terminal => Self::Terminal,
            SessionKind::Agent => Self::Agent,
        }
    }
}

impl LayoutNode {
    fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { first, second, .. } => {
                let mut panes = first.pane_ids();
                panes.extend(second.pane_ids());
                panes
            }
        }
    }

    fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Pane(pane_id) => Some(*pane_id),
            Self::Split { first, second, .. } => first.first_pane().or_else(|| second.first_pane()),
        }
    }

    fn without_pane(&self, pane_id: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) if *id == pane_id => None,
            Self::Pane(id) => Some(Self::Pane(*id)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without_pane(pane_id), second.without_pane(pane_id)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}

impl Pane {
    fn new(kind: PaneKind, session_id: Option<SessionId>) -> Self {
        let output = match kind {
            PaneKind::Terminal => crate::terminal::initial_terminal_text(),
            PaneKind::Agent => crate::plugins::builtin_agent_intro(),
        };
        Self {
            id: PaneId::new(),
            kind,
            session_id,
            terminal_frame: (kind == PaneKind::Terminal)
                .then(|| TerminalFrame::from_text(output.clone())),
            output,
        }
    }

    pub fn title(&self) -> String {
        session_title(self.kind).to_string()
    }
}

fn session_title(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Terminal",
        PaneKind::Agent => "AI Agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_pane_references_top_level_session() {
        let workspace = Workspace::mvp();

        assert_eq!(workspace.terminal_session_ids().len(), 1);
        assert!(workspace.active_terminal_session_id().is_some());
        assert_eq!(workspace.session_count(), 1);
    }

    #[test]
    fn split_creates_new_attachment_for_session() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        let pane_id = workspace.attach_session_to_split(session_id);

        assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
        assert_eq!(workspace.visible_terminal_session_id(1), Some(session_id));
        assert!(workspace.session_is_referenced(session_id));
    }

    #[test]
    fn detach_reports_session_and_removes_reference() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        let pane_id = workspace.attach_session_to_split(session_id);

        assert_eq!(workspace.detach_pane(pane_id), Some(session_id));
        assert!(!workspace.session_is_referenced(session_id));
        assert_eq!(workspace.detached_session_count(), 1);
    }

    #[test]
    fn detach_last_pane_removes_tab() {
        let mut workspace = Workspace::mvp();
        let pane_id = workspace.visible_pane_id(0).expect("initial pane");

        assert!(workspace.detach_pane(pane_id).is_some());
        assert!(workspace.visible_panes().is_empty());
    }

    #[test]
    fn close_visible_pane_keeps_last_pane() {
        let mut workspace = Workspace::mvp();

        assert_eq!(workspace.close_visible_pane(0), None);
        assert_eq!(workspace.visible_panes().len(), 1);
    }

    #[test]
    fn close_visible_pane_detaches_when_another_pane_remains() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.attach_session_to_split(session_id);

        assert_eq!(workspace.close_visible_pane(1), Some(session_id));
        assert_eq!(workspace.visible_panes().len(), 1);
        assert!(!workspace.session_is_referenced(session_id));
        assert_eq!(workspace.session_count(), 2);
        assert_eq!(workspace.detached_session_count(), 1);
    }

    #[test]
    fn detached_session_summaries_list_unattached_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.attach_session_to_split(session_id);
        workspace.close_visible_pane(1);

        assert_eq!(
            workspace.detached_session_summaries(),
            vec![SessionSummary {
                id: session_id,
                kind: SessionKind::Terminal,
                title: "Terminal".to_string(),
                attached: false,
            }]
        );
    }

    #[test]
    fn attach_existing_session_to_split_reuses_session_kind() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.open_tab(PaneKind::Agent, Some(session_id));
        workspace.close_tab_index(1);

        let pane_id = workspace
            .attach_existing_session_to_split(session_id)
            .expect("attached pane");

        assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
        assert_eq!(workspace.visible_panes()[1].kind, PaneKind::Agent);
        assert!(workspace.session_is_referenced(session_id));
        assert_eq!(workspace.detached_session_count(), 0);
    }

    #[test]
    fn opening_tab_is_reflected_in_tab_summaries() {
        let mut workspace = Workspace::mvp();

        workspace.open_tab(PaneKind::Agent, None);

        assert_eq!(
            workspace.tab_summaries(),
            vec![
                TabSummary {
                    index: 0,
                    title: "Terminal".to_string(),
                    active: false,
                    pane_count: 1,
                },
                TabSummary {
                    index: 1,
                    title: "AI Agent".to_string(),
                    active: true,
                    pane_count: 1,
                },
            ]
        );
    }

    #[test]
    fn activate_tab_index_switches_visible_panes() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);

        assert!(workspace.activate_tab_index(0));
        assert_eq!(workspace.visible_panes()[0].kind, PaneKind::Terminal);
        assert!(!workspace.activate_tab_index(9));
        assert_eq!(workspace.visible_panes()[0].kind, PaneKind::Terminal);
    }

    #[test]
    fn close_tab_index_keeps_last_tab() {
        let mut workspace = Workspace::mvp();

        assert!(workspace.close_tab_index(0).is_empty());
        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(workspace.visible_panes().len(), 1);
    }

    #[test]
    fn close_tab_index_removes_tab_panes_and_returns_sessions() {
        let mut workspace = Workspace::mvp();
        let first_session = workspace.active_terminal_session_id().expect("session");
        let second_session = SessionId::new();
        workspace.open_tab(PaneKind::Terminal, Some(second_session));

        assert_eq!(workspace.close_tab_index(1), vec![second_session]);
        assert_eq!(workspace.tab_count(), 1);
        assert!(workspace.session_is_referenced(first_session));
        assert!(!workspace.session_is_referenced(second_session));
        assert_eq!(workspace.session_count(), 2);
        assert_eq!(workspace.detached_session_count(), 1);
        assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
    }

    #[test]
    fn close_active_tab_activates_neighbor() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);
        workspace.open_tab(PaneKind::Terminal, Some(SessionId::new()));

        assert_eq!(workspace.tab_summaries()[2].active, true);
        assert_eq!(workspace.close_tab_index(2).len(), 1);
        assert_eq!(workspace.tab_count(), 2);
        assert_eq!(workspace.tab_summaries()[1].active, true);
        assert_eq!(workspace.active_title(), "AI Agent");
    }

    #[test]
    fn close_inactive_tab_preserves_active_tab() {
        let mut workspace = Workspace::mvp();
        let first_session = workspace.active_terminal_session_id().expect("session");
        workspace.open_tab(PaneKind::Agent, None);

        assert_eq!(workspace.active_title(), "AI Agent");
        assert_eq!(workspace.close_tab_index(0), vec![first_session]);
        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(workspace.active_title(), "AI Agent");
    }

    #[test]
    fn activate_visible_pane_switches_active_pane() {
        let mut workspace = Workspace::mvp();
        workspace.attach_session_to_split(SessionId::new());

        assert_eq!(workspace.active_visible_index(), 1);
        assert!(workspace.activate_visible_pane(0));
        assert_eq!(workspace.active_visible_index(), 0);
        assert!(!workspace.activate_visible_pane(5));
        assert_eq!(workspace.active_visible_index(), 0);
    }

    #[test]
    fn active_tab_index_tracks_active_tab() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);

        assert_eq!(workspace.active_tab_index(), 1);
        assert!(workspace.activate_tab_index(0));
        assert_eq!(workspace.active_tab_index(), 0);
    }

    #[test]
    fn terminate_session_removes_session_and_attachments() {
        let mut workspace = Workspace::mvp();
        let first_session = workspace.active_terminal_session_id().expect("session");
        let second_session = SessionId::new();
        workspace.attach_session_to_split(second_session);

        assert!(workspace.terminate_session(second_session));
        assert_eq!(workspace.session_count(), 1);
        assert!(!workspace.session_is_referenced(second_session));
        assert!(workspace.session_is_referenced(first_session));
        assert_eq!(workspace.visible_panes().len(), 1);
    }

    #[test]
    fn terminate_unknown_session_is_noop() {
        let mut workspace = Workspace::mvp();

        assert!(!workspace.terminate_session(SessionId::new()));
        assert_eq!(workspace.session_count(), 1);
        assert_eq!(workspace.visible_panes().len(), 1);
    }
}
