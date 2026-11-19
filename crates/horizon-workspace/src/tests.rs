#[path = "tests/groups.rs"]
mod groups;

use super::types::{LayoutNode, PaneSummary, SessionSummary, TabSummary};
use super::*;
use crate::SessionId;
use crate::operations::Content;

#[test]
fn terminal_pane_references_top_level_session() {
    let workspace = Workspace::mvp();

    assert_eq!(workspace.session_summaries()[0].kind, SessionKind::Terminal);
    assert!(workspace.active_terminal_session_id().is_some());
    assert_eq!(workspace.session_count(), 1);
}

#[test]
fn split_creates_new_attachment_for_session() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let pane_id = workspace.split_active(SessionKind::Terminal, session_id);

    assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
    assert_eq!(workspace.visible_terminal_session_id(1), Some(session_id));
    assert!(workspace.session_is_referenced(session_id));
}

#[test]
fn pane_id_accessors_resolve_kind_session_and_title_for_a_specific_pane() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let pane_id = workspace.split_active(SessionKind::Agent, session_id);

    assert_eq!(
        workspace
            .pane_plugin(pane_id)
            .map(|plugin| plugin.name.as_str()),
        Some("agent")
    );
    assert_eq!(workspace.agent_session_id(pane_id), Some(session_id));
    assert_eq!(workspace.terminal_session_id(pane_id), None);
    assert!(workspace.pane_title_for(pane_id).is_some());
    // The other (unrelated) pane must not be confused with this one.
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    assert_ne!(workspace.agent_session_id(first_pane_id), Some(session_id));
}

#[test]
fn a_view_pane_has_no_session_and_registers_none() {
    let mut workspace = Workspace::mvp();
    let session_count_before = workspace.session_count();
    let pane_id = workspace.open_tab(Content::View(view()));

    assert_eq!(
        workspace
            .pane_plugin(pane_id)
            .map(|plugin| plugin.name.as_str()),
        Some("preview")
    );
    assert_eq!(workspace.active_session_id(), None);
    // No session is created for a view pane -- `ensure_session` no-ops on
    // `None`, so the session count is untouched.
    assert_eq!(workspace.session_count(), session_count_before);
    assert_eq!(
        workspace.pane_title_for(pane_id),
        Some("Preview".to_string())
    );
}

#[test]
fn split_active_tab_with_view_adds_a_session_less_pane_beside_the_focus() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let session_count_before = workspace.session_count();

    let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);

    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane, view_pane]);
    assert_eq!(
        workspace
            .pane_plugin(view_pane)
            .map(|plugin| plugin.name.as_str()),
        Some("preview")
    );
    assert_eq!(workspace.session_count(), session_count_before);
    // Splitting dives into the new pane, same as every other split.
    assert!(workspace.is_active_pane(view_pane));
}

#[test]
fn closing_a_view_pane_detaches_no_session() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);

    // Trivially destructive: nothing to detach, since there is no session.
    assert_eq!(workspace.close_pane(view_pane), None);
    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane]);
}

/// A view pane's plugin, for a test about the pane rather than the view.
fn view() -> PluginPane {
    plugin("preview", "Preview", serde_json::Value::Null)
}

fn plugin(name: &str, title: &str, state: serde_json::Value) -> PluginPane {
    PluginPane {
        name: name.to_string(),
        title: title.to_string(),
        state,
    }
}

#[test]
fn a_plugin_pane_has_no_session_and_registers_none() {
    let mut workspace = Workspace::mvp();
    let session_count_before = workspace.session_count();
    let pane_id = workspace.open_tab(Content::View(view()));
    assert!(workspace.set_pane_plugin(
        pane_id,
        plugin("board-list", "Board", serde_json::Value::Null)
    ));

    assert_eq!(
        workspace
            .pane_plugin(pane_id)
            .map(|plugin| plugin.name.as_str()),
        Some("board-list")
    );
    assert_eq!(workspace.active_session_id(), None);
    assert_eq!(workspace.session_count(), session_count_before);
    assert_eq!(workspace.pane_title_for(pane_id), Some("Board".to_string()));
}

#[test]
fn split_active_tab_with_a_plugin_view_adds_a_session_less_pane_beside_the_focus() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let session_count_before = workspace.session_count();

    let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);

    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane, view_pane]);
    assert_eq!(
        workspace
            .pane_plugin(view_pane)
            .map(|plugin| plugin.name.as_str()),
        Some("preview")
    );
    assert_eq!(workspace.session_count(), session_count_before);
    assert!(workspace.is_active_pane(view_pane));
}

#[test]
fn closing_a_plugin_pane_detaches_no_session() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);

    assert_eq!(workspace.close_pane(view_pane), None);
    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane]);
}

/// The plugin and its state ride on the pane: only a plugin view takes
/// them, and its state is changed in place.
#[test]
fn a_plugin_view_carries_its_plugin_and_state() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let thread = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
    let shown = plugin("board-thread", "Thread", serde_json::json!({"task": 7}));
    assert!(workspace.set_pane_plugin(thread, shown.clone()));
    assert_eq!(workspace.pane_plugin(thread), Some(&shown));

    let moved = plugin("board-thread", "Thread", serde_json::json!({"task": 12}));
    assert!(workspace.set_pane_plugin(thread, moved.clone()));
    assert_eq!(workspace.pane_plugin(thread), Some(&moved));
    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane, thread]);

    // A session-backed pane is not a plugin view, and an unknown id is
    // not a pane at all.
    assert!(!workspace.set_pane_plugin(terminal_pane, shown.clone()));
    assert!(!workspace.set_pane_plugin(PaneId::new(), shown));
}

/// What the board's linking reads: the panes of one pane's own tab, in
/// layout order, and nothing from any other tab.
#[test]
fn tab_panes_reports_the_panes_of_that_panes_own_tab() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let list = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
    let thread = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
    let elsewhere = workspace.open_tab_with_view_activated(view(), false);

    assert_eq!(workspace.tab_panes(list), vec![terminal_pane, list, thread]);
    assert_eq!(workspace.tab_panes(elsewhere), vec![elsewhere]);
    assert!(workspace.tab_panes(PaneId::new()).is_empty());
}

