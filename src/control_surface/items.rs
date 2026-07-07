use crate::app::command_actions::{find_agent_turn_in_flight, find_pending_agent_approval};
use crate::app::commands::{command_entries, filter_command_entries, CommandId, CommandState};
use crate::control_surface::query::{normalize_palette_query, palette_matches};
use crate::control_surface::{
    PaletteItem, PaletteRow, PaletteStage, SessionManagerRow, ViewChooserRow,
};
use crate::session::Frames;
use crate::workspace::{PaneKind, Workspace};

pub(crate) fn command_state(workspace: &Workspace, frames: &Frames) -> CommandState {
    CommandState {
        tab_count: workspace.tab_count(),
        visible_pane_count: workspace.visible_panes().len(),
        has_active_session: workspace.active_session_id().is_some(),
        detached_session_count: workspace.detached_session_count(),
        has_pending_approval: find_pending_agent_approval(workspace, frames).is_some(),
        has_turn_in_flight: find_agent_turn_in_flight(workspace, frames).is_some(),
    }
}

/// Every session the workspace knows about, for the session manager modal
/// (`control_surface::view::session_manager`) -- detached sessions first
/// (the ones worth hunting for), then attached ones, each group ordered by
/// `display_number`. `sort_by_key`'s stability keeps ties (e.g. a terminal
/// and an agent sharing the same `display_number`) in their original
/// `Workspace::session_summaries` order rather than reshuffling them.
pub(crate) fn session_manager_items(workspace: &Workspace) -> Vec<SessionManagerRow> {
    let mut rows: Vec<SessionManagerRow> = workspace
        .session_summaries()
        .into_iter()
        .map(|session| SessionManagerRow {
            session_id: session.id,
            kind: session.kind,
            display_number: session.display_number,
            title: session.title,
            attached: session.attached,
        })
        .collect();
    rows.sort_by_key(|row| (row.attached, row.display_number));
    rows
}

