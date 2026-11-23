//! The workspace as one tree with tab groups among its nodes
//! (`docs/workspace-layout-design.md`): what is put beside a group, where
//! the cursor goes, what operations act on, where a session no pane shows
//! is opened, and what goes when a group empties.

use super::super::types::{LayoutNode, NodeKey, TabGroupId, TabId};
use super::super::*;
use crate::SessionId;
use crate::operations::Content;

/// The plugin of a view pane, for a pane that holds no session.
fn view(name: &str) -> PluginPane {
    PluginPane {
        name: name.to_string(),
        title: name.to_string(),
        state: serde_json::Value::Null,
    }
}

/// The group at the root of a fresh workspace.
fn root_group(workspace: &Workspace) -> TabGroupId {
    match workspace.layout() {
        LayoutNode::Tabs(group) => group.id,
        _ => panic!("the root is a group"),
    }
}

/// A workspace with two tabs in its group and a list pane on the left of
/// the group, the cursor left on the list. Returns the list's pane.
fn with_a_list_on_the_left(workspace: &mut Workspace) -> PaneId {
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Up);
    assert_eq!(
        workspace.workspace_cursor(),
        Some(Cursor::Strip(root_group(workspace)))
    );
    let list = workspace.split_cursor_with_view(view("sessions"), Side::Left);
    workspace.exit_workspace_mode();
    list
}

#[test]
fn a_pane_split_beside_a_strip_goes_beside_the_whole_group() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    let list = with_a_list_on_the_left(&mut workspace);

    match workspace.layout() {
        LayoutNode::Split { axis, children } => {
            assert_eq!(*axis, SplitAxis::Horizontal);
            assert_eq!(children[0].node.first_key(), NodeKey::Pane(list));
            assert_eq!(children[1].node.first_key(), NodeKey::Group(group));
        }
        _ => panic!("expected the list beside the group"),
    }
    assert!(workspace.layout().is_canonical());
    // The new pane has focus, and stays on screen whatever tab is shown.
    assert!(workspace.is_active_pane(list));
    for index in [0, 1, 0] {
        assert!(workspace.activate_tab_in(group, index));
        assert!(workspace.visible_pane_ids().contains(&list));
    }
    assert_eq!(workspace.tab_groups().len(), 1);
}

#[test]
fn the_cursor_reaches_a_strip_from_the_top_pane_and_leaves_it_by_geometry() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    let list = with_a_list_on_the_left(&mut workspace);
    let shown = workspace.active_tab().expect("a tab").active;

    // From the list, right is the pane the group shows; up from it, the
    // strip; left from the strip, the list, which runs the full height.
    workspace.activate_pane(list);
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Right);
    assert_eq!(workspace.workspace_cursor(), Some(Cursor::Pane(shown)));
    workspace.move_cursor(Direction::Up);
    assert_eq!(workspace.workspace_cursor(), Some(Cursor::Strip(group)));
    workspace.move_cursor(Direction::Down);
    assert_eq!(workspace.workspace_cursor(), Some(Cursor::Pane(shown)));
    workspace.move_cursor(Direction::Up);
    workspace.move_cursor(Direction::Left);
    assert_eq!(workspace.workspace_cursor(), Some(Cursor::Pane(list)));

    // A strip is no pane, and Enter on it focuses the group's shown tab.
    workspace.move_cursor(Direction::Right);
    workspace.move_cursor(Direction::Up);
    assert_eq!(workspace.cursor_pane_id(), None);
    assert_eq!(workspace.cursor_group(), Some(group));
    workspace.commit_workspace_mode();
    assert!(workspace.is_active_pane(shown));
}

#[test]
fn tab_operations_act_on_the_group_the_cursor_is_in() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    let list = with_a_list_on_the_left(&mut workspace);

    // From the list, no group is the cursor's, so tabs are counted of the
    // group that last had focus.
    workspace.activate_pane(list);
    assert_eq!(workspace.cursor_group(), None);
    assert_eq!(workspace.current_group(), Some(group));
    assert_eq!(workspace.tab_count(), 2);

    // A tab opened with the cursor on the strip goes in that group.
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Right);
    workspace.move_cursor(Direction::Up);
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));
    workspace.exit_workspace_mode();
    assert_eq!(workspace.tab_count(), 3);
    assert_eq!(workspace.tab_groups().len(), 1);
}