#[test]
fn opening_a_view_tab_without_activating_leaves_the_active_tab_alone() {
    let mut workspace = Workspace::mvp();
    let first_tab = workspace.active_tab_index();

    let pane_id = workspace.open_tab_with_view_activated(view(), false);

    assert_eq!(workspace.tab_count(), 2);
    assert_eq!(workspace.active_tab_index(), first_tab);
    // The pane exists and is addressable even though nothing dove into it.
    assert_eq!(
        workspace
            .pane_plugin(pane_id)
            .map(|plugin| plugin.name.as_str()),
        Some("preview")
    );
}

#[test]
fn splitting_a_target_session_with_a_view_adds_a_session_less_pane_beside_it() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let target = workspace.active_session_id().expect("the mvp terminal");
    let session_count_before = workspace.session_count();

    let pane_id = workspace
        .split_session_with_view(target, view(), SplitAxis::Horizontal, true)
        .expect("the target session has a pane");

    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane, pane_id]);
    assert_eq!(
        workspace
            .pane_plugin(pane_id)
            .map(|plugin| plugin.name.as_str()),
        Some("preview")
    );
    assert_eq!(workspace.session_count(), session_count_before);
}

#[test]
fn splitting_an_unknown_session_with_a_view_spawns_nothing() {
    let mut workspace = Workspace::mvp();
    let panes_before = workspace.visible_pane_ids();

    assert_eq!(
        workspace.split_session_with_view(SessionId::new(), view(), SplitAxis::Horizontal, true),
        None
    );
    assert_eq!(workspace.visible_pane_ids(), panes_before);
}

#[test]
fn reveal_pane_shows_the_tab_a_pane_is_in_and_focuses_it() {
    let mut workspace = Workspace::mvp();
    let first_pane = workspace.visible_pane_id(0).expect("first pane");
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));
    assert_eq!(workspace.active_tab_index(), 1);

    assert!(workspace.reveal_pane(first_pane));
    assert_eq!(workspace.active_tab_index(), 0);
    assert!(workspace.is_active_pane(first_pane));
    assert!(!workspace.reveal_pane(PaneId::new()));
}

#[test]
fn is_active_pane_reflects_the_tabs_own_active_pane_by_id() {
    let mut workspace = Workspace::mvp();
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    let second_pane_id = workspace.split_active(SessionKind::Terminal, SessionId::new());

    // split_active dives into the new pane.
    assert!(workspace.is_active_pane(second_pane_id));
    assert!(!workspace.is_active_pane(first_pane_id));

    workspace.activate_visible_pane(0);

    assert!(workspace.is_active_pane(first_pane_id));
    assert!(!workspace.is_active_pane(second_pane_id));
}

#[test]
fn visible_index_of_resolves_a_pane_id_to_its_visible_position() {
    let mut workspace = Workspace::mvp();
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    let second_pane_id = workspace.split_active(SessionKind::Terminal, SessionId::new());

    assert_eq!(workspace.visible_index_of(first_pane_id), Some(0));
    assert_eq!(workspace.visible_index_of(second_pane_id), Some(1));
}

#[test]
fn visible_index_of_is_none_for_a_pane_id_not_in_the_active_tab() {
    let workspace = Workspace::mvp();
    let unknown = super::types::PaneId::new();

    assert_eq!(workspace.visible_index_of(unknown), None);
}

#[test]
fn cursor_pane_id_follows_the_workspace_mode_cursor() {
    let mut workspace = Workspace::mvp();
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    let second_pane_id = workspace.split_active(SessionKind::Terminal, SessionId::new());
    workspace.activate_visible_pane(0);
    workspace.enter_workspace_mode();

    assert_eq!(workspace.cursor_pane_id(), Some(first_pane_id));

    workspace.move_cursor(Direction::Right);

    assert_eq!(workspace.cursor_pane_id(), Some(second_pane_id));
}

#[test]
fn all_pane_ids_includes_panes_in_every_tab_not_just_the_active_one() {
    let mut workspace = Workspace::mvp();
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    let second_pane_id = workspace.split_active(SessionKind::Terminal, SessionId::new());
    let third_pane_id = workspace.open_tab(Content::Session(SessionKind::Agent, SessionId::new()));

    let all = workspace.all_pane_ids();

    assert!(all.contains(&first_pane_id));
    assert!(all.contains(&second_pane_id));
    assert!(all.contains(&third_pane_id));
    assert_eq!(all.len(), 3);
}

#[test]
fn split_active_with_new_session_uses_active_pane_kind() {
    let mut workspace = Workspace::mvp();

    let (terminal_kind, terminal_session_id) = workspace
        .split_active_with_new_session()
        .expect("terminal split");
    assert_eq!(terminal_kind, SessionKind::Terminal);
    assert_eq!(
        workspace.visible_terminal_session_id(1),
        Some(terminal_session_id)
    );

    workspace.open_tab(Content::Session(SessionKind::Agent, SessionId::new()));
    let (agent_kind, agent_session_id) = workspace
        .split_active_with_new_session()
        .expect("agent split");
    assert_eq!(agent_kind, SessionKind::Agent);
    assert_eq!(
        workspace.visible_agent_session_id(1),
        Some(agent_session_id)
    );
}

#[test]
fn pane_location_for_session_resolves_the_hosting_pane() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(SessionKind::Terminal, session_id);

    assert_eq!(
        workspace.pane_location_for_session(session_id),
        workspace.visible_pane_id(1)
    );
}

#[test]
fn pane_location_for_session_is_none_once_detached() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(SessionKind::Terminal, session_id);
    workspace.close_visible_pane(1);

    assert_eq!(workspace.pane_location_for_session(session_id), None);
}

