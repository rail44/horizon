use crate::app::command_actions::{find_agent_turn_in_flight, find_pending_agent_approval};
use crate::app::commands::{command_entries, filter_command_entries, CommandState};
use crate::control_surface::query::{normalize_palette_query, palette_matches};
use crate::control_surface::{OverviewItem, PaletteItem};
use crate::session::Frames;
use crate::workspace::Workspace;

pub(crate) fn command_state(workspace: &Workspace, frames: &Frames) -> CommandState {
    CommandState {
        tab_count: workspace.tab_count(),
        visible_pane_count: workspace.visible_panes().len(),
        has_active_session: workspace.active_session_id().is_some(),
        has_pending_approval: find_pending_agent_approval(workspace, frames).is_some(),
        has_turn_in_flight: find_agent_turn_in_flight(workspace, frames).is_some(),
    }
}

pub(crate) fn overview_items(workspace: &Workspace) -> Vec<OverviewItem> {
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

pub(crate) fn palette_items(
    workspace: &Workspace,
    frames: &Frames,
    query: &str,
) -> Vec<PaletteItem> {
    let mut items: Vec<_> =
        filter_command_entries(command_entries(command_state(workspace, frames)), query)
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

    let active_session_id = workspace.active_session_id();
    items.extend(
        workspace
            .session_summaries()
            .into_iter()
            .filter(|session| Some(session.id) != active_session_id)
            .filter(|session| {
                let display_number = session.display_number.to_string();
                palette_matches(
                    &query,
                    &[
                        "terminate",
                        "kill",
                        "end session",
                        session.title.as_str(),
                        session.kind.label(),
                        display_number.as_str(),
                    ],
                )
            })
            .map(|session| PaletteItem::TerminateSession {
                session_id: session.id,
                kind: session.kind,
                display_number: session.display_number,
                title: session.title,
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
        let frames = Frames::default();
        assert_eq!(
            command_state(&workspace, &frames),
            CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        );

        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        assert_eq!(
            command_state(&workspace, &frames),
            CommandState {
                tab_count: 1,
                visible_pane_count: 2,
                has_active_session: true,
                has_pending_approval: false,
                has_turn_in_flight: false,
            }
        );
    }

    #[test]
    fn palette_items_include_detached_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);

        let items = palette_items(&workspace, &Frames::default(), "detached");

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

        let items = palette_items(&workspace, &Frames::default(), "tab 1");

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
    fn palette_items_offer_terminate_for_non_active_sessions_but_not_the_active_one() {
        let mut workspace = Workspace::mvp();
        let active_session = workspace.active_terminal_session_id().expect("session");
        let other_session = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(other_session));
        workspace.close_visible_pane(1);

        let items = palette_items(&workspace, &Frames::default(), "terminate");

        assert!(items.iter().any(|item| matches!(
            item,
            PaletteItem::TerminateSession {
                session_id: id,
                kind: SessionKind::Terminal,
                ..
            } if *id == other_session
        )));
        assert!(!items.iter().any(|item| matches!(
            item,
            PaletteItem::TerminateSession { session_id: id, .. } if *id == active_session
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
