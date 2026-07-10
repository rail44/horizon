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
