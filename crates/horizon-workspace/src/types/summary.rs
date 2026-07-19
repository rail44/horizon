use std::path::PathBuf;

use crate::SessionId;

use super::{PaneKind, SessionKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabSummary {
    pub index: usize,
    pub title: String,
    pub active: bool,
    pub pane_count: usize,
    pub active_session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub kind: SessionKind,
    pub display_number: usize,
    pub title: String,
    pub attached: bool,
    /// Mirrors `WorkspaceSession::workspace_root` -- see its doc comment.
    pub workspace_root: Option<PathBuf>,
    /// Mirrors `WorkspaceSession::parent_session_id` -- see its doc
    /// comment. The session manager modal's lineage view
    /// (`docs/session-relationship-design.md` decision 4b) derives its
    /// derivation tree from this field across every summary.
    pub parent_session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSummary {
    pub tab_index: usize,
    pub pane_index: usize,
    pub title: String,
    pub kind: PaneKind,
    pub active: bool,
    pub tab_active: bool,
}