#[test]
fn the_new_tab_command_on_a_pane_under_no_group_makes_its_place_a_group() {
    let mut workspace = Workspace::mvp();
    let list = with_a_list_on_the_left(&mut workspace);
    workspace.activate_pane(list);

    workspace.new_tab_at_cursor_with_new_session(SessionKind::Terminal);

    let groups = workspace.tab_groups();
    assert_eq!(groups.len(), 2);
    let made = groups
        .iter()
        .find(|group| group.tabs.iter().any(|tab| tab.root.pane_ids() == [list]))
        .expect("a group with the list as its first tab");
    assert_eq!(made.tabs.len(), 2);
    assert_eq!(made.shown_index(), Some(1));
    assert!(workspace.layout().is_canonical());
}

#[test]
fn a_session_no_pane_shows_opens_in_the_group_that_last_had_focus() {
    let mut workspace = Workspace::mvp();
    let detached = SessionId::new();
    workspace.register_detached_session(SessionKind::Agent, detached);
    let list = with_a_list_on_the_left(&mut workspace);
    workspace.activate_pane(list);
    let list_width = |workspace: &Workspace| match workspace.layout() {
        LayoutNode::Split { children, .. } => children.len(),
        _ => 0,
    };
    assert_eq!(list_width(&workspace), 2);

    assert!(workspace.show_session(detached));

    // The list is not split: the session's pane is in the group's shown
    // tab, and has focus.
    assert_eq!(list_width(&workspace), 2);
    let pane = workspace
        .pane_location_for_session(detached)
        .expect("a pane");
    assert!(workspace.is_active_pane(pane));
    assert_eq!(
        workspace.active_tab().expect("a tab").root.pane_ids().len(),
        2
    );
    assert!(
        workspace
            .active_tab()
            .expect("a tab")
            .root
            .pane_ids()
            .contains(&pane)
    );
}

#[test]
fn a_group_down_to_one_pane_is_that_pane_and_panes_beside_it_stay() {
    let mut workspace = Workspace::mvp();
    let list = with_a_list_on_the_left(&mut workspace);
    let group = root_group_of(&workspace);

    // Down to one tab of one pane, the group is that pane, beside the list.
    workspace.close_tab_in(group, 0);
    assert!(workspace.tab_groups().is_empty());
    let left = workspace.layout().pane_ids();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0], list);
    assert!(workspace.is_active_pane(left[1]));
    assert!(workspace.layout().is_canonical());
    workspace.close_pane_or_tab(left[1]);

    assert_eq!(workspace.layout().pane_ids(), vec![list]);
    assert!(workspace.tab_groups().is_empty());
    assert!(workspace.is_active_pane(list));
    // The list closed too, the workspace is empty: a root group with no
    // tab.
    workspace.enter_workspace_mode();
    assert_eq!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedPane(None)
    );
    std::assert_matches!(workspace.layout(), LayoutNode::Tabs(group) if group.tabs.is_empty());
    assert!(workspace.is_workspace_mode_active());
}

/// The panes of `group`'s tab at `index`.
fn panes_of(workspace: &Workspace, group: TabGroupId, index: usize) -> Vec<PaneId> {
    workspace.tab_group(group).expect("the group").tabs[index]
        .root
        .pane_ids()
}

#[test]
fn a_group_whose_one_tab_is_split_keeps_its_strip_until_one_pane_is_left() {
    let mut workspace = Workspace::mvp();
    let list = with_a_list_on_the_left(&mut workspace);
    let group = root_group_of(&workspace);
    workspace.activate_tab_in(group, 1);
    workspace.split_active(SessionKind::Terminal, SessionId::new());
    let split = panes_of(&workspace, group, 1);
    assert_eq!(split.len(), 2);

    // One tab, split: the group stays.
    workspace.close_tab_in(group, 0);
    assert_eq!(workspace.tab_groups().len(), 1);
    assert_eq!(panes_of(&workspace, group, 0), split);
    // A pane moved beside the other in that tab: it stays too.
    assert!(workspace.move_pane_beside(split[1], split[0], Side::Down));
    assert_eq!(workspace.tab_groups().len(), 1);
    assert_eq!(panes_of(&workspace, group, 0), split);

    // One pane left: the group is that pane.
    workspace.close_pane_or_tab(split[1]);
    assert!(workspace.tab_groups().is_empty());
    assert_eq!(workspace.layout().pane_ids(), vec![list, split[0]]);
    assert!(workspace.is_active_pane(split[0]));
    assert!(workspace.layout().is_canonical());
}

