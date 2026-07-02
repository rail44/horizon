use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::SessionId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct PaneId(Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct TabId(Uuid);

#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    pub(super) tabs: Vec<Tab>,
    pub(super) panes: Vec<Pane>,
    pub(super) sessions: Vec<WorkspaceSession>,
    pub(super) active_tab: TabId,
    pub(super) next_terminal_display_number: usize,
    pub(super) next_agent_display_number: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) root: LayoutNode,
    pub(crate) active: PaneId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabSummary {
    pub(crate) index: usize,
    pub(crate) title: String,
    pub(crate) active: bool,
    pub(crate) pane_count: usize,
    pub(crate) active_session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSummary {
    pub(crate) id: SessionId,
    pub(crate) kind: SessionKind,
    pub(crate) display_number: usize,
    pub(crate) title: String,
    pub(crate) attached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaneSummary {
    pub(crate) tab_index: usize,
    pub(crate) pane_index: usize,
    pub(crate) title: String,
    pub(crate) kind: PaneKind,
    pub(crate) active: bool,
    pub(crate) tab_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSession {
    pub(crate) id: SessionId,
    pub(crate) kind: SessionKind,
    pub(crate) display_number: usize,
    pub(crate) title: String,
}

#[derive(Clone, Debug)]
pub(crate) enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitAxis {
    Horizontal,
}

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub(crate) id: PaneId,
    pub(crate) kind: PaneKind,
    pub(crate) session_id: Option<SessionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneKind {
    Terminal,
    Agent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionKind {
    Terminal,
    Agent,
}

impl PaneId {
    pub(super) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl TabId {
    pub(super) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl WorkspaceSession {
    pub(super) fn new(id: SessionId, kind: SessionKind, display_number: usize) -> Self {
        Self {
            id,
            kind,
            display_number,
            title: session_title(kind, display_number),
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

impl PaneKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }
}

impl SessionKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }
}

impl Pane {
    pub(super) fn new(kind: PaneKind, session_id: Option<SessionId>) -> Self {
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