#[test]
fn detach_reports_session_and_removes_reference() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let pane_id = workspace.split_active(SessionKind::Terminal, session_id);

    assert_eq!(workspace.detach_pane(pane_id), Some(session_id));
    assert!(!workspace.session_is_referenced(session_id));
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn detach_last_pane_removes_tab() {
    let mut workspace = Workspace::mvp();
    let pane_id = workspace.visible_pane_id(0).expect("initial pane");

    assert!(workspace.detach_pane(pane_id).is_some());
    assert!(workspace.visible_panes().is_empty());
}

#[test]
fn close_visible_pane_keeps_last_pane() {
    let mut workspace = Workspace::mvp();

    assert_eq!(workspace.close_visible_pane(0), None);
    assert_eq!(workspace.visible_panes().len(), 1);
}

#[test]
fn close_visible_pane_detaches_when_another_pane_remains() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(SessionKind::Terminal, session_id);

    assert_eq!(workspace.close_visible_pane(1), Some(session_id));
    assert_eq!(workspace.visible_panes().len(), 1);
    assert!(!workspace.session_is_referenced(session_id));
    assert_eq!(workspace.session_count(), 2);
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn close_cursor_pane_closes_the_cursor_pane_not_the_focus() {
    let mut workspace = Workspace::mvp();
    let first = workspace.visible_pane_id(0).expect("first pane");
    let second = workspace.split_active(SessionKind::Terminal, SessionId::new());
    // `split_active` dives into the new pane; reset focus to the first so
    // the cursor and focus start on different panes.
    workspace.activate_visible_pane(0);
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Right);
    assert_eq!(workspace.cursor_pane_id(), Some(second));

    assert!(workspace.close_cursor_pane());

    assert_eq!(workspace.visible_pane_ids(), vec![first]);
    assert!(
        workspace.is_active_pane(first),
        "the focused pane must survive a cursor-targeted close"
    );
}

#[test]
fn close_cursor_pane_targets_the_focused_pane_outside_the_mode() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let second = workspace.split_active(SessionKind::Terminal, session_id);
    assert!(workspace.is_active_pane(second));

    assert!(workspace.close_cursor_pane());

    assert_eq!(workspace.visible_panes().len(), 1);
    assert!(!workspace.session_is_referenced(session_id));
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn close_cursor_pane_keeps_the_last_pane_and_the_mode_untouched() {
    let mut workspace = Workspace::mvp();
    let only = workspace.visible_pane_id(0).expect("initial pane");
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Right);

    assert!(!workspace.close_cursor_pane());

    assert_eq!(workspace.visible_pane_ids(), vec![only]);
    assert!(
        workspace.is_workspace_mode_active(),
        "an inert close must not cancel workspace mode"
    );
    assert_eq!(workspace.cursor_pane_id(), Some(only));
}

#[test]
fn close_cursor_pane_reports_a_closed_session_less_pane() {
    let mut workspace = Workspace::mvp();
    let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
    // The split dives into the view pane, so the cursor seeds there.
    workspace.enter_workspace_mode();
    assert_eq!(workspace.cursor_pane_id(), Some(view_pane));

    assert!(
        workspace.close_cursor_pane(),
        "a session-less pane still counts as closed -- the no-op signal \
         must stay reserved for the last-pane guard"
    );
    assert_eq!(workspace.visible_panes().len(), 1);
}

#[test]
fn close_cursor_pane_or_tab_closes_only_the_pane_when_others_remain() {
    let mut workspace = Workspace::mvp();
    let first = workspace.visible_pane_id(0).expect("first pane");
    let second_session = SessionId::new();
    let second = workspace.split_active(SessionKind::Terminal, second_session);
    // `split_active` dives into the new pane, so the mode's cursor seeds there.
    workspace.enter_workspace_mode();
    assert_eq!(workspace.cursor_pane_id(), Some(second));

    assert_eq!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedPane(Some(second_session))
    );

    assert_eq!(workspace.visible_pane_ids(), vec![first]);
    assert_eq!(workspace.tab_count(), 1);
    assert!(!workspace.session_is_referenced(second_session));
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn close_cursor_pane_or_tab_closes_the_tab_on_its_last_pane() {
    // The fall-through that replaced the old silent no-op: with the
    // cursor pane the tab's last one, `x`'s operation closes the whole tab.
    let mut workspace = Workspace::mvp();
    let only_session = workspace.active_terminal_session_id().expect("session");
    workspace.enter_workspace_mode();

    assert_eq!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedTab(vec![only_session])
    );

    assert_eq!(workspace.tab_count(), 0);
    assert_eq!(workspace.visible_panes().len(), 0);
    assert!(!workspace.session_is_referenced(only_session));
    assert_eq!(workspace.detached_session_count(), 1);
    assert!(workspace.to_persisted_json().is_ok());
}

#[test]
fn close_cursor_pane_or_tab_on_last_pane_activates_the_neighbor_tab() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, second_session));
    workspace.enter_workspace_mode();

    assert_eq!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedTab(vec![second_session])
    );

    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
    assert!(workspace.session_is_referenced(first_session));
    assert!(!workspace.session_is_referenced(second_session));
}

#[test]
fn close_cursor_pane_or_tab_is_inert_on_a_zero_tab_workspace() {
    let mut workspace = Workspace::mvp();
    workspace.close_tab_index(0);

    assert_eq!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::Noop
    );
}

#[test]
fn detached_session_summaries_list_unattached_sessions() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(SessionKind::Terminal, session_id);
    workspace.close_visible_pane(1);

    assert_eq!(
        workspace.detached_session_summaries(),
        vec![SessionSummary {
            id: session_id,
            kind: SessionKind::Terminal,
            display_number: 2,
            title: "Terminal #2".to_string(),
            attached: false,
            directory: None,
            parent_session_id: None,
            activity: None,
        }]
    );
}

