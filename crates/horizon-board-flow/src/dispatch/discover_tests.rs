use super::*;
use horizon_agent::contract::{Event, SessionState};
use horizon_agent::persistence::event_log::PersistedSessionContext;
use horizon_agent::roles::RoleId;
use horizon_agent_host::session::test_support::record;
use std::cell::Cell;

/// A record of a board task's session whose workspace is `root`.
fn task_record(root: &Path) -> Record {
    let mut record = record(
        SessionId::new(),
        Event::StateChanged(SessionState::WaitingForUser),
    );
    record.role_id = Some(RoleId(crate::roles::TASK.into()));
    record.session_context = Some(PersistedSessionContext {
        workspace_root: Some(root.into()),
        isolated_worktree: true,
        parent_session_id: None,
        filesystem_grants: Vec::new(),
        spawn_source_session_id: None,
    });
    record
}

#[test]
fn a_candidate_that_does_not_resolve_is_asked_about_once() {
    let gone = PathBuf::from("/nonexistent/.horizon/worktrees/0a1b2c3d");
    let asked = Cell::new(0);
    let resolve = |_: &Path| {
        asked.set(asked.get() + 1);
        None
    };
    let mut source = BoardSource::default();
    for _ in 0..3 {
        source.discover(vec![gone.clone()], resolve).unwrap();
    }
    assert_eq!(asked.get(), 1);
    assert!(source.projects.is_empty());
}

#[test]
fn a_record_naming_an_unresolved_candidate_has_it_asked_about_again() {
    let gone = PathBuf::from("/nonexistent/.horizon/worktrees/0a1b2c3d");
    let asked = Cell::new(0);
    let resolve = |_: &Path| {
        asked.set(asked.get() + 1);
        None
    };
    let state = horizon_agent_host::session::test_support::state_with_rig_config(false, "test");
    let mut source = BoardSource::default();
    let watched = source.watched.clone();
    source.observe(&state, &task_record(&gone));
    source.discover(watched.list(), resolve).unwrap();
    source.discover(watched.list(), resolve).unwrap();
    assert_eq!(asked.get(), 1);

    source.observe(&state, &task_record(&gone));
    source.discover(watched.list(), resolve).unwrap();
    assert_eq!(asked.get(), 2);
}

#[test]
fn a_candidate_whose_board_opens_is_asked_about_once() {
    let dir = super::review_tests::repository();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let asked = Cell::new(0);
    let resolve = |_: &Path| {
        asked.set(asked.get() + 1);
        Some(root.clone())
    };
    let mut source = BoardSource::default();
    for _ in 0..2 {
        source.discover(vec![root.clone()], resolve).unwrap();
    }
    assert_eq!(asked.get(), 1);
    assert_eq!(source.projects.len(), 1);
}
