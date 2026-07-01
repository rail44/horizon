use floem::peniko::Color;

use crate::commands::CommandEntry;
use crate::session::SessionId;
use crate::workspace::{PaneKind, PaneSummary, SessionKind};

use super::query::{pane_kind_label, session_kind_label};

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
    pub fn kind_label(&self) -> String {
        match self {
            Self::Command(_) => "COMMAND".to_string(),
            Self::DetachedSession { .. } => "SESSION".to_string(),
            Self::Tab { .. } => "TAB".to_string(),
        }
    }

    pub fn kind_color(&self) -> Color {
        match self {
            Self::Command(_) => Color::rgb8(132, 220, 198),
            Self::DetachedSession { .. } => Color::rgb8(126, 170, 255),
            Self::Tab { .. } => Color::rgb8(224, 184, 104),
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::Command(entry) => entry.spec.title.to_string(),
            Self::DetachedSession { title, .. } => format!("Attach {title}"),
            Self::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::Command(entry) => entry.spec.description.to_string(),
            Self::DetachedSession {
                kind,
                display_number,
                ..
            } => {
                format!(
                    "Detached {} session #{}; attach to the active tab as a split.",
                    session_kind_label(*kind),
                    display_number
                )
            }
            Self::Tab {
                pane_count, active, ..
            } => {
                if *active {
                    format!("Current tab with {pane_count} pane(s).")
                } else {
                    format!("Switch to tab with {pane_count} pane(s).")
                }
            }
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Self::Command(entry) => entry.enabled,
            Self::DetachedSession { .. } | Self::Tab { .. } => true,
        }
    }
}

impl OverviewItem {
    pub fn kind_label(&self) -> String {
        match self {
            Self::Tab { .. } => "TAB".to_string(),
            Self::DetachedSession { .. } => "DETACHED".to_string(),
            Self::Pane { .. } => "PANE".to_string(),
        }
    }

    pub fn kind_color(&self) -> Color {
        match self {
            Self::Tab { .. } => Color::rgb8(224, 184, 104),
            Self::DetachedSession { .. } => Color::rgb8(126, 170, 255),
            Self::Pane { .. } => Color::rgb8(132, 220, 198),
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
            Self::DetachedSession { title, .. } => format!("Attach {title}"),
            Self::Pane {
                tab_index,
                pane_index,
                title,
                ..
            } => format!("Tab {} / Pane {}: {title}", tab_index + 1, pane_index + 1),
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::Tab {
                pane_count, active, ..
            } => {
                if *active {
                    format!("Current tab · {pane_count} pane(s)")
                } else {
                    format!("Switch to tab · {pane_count} pane(s)")
                }
            }
            Self::DetachedSession {
                kind,
                display_number,
                ..
            } => format!(
                "Detached {} session #{} · Enter attaches as split",
                session_kind_label(*kind),
                display_number
            ),
            Self::Pane {
                kind,
                active,
                tab_active,
                ..
            } => {
                let state = if *tab_active && *active {
                    "Active pane"
                } else if *tab_active {
                    "Visible pane"
                } else {
                    "Pane in inactive tab"
                };
                format!("{state} · {} pane", pane_kind_label(*kind))
            }
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
