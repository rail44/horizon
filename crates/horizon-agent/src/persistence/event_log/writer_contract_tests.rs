use super::tests::{message_record, record_at};
use super::*;
use crate::contract::{Event, SessionId, TurnEndReason};
use crate::live::LiveState;

#[test]
fn equal_tail_with_a_missing_middle_record_rebuilds_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.duckdb");
    let session = SessionId::new();
    let records: Vec<_> = (0..3)
        .map(|seq| message_record(session, seq, "message"))
        .collect();
    {
        let store = Store::open(&path).unwrap();
        store
            .replace_from_event_log_records([records[0].clone(), records[2].clone()])
            .unwrap();
        assert_eq!(store.max_last_sequence().unwrap(), Some(2));
        assert!(matches!(
            duckdb_projection_currency(&store, &records).unwrap(),
            ProjectionCurrency::RebuildNeeded
        ));
    }
    let rebuilt = rebuild_and_open_duckdb_projection(&path, &records).unwrap();
    assert!(rebuilt
        .query(|store| store.matches_log_prefix(&records))
        .unwrap());
    assert_eq!(
        rebuilt
            .query(|store| store.messages_for_session(session))
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn replacing_the_log_with_equal_sequences_rebuilds_different_event_ids() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new();
    store
        .replace_from_event_log_records([record_at(session, 0)])
        .unwrap();
    assert!(matches!(
        duckdb_projection_currency(&store, &[record_at(session, 0)]).unwrap(),
        ProjectionCurrency::RebuildNeeded
    ));
}

#[test]
fn projection_failure_revokes_readers_but_keeps_writing_the_authoritative_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let db_path = dir.path().join("history.duckdb");
    let store = rebuild_and_open_duckdb_projection(&db_path, &[]).unwrap();
    let reader = store.clone();
    let (tx, rx) = unbounded();
    let health = WriterHealth::default();
    let writer = WriterHandle {
        tx,
        health: health.clone(),
    };
    let file = std::fs::File::create(&path).unwrap();
    let worker_path = path.clone();
    let worker = thread::spawn(move || run_writer(file, &worker_path, rx, 0, Some(store), health));
    let session = SessionId::new();
    writer.append(message_record(session, 0, "before")).unwrap();
    writer.flush().unwrap();
    assert_eq!(
        reader.query(|store| store.max_last_sequence()).unwrap(),
        Some(0)
    );
    // Turn projection requires an identity; the JSONL envelope itself is valid.
    let mut invalid = record_at(session, 1);
    invalid.event = Event::TurnEnded(TurnEndReason::Failed);
    invalid.event_kind = "turn_ended".into();
    writer.append(invalid).unwrap();
    writer.append(message_record(session, 2, "after")).unwrap();
    writer.flush().unwrap();
    assert!(writer.failure().is_none());
    assert!(reader
        .query(|store| store.max_last_sequence())
        .unwrap_err()
        .to_string()
        .contains("unavailable"));
    drop(writer);
    worker.join().unwrap();
    drop(reader);
    let records = read(&path).unwrap().records;
    assert_eq!(records.len(), 3);
    // Restart must not hand out a DB that skipped the same invalid record.
    assert!(rebuild_and_open_duckdb_projection(&db_path, &records).is_none());
    let mut repaired = records;
    repaired[1].turn_id = Some("recovered-turn".into());
    let rebuilt = rebuild_and_open_duckdb_projection(&db_path, &repaired).unwrap();
    assert!(rebuilt
        .query(|store| store.matches_log_prefix(&repaired))
        .unwrap());
}

struct FailAfterFirstRecord {
    file: std::fs::File,
    flushed: bool,
}
impl Write for FailAfterFirstRecord {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.flushed {
            return Err(std::io::Error::other("injected disk failure"));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushed = true;
        self.file.flush()
    }
}

#[test]
fn a_failed_batch_is_not_published_and_restart_recovers_only_its_saved_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let (tx, rx) = unbounded();
    let health = WriterHealth::default();
    let writer = WriterHandle {
        tx,
        health: health.clone(),
    };
    let file = FailAfterFirstRecord {
        file: std::fs::File::create(&path).unwrap(),
        flushed: false,
    };
    let worker_path = path.clone();
    let worker = thread::spawn(move || run_writer(file, &worker_path, rx, 0, None, health));
    let session = SessionId::new();
    let live =
        LiveState::with_event_log_and_history(session, None, None, writer.clone(), Vec::new());
    let events = [
        message_record(session, 0, "saved"),
        message_record(session, 1, "unsaved"),
    ];
    assert!(live
        .extend_provider_events(events.iter().map(|record| record.event.clone().into()))
        .is_err());
    assert!(live.events().is_empty());
    assert!(live
        .extend_provider_events([record_at(session, 2).event.into()])
        .is_err());
    drop(live);
    drop(writer);
    worker.join().unwrap();
    let (restarted, ready) = WriterHandle::open(&path);
    let WriterInit::Ready(report) = ready.recv().unwrap() else {
        panic!("restart failed")
    };
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].event, events[0].event);
    restarted
        .append(message_record(session, 99, "recovered"))
        .unwrap();
    restarted.flush().unwrap();
    let recovered = read(&path).unwrap();
    assert_eq!(
        recovered
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}
