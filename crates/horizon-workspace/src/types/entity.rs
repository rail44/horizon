use crate::SessionId;

use super::{LayoutNode, PaneId, PaneKind, SessionKind, TabId};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub sessions: Vec<WorkspaceSession>,
    pub active_tab: TabId,
    pub next_terminal_display_number: usize,
    pub next_agent_display_number: usize,
    /// Workspace mode's cursor (`docs/workspace-mode-design.md`): `None`
    /// outside the mode, where the cursor is simply defined to be wherever
    /// focus is (see `Workspace::cursor_pane_id`) so the two can never
    /// drift apart by construction. `Some(pane_id)` while the mode is
    /// active -- a stable `PaneId` rather than a visible-pane index, so it
    /// survives across a directional move without needing to be re-derived
    /// from the tree's shape (`docs/recursive-layout-design.md`'s slice 4:
    /// `hjkl` resolves geometrically via `workspace::nav`, which only
    /// speaks in `PaneId`s). See `workspace::mode` for the state
    /// transitions.
    pub workspace_mode_cursor: Option<PaneId>,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub root: LayoutNode,
    pub active: PaneId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSession {
    pub id: SessionId,
    pub kind: SessionKind,
    pub display_number: usize,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub session_id: Option<SessionId>,
}

impl WorkspaceSession {
    pub fn new(id: SessionId, kind: SessionKind, display_number: usize) -> Self {
        Self {
            id,
            kind,
            display_number,
            title: session_title(kind, display_number),
        }
    }
}

impl Pane {
    pub fn new(kind: PaneKind, session_id: Option<SessionId>) -> Self {
        Self {
            id: PaneId::new(),
            kind,
            session_id,
        }
    }

    pub fn title(&self) -> String {
        pane_kind_title(self.kind).to_string()
    }
}

fn pane_kind_title(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Terminal",
        PaneKind::Agent => "AI Agent",
        PaneKind::View(view_kind) => view_kind.title(),
    }
}

fn session_title(kind: SessionKind, display_number: usize) -> String {
    match kind {
        SessionKind::Terminal => format!("Terminal #{display_number}"),
        SessionKind::Agent => format!("Agent #{display_number}"),
    }
}