#[test]
fn session_summaries_include_attached_and_detached_sessions() {
    let mut workspace = Workspace::mvp();
    let attached_session = workspace.active_terminal_session_id().expect("session");
    let detached_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, detached_session);
    workspace.close_visible_pane(1);

    assert_eq!(
        workspace.session_summaries(),
        vec![
            SessionSummary {
                id: attached_session,
                kind: SessionKind::Terminal,
                display_number: 1,
                title: "Terminal #1".to_string(),
                attached: true,
                directory: None,
                parent_session_id: None,
                activity: None,
            },
            SessionSummary {
                id: detached_session,
                kind: SessionKind::Terminal,
                display_number: 2,
                title: "Terminal #2".to_string(),
                attached: false,
                directory: None,
                parent_session_id: None,
                activity: None,
            },
        ]
    );
}

#[test]
fn set_session_parent_is_reflected_in_session_summaries() {
    // `docs/session-relationship-design.md` decision 4b: the session
    // manager's lineage view derives its tree from `SessionSummary.
    // parent_session_id`, so `set_session_parent` (mirroring `set_session_
    // directory`) must actually reach the summary a caller reads back.
    let mut workspace = Workspace::mvp();
    let parent_id = workspace.active_terminal_session_id().expect("session");
    let child_id = SessionId::new();
    workspace.register_detached_session(crate::SessionKind::Agent, child_id);

    workspace.set_session_parent(child_id, parent_id);

    let child_summary = workspace
        .session_summaries()
        .into_iter()
        .find(|summary| summary.id == child_id)
        .expect("child summary");
    assert_eq!(child_summary.parent_session_id, Some(parent_id));

    // Unrelated sessions stay parentless.
    let parent_summary = workspace
        .session_summaries()
        .into_iter()
        .find(|summary| summary.id == parent_id)
        .expect("parent summary");
    assert_eq!(parent_summary.parent_session_id, None);
}

#[test]
fn session_identity_survives_detach_and_reattach() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(SessionKind::Terminal, session_id);

    assert_eq!(
        workspace.visible_pane_title(1),
        Some("Terminal #2".to_string())
    );
    workspace.close_visible_pane(1);
    assert_eq!(
        workspace.detached_session_summaries()[0].title,
        "Terminal #2"
    );

    workspace
        .attach_existing_session_to_split_activated(session_id, true)
        .expect("reattached pane");

    assert_eq!(
        workspace.visible_pane_title(1),
        Some("Terminal #2".to_string())
    );
}

#[test]
fn a_registered_detached_session_is_attachable() {
    // The attach-time lookup fallback's model claim (shell board task
    // #46): a daemon-side session the shell has never seen becomes
    // attachable the moment `register_detached_session` records it -- the
    // same model half `adopt_daemon_agent_session` performs before the
    // attach. An unregistered id must still refuse to attach.
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    assert_eq!(
        workspace.attach_existing_session_to_split_activated(session_id, true),
        None,
        "an unregistered id must not attach"
    );

    workspace.register_detached_session(crate::SessionKind::Agent, session_id);
    workspace
        .attach_existing_session_to_split_activated(session_id, true)
        .expect("the registered session attaches");
}

#[test]
fn session_display_numbers_are_not_reused_after_terminate() {
    let mut workspace = Workspace::mvp();
    let second_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second_session);
    workspace.terminate_session(second_session);

    let third_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, third_session);

    assert_eq!(
        workspace.visible_pane_title(1),
        Some("Terminal #3".to_string())
    );
}

#[test]
fn attach_existing_session_to_split_reuses_session_kind() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Agent, session_id));
    workspace.close_tab_index(1);

    let pane_id = workspace
        .attach_existing_session_to_split_activated(session_id, true)
        .expect("attached pane");

    assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
    assert_eq!(workspace.visible_panes()[1].plugin.name, "agent");
    assert!(workspace.session_is_referenced(session_id));
    assert_eq!(workspace.detached_session_count(), 0);
}

#[test]
fn open_tab_with_new_session_attaches_requested_kind() {
    let mut workspace = Workspace::mvp();

    let session_id = workspace.open_tab_with_new_session_activated(crate::SessionKind::Agent, true);

    assert_eq!(workspace.visible_agent_session_id(0), Some(session_id));
    assert_eq!(workspace.visible_panes()[0].plugin.name, "agent");
    assert!(workspace.session_is_referenced(session_id));
}

#[test]
fn opening_tab_is_reflected_in_tab_summaries() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let agent_session = SessionId::new();

    workspace.open_tab(Content::Session(SessionKind::Agent, agent_session));

    assert_eq!(
        workspace.tab_summaries(),
        vec![
            TabSummary {
                index: 0,
                title: "Terminal #1".to_string(),
                active: false,
                pane_count: 1,
                active_session_id: Some(first_session),
                activity: None,
            },
            TabSummary {
                index: 1,
                title: "Agent #1".to_string(),
                active: true,
                pane_count: 1,
                active_session_id: Some(agent_session),
                activity: None,
            },
        ]
    );
}

#[test]
fn activate_tab_index_switches_visible_panes() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(Content::View(SessionKind::Agent.plugin()));

    assert!(workspace.activate_tab_index(0));
    assert_eq!(workspace.visible_panes()[0].plugin.name, "terminal");
    assert!(!workspace.activate_tab_index(9));
    assert_eq!(workspace.visible_panes()[0].plugin.name, "terminal");
}

#[test]
fn close_tab_index_can_empty_the_workspace() {
    // 2026-07-18 owner clarification: closing the workspace's last tab is
    // allowed to leave a valid, empty workspace, rather than being
    // refused to guarantee at least one tab always remains.
    let mut workspace = Workspace::mvp();
    let session_id = workspace.active_terminal_session_id().expect("session");

    assert_eq!(workspace.close_tab_index(0), vec![session_id]);

    assert_eq!(workspace.tab_count(), 0);
    assert_eq!(workspace.visible_panes().len(), 0);
    assert!(workspace.active_tab().is_none());
    assert!(workspace.to_persisted_json().is_ok());
}

