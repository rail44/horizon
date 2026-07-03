use crate::session::SessionId;

use super::{PaneKind, SessionKind};

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