#[test]
fn the_root_group_keeps_its_strip_down_to_one_pane() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));
    workspace.close_tab_in(group, 0);
    std::assert_matches!(workspace.layout(), LayoutNode::Tabs(kept) if kept.tabs.len() == 1);
}

#[test]
fn the_cursor_on_a_strip_whose_group_goes_is_on_the_pane_left() {
    let mut workspace = Workspace::mvp();
    with_a_list_on_the_left(&mut workspace);
    let group = root_group_of(&workspace);
    workspace.workspace_mode = crate::mode::WorkspaceMode::Commands {
        cursor: Some(Cursor::Strip(group)),
    };

    // `x` on the strip closes the tab it shows: the group is the other
    // tab's pane, and the cursor is on it.
    std::assert_matches!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedTab(_)
    );
    assert!(workspace.tab_groups().is_empty());
    let pane = workspace.layout().pane_ids()[1];
    assert_eq!(workspace.workspace_cursor(), Some(Cursor::Pane(pane)));
}

/// The one group in a workspace with a list beside it.
fn root_group_of(workspace: &Workspace) -> TabGroupId {
    workspace.tab_groups()[0].id
}

#[test]
fn x_on_a_strip_closes_the_tab_the_group_shows() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    workspace.open_tab(Content::Session(SessionKind::Terminal, SessionId::new()));
    workspace.enter_workspace_mode();
    workspace.move_cursor(Direction::Up);

    std::assert_matches!(
        workspace.close_cursor_pane_or_tab(),
        CloseCursorOutcome::ClosedTab(_)
    );
    assert_eq!(workspace.tab_count(), 1);
    assert_eq!(workspace.current_group(), Some(group));
}

#[test]
fn a_tree_with_groups_round_trips_through_its_saved_state() {
    let mut workspace = Workspace::mvp();
    let list = with_a_list_on_the_left(&mut workspace);
    workspace.activate_pane(list);
    workspace.new_tab_at_cursor_with_new_session(SessionKind::Terminal);

    let json = workspace.to_persisted_json().expect("serialize");
    let restored = Workspace::from_persisted_json(&json).expect("restore");

    assert_eq!(restored.layout().pane_ids(), workspace.layout().pane_ids());
    assert_eq!(restored.tab_groups().len(), 2);
    assert_eq!(restored.active_session_id(), workspace.active_session_id());
    assert!(restored.layout().is_canonical());
}

#[test]
fn a_saved_state_of_version_1_is_not_read() {
    let json = serde_json::json!({
        "version": 1,
        "active_tab": TabId::new(),
        "next_terminal_display_number": 1,
        "next_agent_display_number": 1,
        "tabs": [],
        "sessions": [],
    })
    .to_string();

    match Workspace::from_persisted_json(&json) {
        Err(WorkspaceStateError::Malformed(_))
        | Err(WorkspaceStateError::UnsupportedVersion { .. }) => {}
        other => panic!("expected a version 1 state refused, got {other:?}"),
    }
}

#[test]
fn a_tab_opened_by_anything_else_goes_in_the_group_that_last_had_focus() {
    let mut workspace = Workspace::mvp();
    let group = root_group(&workspace);
    let list = with_a_list_on_the_left(&mut workspace);
    workspace.activate_pane(list);

    // (As the command line, or the list's own "terminal here", opens one.)
    workspace.open_tab_with_new_session_activated(SessionKind::Terminal, true);

    assert_eq!(workspace.tab_groups().len(), 1);
    assert_eq!(workspace.tab_group(group).expect("the group").tabs.len(), 3);
    assert!(workspace.visible_pane_ids().contains(&list));
}

#[test]
fn with_no_group_a_tab_opens_in_a_new_one_right_of_the_rest() {
    let mut workspace = Workspace::mvp();
    let list = with_a_list_on_the_left(&mut workspace);
    let group = root_group_of(&workspace);
    workspace.close_tab_in(group, 0);
    let left = workspace.layout().pane_ids()[1];
    workspace.close_pane_or_tab(left);
    assert!(workspace.tab_groups().is_empty());

    workspace.open_tab_with_new_session_activated(SessionKind::Terminal, true);

    match workspace.layout() {
        LayoutNode::Split { children, .. } => {
            assert_eq!(children[0].node.first_key(), NodeKey::Pane(list));
            std::assert_matches!(children[1].node, LayoutNode::Tabs(_));
        }
        _ => panic!("expected the list and a new group right of it"),
    }
    assert!(workspace.layout().is_canonical());
}
