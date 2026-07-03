use crate::session::SessionId;

use super::{LayoutNode, PaneId, PaneKind, SessionKind, TabId};

#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    pub(in crate::workspace) tabs: Vec<Tab>,
    pub(in crate::workspace) panes: Vec<Pane>,
    pub(in crate::workspace) sessions: Vec<WorkspaceSession>,
    pub(in crate::workspace) active_tab: TabId,
    pub(in crate::workspace) next_terminal_display_number: usize,
    pub(in crate::workspace) next_agent_display_number: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) root: LayoutNode,
    pub(crate) active: PaneId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSession {
    pub(crate) id: SessionId,
    pub(crate) kind: SessionKind,
    pub(crate) display_number: usize,
    pub(crate) title: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub(crate) id: PaneId,
    pub(crate) kind: PaneKind,
    pub(crate) session_id: Option<SessionId>,
}

impl WorkspaceSession {
    pub(in crate::workspace) fn new(
        id: SessionId,
        kind: SessionKind,
        display_number: usize,
    ) -> Self {
        Self {
            id,
            kind,
            display_number,
            title: session_title(kind, display_number),
        }
    }
}

impl Pane {
    pub(in crate::workspace) fn new(kind: PaneKind, session_id: Option<SessionId>) -> Self {
        Self {
            id: PaneId::new(),
            kind,
            session_id,
        }
    }

    pub(crate) fn title(&self) -> String {
        pane_kind_title(self.kind).to_string()
    }
}

fn pane_kind_title(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Terminal",
        PaneKind::Agent => "AI Agent",
    }
}

fn session_title(kind: SessionKind, display_number: usize) -> String {
    match kind {
        SessionKind::Terminal => format!("Terminal #{display_number}"),
        SessionKind::Agent => format!("Agent #{display_number}"),
    }
}
