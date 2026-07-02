mod query;
mod types;
pub mod view;

pub use types::{
    ControlMode, OverviewItem, PaletteItem, OVERVIEW_VISIBLE_ROWS, PALETTE_VISIBLE_ROWS,
};

use crate::commands::{command_entries, filter_command_entries, CommandState};
use crate::control_surface::query::{normalize_palette_query, palette_matches};
use crate::workspace::Workspace;

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
                        session.kind.label(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::workspace::{PaneKind, SessionKind};

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
}
