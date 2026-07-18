use super::types::{LayoutNode, PaneSummary, SessionSummary, TabSummary};
use super::*;
use crate::SessionId;

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
    let pane_id = workspace.split_active(PaneKind::Terminal, Some(session_id));

    assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
    assert_eq!(workspace.visible_terminal_session_id(1), Some(session_id));
    assert!(workspace.session_is_referenced(session_id));
}

#[test]
fn pane_id_accessors_resolve_kind_session_and_title_for_a_specific_pane() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let pane_id = workspace.split_active(PaneKind::Agent, Some(session_id));

    assert_eq!(workspace.pane_kind(pane_id), Some(PaneKind::Agent));
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
    let pane_id = workspace.open_tab(PaneKind::View(ViewKind::ThemeSettings), None);

    assert_eq!(
        workspace.pane_kind(pane_id),
        Some(PaneKind::View(ViewKind::ThemeSettings))
    );
    assert_eq!(workspace.active_session_id(), None);
    // No session is created for a view pane -- `ensure_session` no-ops on
    // `None`, so the session count is untouched.
    assert_eq!(workspace.session_count(), session_count_before);
    assert_eq!(
        workspace.pane_title_for(pane_id),
        Some("Theme Settings".to_string())
    );
}

#[test]
fn split_active_tab_with_view_adds_a_session_less_pane_beside_the_focus() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let session_count_before = workspace.session_count();

    let view_pane =
        workspace.split_active_tab_with_view(ViewKind::ThemeSettings, SplitAxis::Horizontal);

    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane, view_pane]);
    assert_eq!(
        workspace.pane_kind(view_pane),
        Some(PaneKind::View(ViewKind::ThemeSettings))
    );
    assert_eq!(workspace.session_count(), session_count_before);
    // Splitting dives into the new pane, same as every other split.
    assert!(workspace.is_active_pane(view_pane));
}

#[test]
fn closing_a_view_pane_detaches_no_session() {
    let mut workspace = Workspace::mvp();
    let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
    let view_pane =
        workspace.split_active_tab_with_view(ViewKind::ThemeSettings, SplitAxis::Horizontal);

    // Trivially destructive: nothing to detach, since there is no session.
    assert_eq!(workspace.close_pane(view_pane), None);
    assert_eq!(workspace.visible_pane_ids(), vec![terminal_pane]);
}

#[test]
fn is_active_pane_reflects_the_tabs_own_active_pane_by_id() {
    let mut workspace = Workspace::mvp();
    let first_pane_id = workspace.visible_pane_id(0).expect("first pane");
    let second_pane_id = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

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
    let second_pane_id = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

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
    let second_pane_id = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
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
    let second_pane_id = workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
    let third_pane_id = workspace.open_tab(PaneKind::Agent, Some(SessionId::new()));

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
    assert_eq!(terminal_kind, PaneKind::Terminal);
    assert_eq!(
        workspace.visible_terminal_session_id(1),
        Some(terminal_session_id)
    );

    workspace.open_tab(PaneKind::Agent, Some(SessionId::new()));
    let (agent_kind, agent_session_id) = workspace
        .split_active_with_new_session()
        .expect("agent split");
    assert_eq!(agent_kind, PaneKind::Agent);
    assert_eq!(
        workspace.visible_agent_session_id(1),
        Some(agent_session_id)
    );
}

#[test]
fn pane_location_for_session_resolves_the_hosting_pane() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(session_id));

    assert_eq!(
        workspace.pane_location_for_session(session_id),
        Some((0, 1))
    );
}

#[test]
fn pane_location_for_session_is_none_once_detached() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(session_id));
    workspace.close_visible_pane(1);

    assert_eq!(workspace.pane_location_for_session(session_id), None);
}