#[test]
fn close_tab_index_removes_tab_panes_and_returns_sessions() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, second_session));

    assert_eq!(workspace.close_tab_index(1), vec![second_session]);
    assert_eq!(workspace.tab_count(), 1);
    assert!(workspace.session_is_referenced(first_session));
    assert!(!workspace.session_is_referenced(second_session));
    assert_eq!(workspace.session_count(), 2);
    assert_eq!(workspace.detached_session_count(), 1);
    assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
}

#[test]
fn close_active_pane_detaches_active_session() {
    let mut workspace = Workspace::mvp();
    let second_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second_session);

    assert_eq!(workspace.close_active_pane(), Some(second_session));
    assert_eq!(workspace.visible_panes().len(), 1);
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn close_active_tab_returns_active_tab_sessions() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, second_session));

    assert_eq!(workspace.close_active_tab(), vec![second_session]);
    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
}

#[test]
fn close_active_tab_activates_neighbor() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(Content::View(SessionKind::Agent.plugin()));
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));

    assert!(workspace.tab_summaries()[2].active);
    assert_eq!(workspace.close_tab_index(2).len(), 1);
    assert_eq!(workspace.tab_count(), 2);
    assert!(workspace.tab_summaries()[1].active);
    assert_eq!(workspace.active_title(), "AI Agent");
}

#[test]
fn close_inactive_tab_preserves_active_tab() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    workspace.open_tab(Content::View(SessionKind::Agent.plugin()));

    assert_eq!(workspace.active_title(), "AI Agent");
    assert_eq!(workspace.close_tab_index(0), vec![first_session]);
    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_title(), "AI Agent");
}

#[test]
fn activate_visible_pane_switches_active_pane() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(SessionKind::Terminal, SessionId::new());

    assert_eq!(workspace.active_visible_index(), 1);
    assert!(workspace.activate_visible_pane(0));
    assert_eq!(workspace.active_visible_index(), 0);
    assert!(!workspace.activate_visible_pane(5));
    assert_eq!(workspace.active_visible_index(), 0);
}

#[test]
fn pane_summaries_include_split_panes_by_tab() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(SessionKind::Terminal, SessionId::new());

    assert_eq!(
        workspace.pane_summaries(),
        vec![
            PaneSummary {
                tab_index: 0,
                pane_index: 0,
                title: "Terminal #1".to_string(),
                plugin: "terminal".to_string(),
                active: false,
                tab_active: true,
            },
            PaneSummary {
                tab_index: 0,
                pane_index: 1,
                title: "Terminal #2".to_string(),
                plugin: "terminal".to_string(),
                active: true,
                tab_active: true,
            },
        ]
    );
}

#[test]
fn activate_tab_index_switches_tab_and_focuses_its_active_pane() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(SessionKind::Terminal, SessionId::new());
    workspace.activate_visible_pane(0);
    workspace.open_tab(Content::View(SessionKind::Agent.plugin()));

    assert_eq!(workspace.active_tab_index(), 1);
    assert!(workspace.activate_tab_index(0));
    assert_eq!(workspace.active_tab_index(), 0);
    assert_eq!(workspace.active_visible_index(), 0);
    assert_eq!(workspace.active_title(), "Terminal #1");
    assert!(!workspace.activate_tab_index(9));
    assert_eq!(workspace.active_title(), "Terminal #1");
}

#[test]
fn active_tab_index_tracks_active_tab() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(Content::View(SessionKind::Agent.plugin()));

    assert_eq!(workspace.active_tab_index(), 1);
    assert!(workspace.activate_tab_index(0));
    assert_eq!(workspace.active_tab_index(), 0);
}

#[test]
fn terminate_session_removes_session_and_attachments() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second_session);

    assert!(workspace.terminate_session(second_session));
    assert_eq!(workspace.session_count(), 1);
    assert!(!workspace.session_is_referenced(second_session));
    assert!(workspace.session_is_referenced(first_session));
    assert_eq!(workspace.visible_panes().len(), 1);
}

#[test]
fn terminate_session_on_the_active_tab_activates_the_neighbor_tab() {
    // A terminal exiting closes its tab through `terminate_session` ->
    // `detach_pane`, which must pick the same next active tab as the x-key
    // path (`close_tab_index`, see
    // `close_cursor_pane_or_tab_on_last_pane_activates_the_neighbor_tab`):
    // the tab that shifts into the removed slot, not the first tab.
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let middle_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, middle_session));
    let exiting_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, exiting_session));

    assert!(workspace.terminate_session(exiting_session));

    assert_eq!(workspace.tab_count(), 2);
    assert_eq!(workspace.active_tab_index(), 1);
    assert_eq!(workspace.active_terminal_session_id(), Some(middle_session));
    assert!(workspace.session_is_referenced(first_session));
    assert!(workspace.session_is_referenced(middle_session));
    assert!(!workspace.session_is_referenced(exiting_session));
}

#[test]
fn terminate_session_on_the_last_tab_falls_back_to_the_left_neighbor() {
    // Neighbor-rule edge case, matching `close_tab_index`: removing the
    // rightmost tab clamps to the new last tab.
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.open_tab(Content::Session(SessionKind::Terminal, second_session));

    assert!(workspace.terminate_session(second_session));

    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_tab_index(), 0);
    assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
}

#[test]
fn terminate_active_session_returns_removed_session() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second_session);

    assert_eq!(workspace.terminate_active_session(), Some(second_session));
    assert_eq!(workspace.session_count(), 1);
    assert!(!workspace.session_is_referenced(second_session));
    assert!(workspace.session_is_referenced(first_session));
}