pub(crate) fn palette_items(
    workspace: &Workspace,
    frames: &Frames,
    query: &str,
) -> Vec<PaletteItem> {
    // `TerminateAllDetachedSessions` is excluded from the generic mapping
    // below: `PaletteItem::Command` only ever renders `spec.title` verbatim
    // (a static string), but this row's title must show the live detached
    // count, and it must not be listed at all when there is nothing to
    // clean up. Both are handled by the dedicated
    // `PaletteItem::TerminateAllDetached` row appended further down —
    // mirroring how `PaletteItem::TerminateSession` carries a per-session
    // label instead of going through `Command(CommandEntry)`.
    let mut items: Vec<_> =
        filter_command_entries(command_entries(command_state(workspace, frames)), query)
            .into_iter()
            .filter(|entry| entry.spec.id != CommandId::TerminateAllDetachedSessions)
            .map(PaletteItem::Command)
            .collect();
    let query = normalize_palette_query(query);

    let detached_sessions = workspace.detached_session_summaries();
    if !detached_sessions.is_empty()
        && palette_matches(
            &query,
            &[
                "terminate",
                "all",
                "detached",
                "sessions",
                "cleanup",
                "bulk",
            ],
        )
    {
        items.push(PaletteItem::TerminateAllDetached {
            count: detached_sessions.len(),
        });
    }

    items.extend(
        detached_sessions
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

/// The palette's second-stage view chooser rows (`PaletteStage::
/// ViewChooser`) -- registry-driven, per `docs/roadmap.md`'s "Placement-
/// first session creation": `Terminal`/`Agent` (no role) first, then one
/// row per `horizon_agent::roles::all()` entry, sorted by role id for a
/// deterministic list (future user-defined agents and WASM views are meant
/// to extend this list, not add new palette commands). Unlike
/// `palette_items`, this needs no `Workspace`/`Frames` snapshot -- every row
/// is a fixed `(kind, role_id)` pair, not derived from live session state.
pub(crate) fn view_chooser_rows(query: &str) -> Vec<ViewChooserRow> {
    let query = normalize_palette_query(query);

    let mut roles: Vec<&horizon_agent::roles::RoleDefinition> =
        horizon_agent::roles::all().to_vec();
    roles.sort_by_key(|role| role.id);

    let mut rows = vec![
        ViewChooserRow {
            kind: PaneKind::Terminal,
            role_id: None,
            title: "Terminal".to_string(),
        },
        ViewChooserRow {
            kind: PaneKind::Agent,
            role_id: None,
            title: "Agent".to_string(),
        },
    ];
    rows.extend(roles.into_iter().map(|role| ViewChooserRow {
        kind: PaneKind::Agent,
        role_id: Some(horizon_agent::roles::RoleId(role.id.to_string())),
        title: role.title.to_string(),
    }));

    rows.into_iter()
        .filter(|row| query.is_empty() || normalize_palette_query(&row.title).contains(&query))
        .collect()
}

/// The single stage-branching point for "what rows does the palette show
/// right now" -- `view::palette`'s rendering and every navigation function
/// in `actions` (`move_palette_selection`, `clamp_current_palette_selection`
/// via `update_palette_query`, `execute_palette_selection`) all go through
/// this rather than re-deciding `Commands` vs `ViewChooser` themselves, so
/// the row count and the row list can never disagree.
pub(crate) fn palette_rows(
    workspace: &Workspace,
    frames: &Frames,
    stage: PaletteStage,
    query: &str,
) -> Vec<PaletteRow> {
    match stage {
        PaletteStage::Commands => palette_items(workspace, frames, query)
            .into_iter()
            .map(PaletteRow::Catalog)
            .collect(),
        PaletteStage::ViewChooser { .. } => view_chooser_rows(query)
            .into_iter()
            .map(PaletteRow::Chooser)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::workspace::SessionKind;

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
                detached_session_count: 0,
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
                detached_session_count: 0,
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
    fn palette_items_offer_terminate_all_with_live_count_only_when_sessions_are_detached() {
        let mut workspace = Workspace::mvp();

        assert!(!palette_items(&workspace, &Frames::default(), "cleanup")
            .iter()
            .any(|item| matches!(item, PaletteItem::TerminateAllDetached { .. })));

        let first = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(first));
        workspace.close_visible_pane(1);
        let second = SessionId::new();
        workspace.split_active(PaneKind::Agent, Some(second));
        workspace.close_visible_pane(1);

        let items = palette_items(&workspace, &Frames::default(), "cleanup");
        assert!(items
            .iter()
            .any(|item| matches!(item, PaletteItem::TerminateAllDetached { count: 2 })));
        // The catalog `Command` row for the same `CommandId` is never
        // listed — the dynamic-count row above replaces it.
        assert!(!items.iter().any(|item| matches!(
            item,
            PaletteItem::Command(entry) if entry.spec.id == CommandId::TerminateAllDetachedSessions
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
    fn session_manager_items_list_detached_sessions_before_attached_ones() {
        let mut workspace = Workspace::mvp();
        let detached = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(detached));
        workspace.close_visible_pane(1);

        let items = session_manager_items(&workspace);

        assert_eq!(items.len(), 2);
        assert!(!items[0].attached);
        assert_eq!(items[0].session_id, detached);
        assert!(items[1].attached);
    }

    #[test]
    fn session_manager_items_sort_each_group_by_display_number() {
        let mut workspace = Workspace::mvp();
        let second = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(second));
        workspace.close_visible_pane(1);
        let third = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(third));
        workspace.close_visible_pane(1);

        let items = session_manager_items(&workspace);

        let detached_numbers: Vec<usize> = items
            .iter()
            .filter(|row| !row.attached)
            .map(|row| row.display_number)
            .collect();
        assert_eq!(detached_numbers, vec![2, 3]);
    }

    #[test]
    fn view_chooser_rows_lists_kinds_before_roles() {
        let rows = view_chooser_rows("");

        // Terminal, then Agent (no role), then every registered role --
        // today just `config`, but the order (kinds first, roles sorted by
        // id) must hold regardless of how many roles exist.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, PaneKind::Terminal);
        assert_eq!(rows[0].role_id, None);
        assert_eq!(rows[0].title, "Terminal");
        assert_eq!(rows[1].kind, PaneKind::Agent);
        assert_eq!(rows[1].role_id, None);
        assert_eq!(rows[1].title, "Agent");
        assert_eq!(rows[2].kind, PaneKind::Agent);
        assert_eq!(
            rows[2].role_id,
            Some(horizon_agent::roles::RoleId("config".to_string()))
        );
        assert_eq!(rows[2].title, "Configuration Agent");
    }

    #[test]
    fn view_chooser_rows_filters_by_title() {
        let rows = view_chooser_rows("config");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Configuration Agent");
    }

    #[test]
    fn palette_rows_commands_stage_wraps_palette_items() {
        let workspace = Workspace::mvp();
        let frames = Frames::default();

        let rows = palette_rows(&workspace, &frames, PaletteStage::Commands, "split pane");

        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            PaletteRow::Catalog(PaletteItem::Command(entry)) if entry.spec.id == CommandId::SplitPane
        ));
    }

    #[test]
    fn palette_rows_view_chooser_stage_wraps_chooser_rows() {
        let workspace = Workspace::mvp();
        let frames = Frames::default();

        let rows = palette_rows(
            &workspace,
            &frames,
            PaletteStage::ViewChooser {
                placement: crate::control_surface::Placement::NewTab,
            },
            "",
        );

        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], PaletteRow::Chooser(row) if row.kind == PaneKind::Terminal));
    }
}