#[test]
fn detach_reports_session_and_removes_reference() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    let pane_id = workspace.split_active(PaneKind::Terminal, Some(session_id));

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
    workspace.split_active(PaneKind::Terminal, Some(session_id));

    assert_eq!(workspace.close_visible_pane(1), Some(session_id));
    assert_eq!(workspace.visible_panes().len(), 1);
    assert!(!workspace.session_is_referenced(session_id));
    assert_eq!(workspace.session_count(), 2);
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn detached_session_summaries_list_unattached_sessions() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(session_id));
    workspace.close_visible_pane(1);

    assert_eq!(
        workspace.detached_session_summaries(),
        vec![SessionSummary {
            id: session_id,
            kind: SessionKind::Terminal,
            display_number: 2,
            title: "Terminal #2".to_string(),
            attached: false,
        }]
    );
}

#[test]
fn session_summaries_include_attached_and_detached_sessions() {
    let mut workspace = Workspace::mvp();
    let attached_session = workspace.active_terminal_session_id().expect("session");
    let detached_session = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(detached_session));
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
            },
            SessionSummary {
                id: detached_session,
                kind: SessionKind::Terminal,
                display_number: 2,
                title: "Terminal #2".to_string(),
                attached: false,
            },
        ]
    );
}

#[test]
fn session_identity_survives_detach_and_reattach() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(session_id));

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
fn session_display_numbers_are_not_reused_after_terminate() {
    let mut workspace = Workspace::mvp();
    let second_session = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(second_session));
    workspace.terminate_session(second_session);

    let third_session = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(third_session));

    assert_eq!(
        workspace.visible_pane_title(1),
        Some("Terminal #3".to_string())
    );
}

#[test]
fn attach_existing_session_to_split_reuses_session_kind() {
    let mut workspace = Workspace::mvp();
    let session_id = SessionId::new();
    workspace.open_tab(PaneKind::Agent, Some(session_id));
    workspace.close_tab_index(1);

    let pane_id = workspace
        .attach_existing_session_to_split_activated(session_id, true)
        .expect("attached pane");

    assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
    assert_eq!(workspace.visible_panes()[1].kind, PaneKind::Agent);
    assert!(workspace.session_is_referenced(session_id));
    assert_eq!(workspace.detached_session_count(), 0);
}

#[test]
fn open_tab_with_new_session_attaches_requested_kind() {
    let mut workspace = Workspace::mvp();

    let session_id = workspace.open_tab_with_new_session_activated(PaneKind::Agent, true);

    assert_eq!(workspace.visible_agent_session_id(0), Some(session_id));
    assert_eq!(workspace.visible_panes()[0].kind, PaneKind::Agent);
    assert!(workspace.session_is_referenced(session_id));
}

#[test]
fn opening_tab_is_reflected_in_tab_summaries() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let agent_session = SessionId::new();

    workspace.open_tab(PaneKind::Agent, Some(agent_session));

    assert_eq!(
        workspace.tab_summaries(),
        vec![
            TabSummary {
                index: 0,
                title: "Terminal #1".to_string(),
                active: false,
                pane_count: 1,
                active_session_id: Some(first_session),
            },
            TabSummary {
                index: 1,
                title: "Agent #1".to_string(),
                active: true,
                pane_count: 1,
                active_session_id: Some(agent_session),
            },
        ]
    );
}

#[test]
fn activate_tab_index_switches_visible_panes() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(PaneKind::Agent, None);

    assert!(workspace.activate_tab_index(0));
    assert_eq!(workspace.visible_panes()[0].kind, PaneKind::Terminal);
    assert!(!workspace.activate_tab_index(9));
    assert_eq!(workspace.visible_panes()[0].kind, PaneKind::Terminal);
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
    workspace.open_tab(PaneKind::Terminal, Some(second_session));

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
    workspace.split_active(PaneKind::Terminal, Some(second_session));

    assert_eq!(workspace.close_active_pane(), Some(second_session));
    assert_eq!(workspace.visible_panes().len(), 1);
    assert_eq!(workspace.detached_session_count(), 1);
}

#[test]
fn close_active_tab_returns_active_tab_sessions() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.open_tab(PaneKind::Terminal, Some(second_session));

    assert_eq!(workspace.close_active_tab(), vec![second_session]);
    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_terminal_session_id(), Some(first_session));
}

#[test]
fn close_active_tab_activates_neighbor() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(PaneKind::Agent, None);
    workspace.open_tab(PaneKind::Terminal, Some(SessionId::new()));

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
    workspace.open_tab(PaneKind::Agent, None);

    assert_eq!(workspace.active_title(), "AI Agent");
    assert_eq!(workspace.close_tab_index(0), vec![first_session]);
    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.active_title(), "AI Agent");
}