#[test]
fn terminate_active_session_targets_the_cursor_not_focus_in_workspace_mode() {
    // Inside workspace mode the cursor and focus diverge: `hjkl` moves the
    // cursor (the operation target) without touching focus (where keyboard
    // input flows). `terminate_active_session` must follow the cursor, so
    // the session under it is the one that gets killed -- not the focused
    // pane's session. Outside the mode the two coincide by construction
    // (`cursor_pane_id` falls back to `tab.active`), so the non-mode case
    // is already covered by `terminate_active_session_returns_removed_session`.
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    let second = workspace.split_active(SessionKind::Terminal, second_session);
    let first = workspace.visible_pane_id(0).expect("first pane");

    // Focus the first pane, then move the workspace-mode cursor onto the
    // second so the two diverge.
    workspace.activate_visible_pane(0);
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Right);
    assert_eq!(workspace.cursor_pane_id(), Some(second));
    assert!(
        workspace.is_active_pane(first),
        "focus must stay on the first pane while the cursor roams"
    );

    // The cursor sits on `second`; terminate must hit that session, not the
    // focused (first) pane's.
    assert_eq!(workspace.terminate_active_session(), Some(second_session));
    assert!(!workspace.session_is_referenced(second_session));
    assert!(workspace.session_is_referenced(first_session));
}

#[test]
fn terminate_unknown_session_is_noop() {
    let mut workspace = Workspace::mvp();

    assert!(!workspace.terminate_session(SessionId::new()));
    assert_eq!(workspace.session_count(), 1);
    assert_eq!(workspace.visible_panes().len(), 1);
}

#[test]
fn repeated_splits_of_the_focused_pane_stay_in_one_row() {
    // `split_active` always splits the focused pane, and each split leaves
    // its new pane focused, so three consecutive splits chain onto the same
    // (last-created) pane -- this should absorb into a single flat row
    // rather than nest (docs/recursive-layout-design.md's shallow-nesting
    // invariant).
    let mut workspace = Workspace::mvp();
    workspace.split_active(SessionKind::Terminal, SessionId::new());
    workspace.split_active(SessionKind::Terminal, SessionId::new());
    workspace.split_active(SessionKind::Terminal, SessionId::new());

    assert_eq!(workspace.visible_panes().len(), 4);
    let root = &workspace.active_tab().expect("tab").root;
    assert!(root.is_canonical());
    match root {
        LayoutNode::Split { children, .. } => assert_eq!(children.len(), 4),
        _ => panic!("expected a single flat row of 4 panes"),
    }
}

#[test]
fn split_session_with_new_session_targets_the_sessions_own_pane() {
    // The new pane must land next to `target_session_id`'s own pane, not
    // whichever pane happens to be focused in that tab: here the tab's
    // focus is refocused onto the *first* pane before splitting on it, so
    // the new pane must be inserted right after it (index 1), leaving the
    // second session's pane pushed to the end (index 2) rather than the
    // new pane simply being appended.
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second_session);
    workspace.activate_visible_pane(0);

    let third_session = workspace
        .split_session_with_new_session(
            first_session,
            crate::SessionKind::Terminal,
            SplitAxis::Horizontal,
            true,
        )
        .expect("split next to the first session's pane");

    let root = &workspace.active_tab().expect("tab").root;
    assert!(root.is_canonical());
    assert_eq!(root.pane_ids().len(), 3);
    assert_eq!(
        workspace.visible_terminal_session_id(0),
        Some(first_session)
    );
    assert_eq!(
        workspace.visible_terminal_session_id(1),
        Some(third_session)
    );
    assert_eq!(
        workspace.visible_terminal_session_id(2),
        Some(second_session)
    );
}

#[test]
fn split_session_with_new_session_honors_the_vertical_axis() {
    // `docs/recursive-layout-design.md`'s slice 3: the axis threaded through
    // `split_session_with_new_session` must actually reach the tree, not
    // just get accepted and dropped.
    let mut workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");

    workspace
        .split_session_with_new_session(
            session,
            crate::SessionKind::Terminal,
            SplitAxis::Vertical,
            true,
        )
        .expect("split next to the session's pane");

    let root = &workspace.active_tab().expect("tab").root;
    match root {
        LayoutNode::Split { axis, children } => {
            assert_eq!(*axis, SplitAxis::Vertical);
            assert_eq!(children.len(), 2);
        }
        _ => panic!("expected a vertical split of 2 panes"),
    }
}

#[test]
fn a_derived_title_updates_an_auto_titled_session_and_dedupes() {
    let mut workspace = Workspace::mvp();
    let session_id = workspace.active_terminal_session_id().expect("session");

    assert!(workspace.set_session_derived_title(session_id, Some("~/src/horizon")));
    assert_eq!(workspace.session_summaries()[0].title, "~/src/horizon");
    // A source re-reporting the title it already set changes nothing: the
    // shell's pump keys its persist/repaint off this false.
    assert!(!workspace.set_session_derived_title(session_id, Some("~/src/horizon")));
}

#[test]
fn a_title_reset_restores_the_fallback_and_an_unknown_session_is_ignored() {
    let mut workspace = Workspace::mvp();
    let session_id = workspace.active_terminal_session_id().expect("session");
    let fallback = workspace.session_summaries()[0].title.clone();

    assert!(workspace.set_session_derived_title(session_id, Some("cargo build")));
    // The `None` arm (`TerminalUpdate::Title(None)`, a shell's title reset)
    // restores the kind-and-display-number default the session was created
    // with.
    assert!(workspace.set_session_derived_title(session_id, None));
    assert_eq!(workspace.session_summaries()[0].title, fallback);
    // A report for a session the model does not know is a no-op.
    assert!(!workspace.set_session_derived_title(SessionId::new(), Some("nope")));
}

