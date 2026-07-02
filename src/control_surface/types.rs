use crate::commands::CommandEntry;
use crate::session::SessionId;
use crate::workspace::{PaneKind, PaneSummary, SessionKind};

pub const PALETTE_VISIBLE_ROWS: usize = 6;
pub const OVERVIEW_VISIBLE_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlMode {
    Commands,
    Workspace,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteItem {
    Command(CommandEntry),
    DetachedSession {
        session_id: SessionId,
        kind: SessionKind,
        display_number: usize,
        title: String,
    },
    Tab {
        index: usize,
        title: String,
        pane_count: usize,
        active: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum OverviewItem {
    Tab {
        index: usize,
        title: String,
        pane_count: usize,
        active: bool,
    },
    DetachedSession {
        session_id: SessionId,
        title: String,
        kind: SessionKind,
        display_number: usize,
    },
    Pane {
        tab_index: usize,
        pane_index: usize,
        title: String,
        kind: PaneKind,
        active: bool,
        tab_active: bool,
    },
}

impl PaletteItem {
    pub fn enabled(&self) -> bool {
        match self {
            Self::Command(entry) => entry.enabled,
            Self::DetachedSession { .. } | Self::Tab { .. } => true,
        }
    }
}

impl From<PaneSummary> for OverviewItem {
    fn from(pane: PaneSummary) -> Self {
        Self::Pane {
            tab_index: pane.tab_index,
            pane_index: pane.pane_index,
            title: pane.title,
            kind: pane.kind,
            active: pane.active,
            tab_active: pane.tab_active,
        }
    }
}