#[test]
fn activate_visible_pane_switches_active_pane() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

    assert_eq!(workspace.active_visible_index(), 1);
    assert!(workspace.activate_visible_pane(0));
    assert_eq!(workspace.active_visible_index(), 0);
    assert!(!workspace.activate_visible_pane(5));
    assert_eq!(workspace.active_visible_index(), 0);
}

#[test]
fn pane_summaries_include_split_panes_by_tab() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

    assert_eq!(
        workspace.pane_summaries(),
        vec![
            PaneSummary {
                tab_index: 0,
                pane_index: 0,
                title: "Terminal #1".to_string(),
                kind: PaneKind::Terminal,
                active: false,
                tab_active: true,
            },
            PaneSummary {
                tab_index: 0,
                pane_index: 1,
                title: "Terminal #2".to_string(),
                kind: PaneKind::Terminal,
                active: true,
                tab_active: true,
            },
        ]
    );
}

#[test]
fn activate_pane_index_switches_tab_and_pane() {
    let mut workspace = Workspace::mvp();
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
    workspace.open_tab(PaneKind::Agent, None);

    assert_eq!(workspace.active_tab_index(), 1);
    assert!(workspace.activate_pane_index(0, 0));
    assert_eq!(workspace.active_tab_index(), 0);
    assert_eq!(workspace.active_visible_index(), 0);
    assert_eq!(workspace.active_title(), "Terminal #1");
    assert!(!workspace.activate_pane_index(9, 0));
    assert!(!workspace.activate_pane_index(0, 9));
    assert_eq!(workspace.active_title(), "Terminal #1");
}

#[test]
fn active_tab_index_tracks_active_tab() {
    let mut workspace = Workspace::mvp();
    workspace.open_tab(PaneKind::Agent, None);

    assert_eq!(workspace.active_tab_index(), 1);
    assert!(workspace.activate_tab_index(0));
    assert_eq!(workspace.active_tab_index(), 0);
}

#[test]
fn terminate_session_removes_session_and_attachments() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(second_session));

    assert!(workspace.terminate_session(second_session));
    assert_eq!(workspace.session_count(), 1);
    assert!(!workspace.session_is_referenced(second_session));
    assert!(workspace.session_is_referenced(first_session));
    assert_eq!(workspace.visible_panes().len(), 1);
}

#[test]
fn terminate_active_session_returns_removed_session() {
    let mut workspace = Workspace::mvp();
    let first_session = workspace.active_terminal_session_id().expect("session");
    let second_session = SessionId::new();
    workspace.split_active(PaneKind::Terminal, Some(second_session));

    assert_eq!(workspace.terminate_active_session(), Some(second_session));
    assert_eq!(workspace.session_count(), 1);
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
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
    workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));

    assert_eq!(workspace.visible_panes().len(), 4);
    let root = &workspace.tabs[workspace.active_tab_index()].root;
    assert!(root.is_canonical());
    match root {
        LayoutNode::Split { children, .. } => assert_eq!(children.len(), 4),
        LayoutNode::Pane(_) => panic!("expected a single flat row of 4 panes"),
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
    workspace.split_active(PaneKind::Terminal, Some(second_session));
    workspace.activate_visible_pane(0);

    let third_session = workspace
        .split_session_with_new_session(
            first_session,
            PaneKind::Terminal,
            SplitAxis::Horizontal,
            true,
        )
        .expect("split next to the first session's pane");

    let root = &workspace.tabs[workspace.active_tab_index()].root;
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
        .split_session_with_new_session(session, PaneKind::Terminal, SplitAxis::Vertical, true)
        .expect("split next to the session's pane");

    let root = &workspace.tabs[workspace.active_tab_index()].root;
    match root {
        LayoutNode::Split { axis, children } => {
            assert_eq!(*axis, SplitAxis::Vertical);
            assert_eq!(children.len(), 2);
        }
        LayoutNode::Pane(_) => panic!("expected a vertical split of 2 panes"),
    }
}