#[test]
fn what_a_session_does_is_news_until_the_session_is_looked_at() {
    use crate::types::{
        AgentStatus, Attention, BlockedOn, Doing, ProgramState, ProgramStatus, SessionActivity,
    };

    let mut workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");
    let program = |state, blocked_on| {
        Doing::Program(ProgramStatus {
            state,
            blocked_on,
            app: Some("claude-code".into()),
        })
    };
    let activity = |workspace: &Workspace| workspace.session_summaries().remove(0).activity;
    let attention =
        |workspace: &Workspace| activity(workspace).and_then(|activity| activity.attention());
    let done = program(ProgramState::Done, None);

    assert!(workspace.set_session_activity(session, Some(done.clone()), None, 0));
    assert!(!workspace.set_session_activity(session, Some(done.clone()), None, 0));
    assert_eq!(attention(&workspace), Some(Attention::Unseen));
    // The tab is titled after the session, and says the same of it.
    assert_eq!(workspace.tab_summaries()[0].activity, activity(&workspace));

    assert!(workspace.mark_session_seen(session));
    assert!(!workspace.mark_session_seen(session));
    assert_eq!(attention(&workspace), None);
    // Still done, said again: the same piece of work, and still seen.
    assert!(!workspace.set_session_activity(session, Some(done), None, 0));
    assert_eq!(attention(&workspace), None);

    // What it newly does has not been looked at, and a session that waits
    // is waited on whether looked at or not.
    let blocked = program(ProgramState::Blocked, Some(BlockedOn::Permission));
    assert!(workspace.set_session_activity(session, Some(blocked), None, 0));
    assert!(activity(&workspace).is_some_and(|activity| !activity.seen));
    workspace.mark_session_seen(session);
    assert_eq!(attention(&workspace), Some(Attention::Approval));

    assert!(workspace.set_session_activity(session, None, None, 0));
    assert_eq!(activity(&workspace), None);
    assert!(!workspace.mark_session_seen(session));
    assert!(!workspace.set_session_activity(SessionId::new(), None, None, 0));

    // Every kind's words have the one reading.
    let reads = |doing, seen| {
        let activity = SessionActivity {
            doing,
            detail: None,
            since_ms: 0,
            seen,
        };
        activity.attention()
    };
    let question = Some(BlockedOn::Question);
    for (doing, seen, read) in [
        (
            program(ProgramState::Blocked, question),
            true,
            Some(Attention::Input),
        ),
        (
            program(ProgramState::Blocked, Some(BlockedOn::Auth)),
            true,
            Some(Attention::Login),
        ),
        (
            program(ProgramState::Blocked, None),
            true,
            Some(Attention::Blocked),
        ),
        (
            program(ProgramState::Error, None),
            true,
            Some(Attention::Failed),
        ),
        (program(ProgramState::Working, None), false, None),
        (program(ProgramState::Idle, None), false, None),
        (
            Doing::Agent(AgentStatus::WaitingForApproval),
            true,
            Some(Attention::Approval),
        ),
        (
            Doing::Agent(AgentStatus::Paused),
            true,
            Some(Attention::Input),
        ),
        (
            Doing::Agent(AgentStatus::Failed),
            true,
            Some(Attention::Failed),
        ),
        (
            Doing::Agent(AgentStatus::Completed),
            false,
            Some(Attention::Unseen),
        ),
        (Doing::Agent(AgentStatus::Completed), true, None),
        (Doing::Agent(AgentStatus::WaitingForInput), false, None),
        (Doing::Agent(AgentStatus::Running), false, None),
        (Doing::Agent(AgentStatus::Cancelled), false, None),
    ] {
        assert_eq!(reads(doing.clone(), seen), read, "{doing:?}, seen: {seen}");
    }
}

#[test]
fn what_was_looked_at_stays_seen_until_the_session_has_been_at_work() {
    use crate::types::{AgentStatus, Doing, ProgramState, ProgramStatus};

    let mut workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");
    let seen = |workspace: &Workspace| {
        (workspace.session_activity(session)).is_some_and(|activity| activity.seen)
    };
    let program = |state| {
        Doing::Program(ProgramStatus {
            state,
            blocked_on: None,
            app: None,
        })
    };
    let completed = Doing::Agent(AgentStatus::Completed);

    // Out of reach and back with nothing done in between: still seen.
    workspace.set_session_activity(session, Some(completed.clone()), None, 0);
    workspace.mark_session_seen(session);
    workspace.set_session_activity(session, Some(Doing::Agent(AgentStatus::Loading)), None, 0);
    assert!(!seen(&workspace));
    workspace.set_session_activity(session, Some(completed.clone()), None, 0);
    assert!(seen(&workspace));

    // At work and done again: what is done now is new.
    workspace.set_session_activity(session, Some(Doing::Agent(AgentStatus::Running)), None, 0);
    workspace.set_session_activity(session, Some(completed.clone()), None, 0);
    assert!(!seen(&workspace));

    // The same holds for a program, also where it took its report back.
    workspace.set_session_activity(session, Some(program(ProgramState::Done)), None, 0);
    workspace.mark_session_seen(session);
    workspace.set_session_activity(session, None, None, 0);
    workspace.set_session_activity(session, Some(program(ProgramState::Done)), None, 0);
    assert!(seen(&workspace));
    workspace.set_session_activity(session, Some(program(ProgramState::Working)), None, 0);
    workspace.set_session_activity(session, Some(program(ProgramState::Done)), None, 0);
    assert!(!seen(&workspace));
}

#[test]
fn what_was_looked_at_is_kept_across_a_restart() {
    use crate::types::{BlockedOn, Doing, ProgramState, ProgramStatus};

    let mut workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");
    let done = Doing::Program(ProgramStatus {
        state: ProgramState::Done,
        blocked_on: None,
        app: Some("claude-code".into()),
    });
    let blocked = Doing::Program(ProgramStatus {
        state: ProgramState::Blocked,
        blocked_on: Some(BlockedOn::Permission),
        app: Some("claude-code".into()),
    });
    let seen = |workspace: &Workspace| {
        (workspace.session_activity(session)).is_some_and(|activity| activity.seen)
    };

    // Before anything is looked at, the state is as it was before this
    // was kept: a build from then reads it.
    let untouched = workspace.to_persisted_json().expect("serialize");
    assert!(!untouched.contains("looked_at"), "{untouched}");

    workspace.set_session_activity(session, Some(done.clone()), None, 0);
    workspace.mark_session_seen(session);
    let json = workspace.to_persisted_json().expect("serialize");

    // Restarted, the session says what it is doing again: the same as it
    // was last seen doing, which has been looked at.
    let mut restored = Workspace::from_persisted_json(&json).expect("restore");
    assert_eq!(restored.session_activity(session), None);
    restored.set_session_activity(session, Some(done.clone()), None, 0);
    assert!(seen(&restored));
    assert_eq!(restored.to_persisted_json().expect("serialize again"), json);

    // Something else than it was last seen doing has not been.
    let mut restored = Workspace::from_persisted_json(&json).expect("restore");
    restored.set_session_activity(session, Some(blocked.clone()), None, 0);
    assert!(!seen(&restored));

    // Every part of what was seen is kept.
    restored.mark_session_seen(session);
    let json = restored.to_persisted_json().expect("serialize");
    let mut again = Workspace::from_persisted_json(&json).expect("restore");
    again.set_session_activity(session, Some(blocked), None, 0);
    assert!(seen(&again));

    // A word this build does not know is nothing looked at, not a state
    // that cannot be read.
    let unknown = json.replace("\"blocked\"", "\"dreaming\"");
    assert_ne!(unknown, json);
    let mut other = Workspace::from_persisted_json(&unknown).expect("restore");
    other.set_session_activity(session, Some(done), None, 0);
    assert!(!seen(&other));
}

