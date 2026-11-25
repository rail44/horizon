use super::*;
use crate::contract::{Message, MessageRole};
use crate::persistence::event_log::WriterHandle;

fn record(session: SessionId, event: Event) -> Record {
    Record {
        schema: super::super::AGENT_EVENT_LOG_SCHEMA.into(),
        version: super::super::AGENT_EVENT_LOG_VERSION,
        event_id: uuid::Uuid::new_v4().to_string(),
        parent_id: None,
        sequence: 0,
        session_id: session,
        turn_id: None,
        provider_id: None,
        role_id: None,
        session_context: None,
        event_kind: crate::contract::event_kind(&event).into(),
        event,
        provider_payload: None,
        created_at_unix_ms: 1,
    }
}

fn message(text: &str) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::User,
        text: text.into(),
    })
}

fn context(source: Option<SessionId>) -> PersistedSessionContext {
    PersistedSessionContext {
        workspace_root: Some(PathBuf::from("/work")),
        isolated_worktree: false,
        parent_session_id: None,
        filesystem_grants: Vec::new(),
        spawn_source_session_id: source,
    }
}

fn texts(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| match &record.event {
            Event::MessageCommitted(message) => Some(message.text.clone()),
            _ => None,
        })
        .collect()
}

/// Appends `events` to `session` as one chain, the first naming `parent`.
fn chain(
    writer: &WriterHandle,
    session: SessionId,
    parent: Option<String>,
    source: Option<SessionId>,
    events: Vec<Event>,
) -> Vec<String> {
    let mut parent = parent;
    let mut ids = Vec::new();
    for event in events {
        let mut record = record(session, event);
        record.parent_id = parent.take();
        record.session_context = Some(context(source));
        parent = Some(record.event_id.clone());
        ids.push(record.event_id.clone());
        writer.append(record).unwrap();
    }
    writer.flush().unwrap();
    ids
}

#[test]
fn a_fork_continues_its_source_up_to_where_it_was_forked() {
    let dir = tempfile::tempdir().unwrap();
    let writer = WriterHandle::open(dir.path());
    let (source, fork) = (SessionId::new(), SessionId::new());
    let ids = chain(
        &writer,
        source,
        None,
        None,
        vec![message("a"), message("b"), message("after the fork")],
    );
    chain(
        &writer,
        fork,
        Some(ids[1].clone()),
        Some(source),
        vec![message("c")],
    );
    let fork_of_fork = SessionId::new();
    let fork_ids = read_session(dir.path(), fork).unwrap().records;
    chain(
        &writer,
        fork_of_fork,
        Some(fork_ids[0].event_id.clone()),
        Some(fork),
        vec![message("d")],
    );

    assert_eq!(
        texts(&read_branch(dir.path(), fork).unwrap()),
        ["a", "b", "c"]
    );
    assert_eq!(
        texts(&read_branch(dir.path(), fork_of_fork).unwrap()),
        ["a", "b", "c", "d"]
    );
    assert_eq!(
        texts(&read_branch(dir.path(), source).unwrap()),
        ["a", "b", "after the fork"]
    );
}

#[test]
fn a_fork_that_names_no_source_is_found_through_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let writer = WriterHandle::open(dir.path());
    let (source, fork) = (SessionId::new(), SessionId::new());
    let ids = chain(&writer, source, None, None, vec![message("a")]);
    chain(
        &writer,
        fork,
        Some(ids[0].clone()),
        None,
        vec![message("b")],
    );
    assert_eq!(texts(&read_branch(dir.path(), fork).unwrap()), ["a", "b"]);
}

#[test]
fn the_summary_reads_back_to_the_latest_state_and_the_latest_context() {
    let dir = tempfile::tempdir().unwrap();
    let writer = WriterHandle::open(dir.path());
    let session = SessionId::new();
    chain(
        &writer,
        session,
        None,
        None,
        vec![
            Event::StateChanged(SessionState::Running),
            Event::StateChanged(SessionState::Terminated),
        ],
    );
    // A judge verdict carries no context; a later acknowledgement no state.
    writer.append(record(session, message("verdict"))).unwrap();
    writer.flush().unwrap();

    let summary = session_summary(&session_path(dir.path(), session))
        .unwrap()
        .unwrap();
    assert_eq!(summary.session_id, session);
    assert!(summary.dead);
    assert_eq!(summary.session_context, Some(context(None)));
}

#[test]
fn a_session_waiting_for_its_user_is_alive() {
    let dir = tempfile::tempdir().unwrap();
    let writer = WriterHandle::open(dir.path());
    let session = SessionId::new();
    chain(
        &writer,
        session,
        None,
        None,
        vec![Event::StateChanged(SessionState::WaitingForUser)],
    );
    let summary = session_summary(&session_path(dir.path(), session))
        .unwrap()
        .unwrap();
    assert!(!summary.dead);
}

#[test]
fn backward_lines_skip_an_unterminated_tail_and_cross_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines");
    let long = "x".repeat(BackwardLines::CHUNK as usize + 10);
    std::fs::write(&path, format!("first\n{long}\n\nlast\ntorn")).unwrap();
    let mut lines = BackwardLines::open(&path).unwrap();
    let mut read = Vec::new();
    while let Some(line) = lines.next_line().unwrap() {
        read.push(String::from_utf8(line).unwrap());
    }
    assert_eq!(read, ["last".to_string(), long, "first".to_string()]);
    assert!(
        session_summary(&dir.path().join("missing"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn every_session_is_listed_and_read_together_in_time_order() {
    let dir = tempfile::tempdir().unwrap();
    let writer = WriterHandle::open(dir.path());
    let (first, second) = (SessionId::new(), SessionId::new());
    let mut later = record(first, message("later"));
    later.created_at_unix_ms = 20;
    let mut earlier = record(second, message("earlier"));
    earlier.created_at_unix_ms = 10;
    writer.append(later).unwrap();
    writer.append(earlier).unwrap();
    writer.flush().unwrap();
    std::fs::write(dir.path().join("not-a-session.txt"), "").unwrap();

    let mut listed = session_ids(dir.path()).unwrap();
    listed.sort_by_key(|session| session.as_uuid());
    let mut expected = vec![first, second];
    expected.sort_by_key(|session| session.as_uuid());
    assert_eq!(listed, expected);
    assert_eq!(
        texts(&read_all(dir.path()).unwrap().records),
        ["earlier", "later"]
    );
}
