use floem::peniko::Color;

use crate::commands::{command_entries, filter_command_entries, CommandEntry, CommandState};
use crate::workspace::{PaneKind, PaneSummary, SessionId, SessionKind, Workspace};

pub const PALETTE_VISIBLE_ROWS: usize = 6;
pub const OVERVIEW_VISIBLE_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlMode {
    Commands,
    Workspace,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
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

pub fn command_state(workspace: &Workspace) -> CommandState {
    CommandState {
        tab_count: workspace.tab_count(),
        visible_pane_count: workspace.visible_panes().len(),
        has_active_session: workspace.active_session_id().is_some(),
    }
}

pub fn overview_items(workspace: &Workspace) -> Vec<OverviewItem> {
    let tabs = workspace.tab_summaries();
    let panes = workspace.pane_summaries();
    let mut items = Vec::new();

    for tab in tabs {
        let tab_index = tab.index;
        let pane_count = tab.pane_count;
        items.push(OverviewItem::Tab {
            index: tab.index,
            title: tab.title,
            pane_count: tab.pane_count,
            active: tab.active,
        });

        if pane_count > 1 {
            items.extend(
                panes
                    .iter()
                    .filter(|pane| pane.tab_index == tab_index)
                    .cloned()
                    .map(OverviewItem::from),
            );
        }
    }

    items.extend(
        workspace
            .detached_session_summaries()
            .into_iter()
            .map(|session| OverviewItem::DetachedSession {
                session_id: session.id,
                title: session.title,
                kind: session.kind,
                display_number: session.display_number,
            }),
    );

    items
}

pub fn overview_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= OVERVIEW_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(OVERVIEW_VISIBLE_ROWS - 1)
}

pub fn palette_items(workspace: &Workspace, query: &str) -> Vec<PaletteItem> {
    let mut items: Vec<_> =
        filter_command_entries(command_entries(command_state(workspace)), query)
            .into_iter()
            .map(PaletteItem::Command)
            .collect();
    let query = normalize_palette_query(query);

    items.extend(
        workspace
            .detached_session_summaries()
            .into_iter()
            .filter(|session| {
                let display_number = session.display_number.to_string();
                palette_matches(
                    &query,
                    &[
                        "detached",
                        "session",
                        session.title.as_str(),
                        session_kind_label(session.kind),
                        display_number.as_str(),
                    ],
                )
            })
            .map(|session| PaletteItem::DetachedSession {
                session_id: session.id,
                kind: session.kind,
                display_number: session.display_number,
                title: session.title,
            }),
    );

    items.extend(
        workspace
            .tab_summaries()
            .into_iter()
            .filter(|tab| {
                let index_label = format!("tab {}", tab.index + 1);
                palette_matches(
                    &query,
                    &["tab", index_label.as_str(), tab.title.as_str(), "switch"],
                )
            })
            .map(|tab| PaletteItem::Tab {
                index: tab.index,
                title: tab.title,
                pane_count: tab.pane_count,
                active: tab.active,
            }),
    );

    items
}

pub fn palette_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= PALETTE_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(PALETTE_VISIBLE_ROWS - 1)
}

fn palette_matches(query: &str, fields: &[&str]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .any(|field| normalize_palette_query(field).contains(query))
}

fn normalize_palette_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn session_kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "terminal",
        SessionKind::Agent => "agent",
    }
}

fn pane_kind_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "terminal",
        PaneKind::Agent => "agent",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{PaneKind, SessionId, SessionKind};

    #[test]
    fn command_state_reflects_workspace_counts() {
        let mut workspace = Workspace::mvp();
        assert_eq!(
            command_state(&workspace),
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
            }
        );

        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        assert_eq!(
            command_state(&workspace),
            CommandState {
                tab_count: 1,
                visible_pane_count: 2,
                has_active_session: true,
            }
        );
    }

    #[test]
    fn palette_items_include_detached_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);

        let items = palette_items(&workspace, "detached");

        assert!(items.iter().any(|item| matches!(
            item,
            PaletteItem::DetachedSession {
                session_id: id,
                kind: SessionKind::Terminal,
                ..
            } if *id == session_id
        )));
    }

    #[test]
    fn palette_items_include_tabs_by_index() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);

        let items = palette_items(&workspace, "tab 1");

        assert!(items.iter().any(|item| matches!(
            item,
            PaletteItem::Tab {
                index: 0,
                title,
                active: false,
                ..
            } if title == "Terminal #1"
        )));
    }

    #[test]
    fn overview_items_include_tabs_and_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);

        let items = overview_items(&workspace);

        assert!(matches!(
            items[0],
            OverviewItem::Tab {
                index: 0,
                active: true,
                ..
            }
        ));
        assert!(!items.iter().any(
            |item| matches!(item, OverviewItem::Pane { title, .. } if title == "Terminal #1")
        ));
        assert!(items.iter().any(|item| matches!(
            item,
            OverviewItem::DetachedSession {
                session_id: id,
                title,
                ..
            } if *id == session_id && title == "Terminal #2"
        )));
    }

    #[test]
    fn overview_items_include_split_panes_under_their_tab() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

        let items = overview_items(&workspace);

        assert!(matches!(
            &items[0],
            OverviewItem::Tab {
                title,
                active: true,
                ..
            } if title == "Terminal #2"
        ));
        assert!(matches!(
            &items[1],
            OverviewItem::Pane {
                tab_index: 0,
                pane_index: 0,
                title,
                kind: PaneKind::Terminal,
                active: false,
                tab_active: true,
            } if title == "Terminal #1"
        ));
        assert!(matches!(
            &items[2],
            OverviewItem::Pane {
                tab_index: 0,
                pane_index: 1,
                title,
                kind: PaneKind::Terminal,
                active: true,
                tab_active: true,
            } if title == "Terminal #2"
        ));
    }

    #[test]
    fn overview_visible_start_keeps_selection_in_rendered_rows() {
        assert_eq!(overview_visible_start(0, 12), 0);
        assert_eq!(overview_visible_start(7, 12), 0);
        assert_eq!(overview_visible_start(8, 12), 1);
        assert_eq!(overview_visible_start(11, 12), 4);
    }

    #[test]
    fn palette_visible_start_keeps_selection_in_rendered_rows() {
        assert_eq!(palette_visible_start(0, 10), 0);
        assert_eq!(palette_visible_start(5, 10), 0);
        assert_eq!(palette_visible_start(6, 10), 1);
        assert_eq!(palette_visible_start(9, 10), 4);
    }

    #[test]
    fn palette_visible_start_handles_short_lists() {
        assert_eq!(palette_visible_start(0, 0), 0);
        assert_eq!(palette_visible_start(3, 4), 0);
    }
}