#[test]
fn showing_a_session_goes_to_its_pane_or_puts_it_beside_the_active_one() {
    let mut workspace = Workspace::mvp();
    let first = workspace.active_terminal_session_id().expect("session");
    let second = SessionId::new();
    workspace.split_active(SessionKind::Terminal, second);
    assert_eq!(workspace.active_session_id(), Some(second));

    // One a pane shows: that pane has the keys, and the mode is left.
    workspace.enter_workspace_mode();
    assert!(workspace.show_session(first));
    assert_eq!(workspace.active_session_id(), Some(first));
    assert!(!workspace.is_workspace_mode_active());
    assert_eq!(workspace.visible_panes().len(), 2);

    // One no pane shows: a pane of it, beside the active one.
    let detached = SessionId::new();
    workspace.register_detached_session(SessionKind::Agent, detached);
    assert!(workspace.show_session(detached));
    assert_eq!(workspace.active_session_id(), Some(detached));
    assert_eq!(workspace.visible_panes().len(), 3);

    // One the workspace does not have changes nothing.
    assert!(!workspace.show_session(SessionId::new()));
    assert_eq!(workspace.visible_panes().len(), 3);
}

#[test]
fn ending_a_session_with_what_was_derived_from_it_leaves_the_others() {
    let mut workspace = Workspace::mvp();
    let kept = workspace.active_terminal_session_id().expect("session");
    let (root, child, grandchild) = (SessionId::new(), SessionId::new(), SessionId::new());
    for session in [root, child, grandchild] {
        workspace.register_detached_session(SessionKind::Agent, session);
    }
    workspace.set_session_parent(child, root);
    workspace.set_session_parent(grandchild, child);

    assert_eq!(workspace.terminate_session_with_derived(child), 2);
    let left: Vec<SessionId> = (workspace.session_summaries().iter())
        .map(|session| session.id)
        .collect();
    assert_eq!(left, [kept, root]);
    assert_eq!(workspace.terminate_session_with_derived(child), 0);
}

#[test]
fn a_session_has_done_what_it_does_since_it_came_to_do_it() {
    use crate::types::{AgentStatus, Doing, ProgramState, ProgramStatus};

    let mut workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");
    let since = |workspace: &Workspace| {
        workspace
            .session_activity(session)
            .map(|activity| activity.since_ms)
    };
    let detail = |workspace: &Workspace| {
        (workspace.session_activity(session)).and_then(|activity| activity.detail.clone())
    };
    let working = Doing::Program(ProgramStatus {
        state: ProgramState::Working,
        blocked_on: None,
        app: None,
    });
    let completed = Doing::Agent(AgentStatus::Completed);

    // Its words change as it works; it has worked since it began.
    assert!(workspace.set_session_activity(session, Some(working.clone()), None, 100));
    let reading = Some("Reading notes.txt".to_string());
    assert!(workspace.set_session_activity(session, Some(working.clone()), reading.clone(), 200));
    assert!(!workspace.set_session_activity(session, Some(working.clone()), reading.clone(), 300));
    assert_eq!(
        (since(&workspace), detail(&workspace)),
        (Some(100), reading)
    );

    // What it comes to do next begins then.
    workspace.set_session_activity(session, Some(completed.clone()), None, 400);
    assert_eq!(since(&workspace), Some(400));

    // Out of reach and back, or restarted, it has done it since it did.
    workspace.set_session_activity(session, Some(Doing::Agent(AgentStatus::Loading)), None, 500);
    assert_eq!(since(&workspace), Some(500));
    workspace.set_session_activity(session, Some(completed.clone()), None, 600);
    assert_eq!(since(&workspace), Some(400));
    let json = workspace.to_persisted_json().expect("serialize");
    let mut restored = Workspace::from_persisted_json(&json).expect("restore");
    restored.set_session_activity(session, Some(completed.clone()), None, 900);
    assert_eq!(since(&restored), Some(400));
    assert_eq!(restored.to_persisted_json().expect("serialize again"), json);

    // Having worked since, it has not.
    restored.set_session_activity(session, Some(working), None, 1000);
    restored.set_session_activity(session, Some(completed), None, 1100);
    assert_eq!(since(&restored), Some(1100));
}

#[test]
fn a_state_kept_with_a_programs_words_in_it_is_still_read() {
    let workspace = Workspace::mvp();
    let session = workspace.active_terminal_session_id().expect("session");
    let json = workspace.to_persisted_json().expect("serialize");
    let mut state: serde_json::Value = serde_json::from_str(&json).expect("json");
    state["sessions"][0]["looked_at"] = serde_json::json!({
        "program": {"state": "blocked", "message": "approve Bash: ls"}
    });
    let with_words = state.to_string();
    let restored = Workspace::from_persisted_json(&with_words).expect("restore");
    assert_eq!(restored.session_activity(session), None);
}
