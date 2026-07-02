mod input;
mod layout;
mod operations;
mod types;
pub mod view;

pub use input::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane,
    handle_active_pane_key, request_active_pane_focus, trace_ime, visible_agent_sender,
    visible_terminal_sender, AgentDrafts, PaneFocusRequests, MAX_VISIBLE_PANES,
};
pub use types::{
    LayoutNode, Pane, PaneId, PaneKind, PaneSummary, SessionKind, SessionSummary, SplitAxis, Tab,
    TabId, TabSummary, Workspace, WorkspaceSession,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

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
        let pane_id = workspace.attach_session_to_split(session_id);

        assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
        assert_eq!(workspace.visible_terminal_session_id(1), Some(session_id));
        assert!(workspace.session_is_referenced(session_id));
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
    fn detach_reports_session_and_removes_reference() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        let pane_id = workspace.attach_session_to_split(session_id);

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
        workspace.attach_session_to_split(session_id);

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
        workspace.attach_session_to_split(session_id);
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
        workspace.attach_session_to_split(detached_session);
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
        workspace.attach_session_to_split(session_id);

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
            .attach_existing_session_to_split(session_id)
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
        workspace.attach_session_to_split(second_session);
        workspace.terminate_session(second_session);

        let third_session = SessionId::new();
        workspace.attach_session_to_split(third_session);

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
            .attach_existing_session_to_split(session_id)
            .expect("attached pane");

        assert_eq!(workspace.visible_pane_id(1), Some(pane_id));
        assert_eq!(workspace.visible_panes()[1].kind, PaneKind::Agent);
        assert!(workspace.session_is_referenced(session_id));
        assert_eq!(workspace.detached_session_count(), 0);
    }

    #[test]
    fn open_tab_with_new_session_attaches_requested_kind() {
        let mut workspace = Workspace::mvp();

        let session_id = workspace.open_tab_with_new_session(PaneKind::Agent);

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
    fn close_tab_index_keeps_last_tab() {
        let mut workspace = Workspace::mvp();

        assert!(workspace.close_tab_index(0).is_empty());
        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(workspace.visible_panes().len(), 1);
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
        workspace.attach_session_to_split(second_session);

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

        assert_eq!(workspace.tab_summaries()[2].active, true);
        assert_eq!(workspace.close_tab_index(2).len(), 1);
        assert_eq!(workspace.tab_count(), 2);
        assert_eq!(workspace.tab_summaries()[1].active, true);
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
        workspace.attach_session_to_split(SessionId::new());

        assert_eq!(workspace.active_visible_index(), 1);
        assert!(workspace.activate_visible_pane(0));
        assert_eq!(workspace.active_visible_index(), 0);
        assert!(!workspace.activate_visible_pane(5));
        assert_eq!(workspace.active_visible_index(), 0);
    }

    #[test]
    fn pane_summaries_include_split_panes_by_tab() {
        let mut workspace = Workspace::mvp();
        workspace.attach_session_to_split(SessionId::new());

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
        workspace.attach_session_to_split(SessionId::new());
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
        workspace.attach_session_to_split(second_session);

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
        workspace.attach_session_to_split(second_session);

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
}
