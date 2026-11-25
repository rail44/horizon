use super::*;
use crate::contract::{Event, Message, MessageRole};
use crate::persistence::event_log::{WriterHandle, session_path};
use crate::persistence::projection::duckdb::{Access, Projection};

fn message(session: SessionId, text: &str) -> crate::persistence::event_log::Record {
    crate::persistence::event_log::Record {
        schema: "horizon.agent.event_log".into(),
        version: AGENT_EVENT_LOG_VERSION,
        event_id: uuid::Uuid::new_v4().to_string(),
        parent_id: None,
        sequence: 0,
        session_id: session,
        turn_id: None,
        provider_id: None,
        role_id: None,
        session_context: None,
        event_kind: "message_committed".into(),
        event: Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: text.into(),
        }),
        provider_payload: None,
        created_at_unix_ms: 1,
    }
}

fn texts(projection: &Projection, session: SessionId) -> Vec<String> {
    projection
        .query(|store| store.messages_for_session(session))
        .unwrap()
        .into_iter()
        .map(|message| message.text)
        .collect()
}

#[test]
fn a_query_sees_every_record_written_before_it_in_every_session() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let projection = Projection::new(&sessions, dir.path().join("state.duckdb"));
    let (first, second) = (SessionId::new(), SessionId::new());
    let writer = WriterHandle::open(&sessions);
    writer.append(message(first, "one")).unwrap();
    writer.append(message(second, "two")).unwrap();
    writer.flush().unwrap();

    assert_eq!(texts(&projection, first), ["one"]);
    assert_eq!(texts(&projection, second), ["two"]);

    writer.append(message(first, "three")).unwrap();
    writer.flush().unwrap();
    assert_eq!(texts(&projection, first), ["one", "three"]);
}

#[test]
fn a_catch_up_reads_only_what_each_file_gained() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let db = dir.path().join("state.duckdb");
    let session = SessionId::new();
    let writer = WriterHandle::open(&sessions);
    writer.append(message(session, "one")).unwrap();
    writer.flush().unwrap();

    let store = Store::open_waiting(&db, Access::ReadWrite).unwrap();
    assert_eq!(store.catch_up_from_sessions(&sessions).unwrap().applied, 1);
    assert_eq!(store.catch_up_from_sessions(&sessions).unwrap().applied, 0);
    writer.append(message(session, "two")).unwrap();
    writer.flush().unwrap();
    assert_eq!(store.catch_up_from_sessions(&sessions).unwrap().applied, 1);
}

#[test]
fn a_file_replaced_underneath_its_cursor_rebuilds_the_projection() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let db = dir.path().join("state.duckdb");
    let session = SessionId::new();
    let writer = WriterHandle::open(&sessions);
    writer.append(message(session, "before")).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let store = Store::open_waiting(&db, Access::ReadWrite).unwrap();
    store.catch_up_from_sessions(&sessions).unwrap();

    std::fs::remove_file(session_path(&sessions, session)).unwrap();
    let writer = WriterHandle::open(&sessions);
    writer.append(message(session, "after")).unwrap();
    writer.flush().unwrap();
    store.catch_up_from_sessions(&sessions).unwrap();

    let texts: Vec<String> = store
        .messages_for_session(session)
        .unwrap()
        .into_iter()
        .map(|message| message.text)
        .collect();
    assert_eq!(texts, ["after"]);
}

#[test]
fn a_session_is_as_recent_as_its_latest_record() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let projection = Projection::new(&sessions, dir.path().join("state.duckdb"));
    let session = SessionId::new();
    let writer = WriterHandle::open(&sessions);
    let mut record = message(session, "dated");
    record.created_at_unix_ms = 1_700_000_000_000;
    writer.append(record).unwrap();
    writer.flush().unwrap();

    let at: String = projection
        .query(|store| {
            Ok(store
                .list_sessions(None, None, None, None, 10)?
                .remove(0)
                .at)
        })
        .unwrap();
    assert!(at.starts_with("2023-11-14"), "{at}");
}
