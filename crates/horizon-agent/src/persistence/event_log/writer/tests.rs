use uuid::Uuid;

use super::*;
use crate::contract::{Event, SessionState};
use crate::persistence::event_log::{read, read_session};

pub(crate) fn record_at(session_id: SessionId, sequence: u64) -> Record {
    Record {
        schema: super::super::AGENT_EVENT_LOG_SCHEMA.to_string(),
        version: super::super::AGENT_EVENT_LOG_VERSION,
        event_id: Uuid::new_v4().to_string(),
        parent_id: None,
        sequence,
        session_id,
        turn_id: None,
        provider_id: None,
        role_id: None,
        session_context: None,
        event_kind: "state_changed".to_string(),
        event: Event::StateChanged(SessionState::Running),
        provider_payload: None,
        created_at_unix_ms: sequence + 1,
    }
}

fn sequences(dir: &Path, session: SessionId) -> Vec<u64> {
    read_session(dir, session)
        .unwrap()
        .records
        .iter()
        .map(|record| record.sequence)
        .collect()
}

#[test]
fn each_session_counts_its_own_sequence_and_goes_on_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (SessionId::new(), SessionId::new());
    let writer = WriterHandle::open(dir.path());
    for session in [first, second, first] {
        writer.append(record_at(session, 999)).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(sequences(dir.path(), first), [0, 1]);
    assert_eq!(sequences(dir.path(), second), [0]);

    let writer = WriterHandle::open(dir.path());
    writer.append(record_at(first, 0)).unwrap();
    writer.flush().unwrap();
    assert_eq!(sequences(dir.path(), first), [0, 1, 2]);
}

fn assert_restart_discards_partial_tail(prefix: &[u8], tail: &[u8], next_sequence: u64) {
    let dir = tempfile::tempdir().unwrap();
    let session = SessionId::new();
    let path = session_path(dir.path(), session);
    let mut contents = prefix.to_vec();
    contents.extend_from_slice(tail);
    std::fs::write(&path, contents).unwrap();
    let before = read(&path).expect("an unfinished tail is not a record");
    assert!(before.ignored_partial_line);

    let writer = WriterHandle::open(dir.path());
    let appended = record_at(session, 999);
    writer.append(appended.clone()).unwrap();
    writer.flush().unwrap();

    let after = read(&path).unwrap();
    assert_eq!(after.records.len(), before.records.len() + 1);
    assert_eq!(after.corrupt_line_count, before.corrupt_line_count);
    assert!(!after.ignored_partial_line);
    let last = after.records.last().unwrap();
    assert_eq!(last.event_id, appended.event_id);
    assert_eq!(last.sequence, next_sequence);
    assert!(std::fs::read(&path).unwrap().starts_with(prefix));
}

#[test]
fn restart_preserves_complete_lines_and_discards_a_long_partial_tail() {
    let record = serde_json::to_string(&record_at(SessionId::new(), 4)).unwrap();
    let prefix = format!("broken complete line\n{record}\n");
    assert_restart_discards_partial_tail(prefix.as_bytes(), &vec![b'x'; 20_000], 5);
}

#[test]
fn restart_does_not_promote_an_unterminated_valid_record() {
    let tail = serde_json::to_vec(&record_at(SessionId::new(), 99)).unwrap();
    assert_restart_discards_partial_tail(b"", &tail, 0);
}

#[test]
fn restart_discards_a_tail_split_inside_a_utf8_character() {
    let record = serde_json::to_string(&record_at(SessionId::new(), 2)).unwrap();
    let prefix = format!("{record}\n");
    let character = "あ".as_bytes();
    assert_restart_discards_partial_tail(prefix.as_bytes(), &character[..2], 3);
}

#[test]
fn a_session_file_that_cannot_be_opened_stops_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_directory = dir.path().join("file");
    std::fs::write(&not_a_directory, "").unwrap();
    let writer = WriterHandle::open(&not_a_directory);
    let failures = writer.subscribe_failure();
    writer.append(record_at(SessionId::new(), 0)).unwrap();
    assert!(writer.flush().is_err(), "the failed open is reported");
    assert!(failures.receiver().try_recv().is_ok());
    assert!(writer.append(record_at(SessionId::new(), 0)).is_err());
}

struct FailOnce {
    on_flush: bool,
    failed: bool,
}

impl Write for FailOnce {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.on_flush && !self.failed {
            self.failed = true;
            return Err(std::io::Error::other("injected write failure"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.on_flush && !self.failed {
            self.failed = true;
            return Err(std::io::Error::other("injected flush failure"));
        }
        Ok(())
    }
}

fn failing_writer(on_flush: bool) -> WriterHandle {
    WriterHandle::start(move |_| {
        Ok(SessionFile {
            path: PathBuf::from("injected.jsonl"),
            writer: BufWriter::new(Box::new(FailOnce {
                on_flush,
                failed: false,
            })),
            next_sequence: 0,
        })
    })
}

#[test]
fn a_failed_write_or_flush_is_retained_by_every_later_flush() {
    for on_flush in [false, true] {
        let writer = failing_writer(on_flush);
        let session = SessionId::new();
        writer.append(record_at(session, 0)).unwrap();
        let first = writer.flush();
        assert!(writer.append(record_at(session, 1)).is_err());
        let second = writer.flush();
        assert!(first.is_err(), "a later flush reports the failed append");
        assert!(
            second.is_err(),
            "the writer does not recover across an uncertain record boundary"
        );
    }
}

#[test]
fn failed_durable_input_does_not_enter_live_history() {
    use crate::contract::SessionInput;
    use crate::live::LiveState;

    for on_flush in [false, true] {
        let writer = failing_writer(on_flush);
        let history = vec![Event::StateChanged(SessionState::WaitingForUser)];
        let live = LiveState::with_event_log_and_history(
            SessionId::new(),
            None,
            None,
            writer.clone(),
            history.clone(),
        );
        let frame = live.frame();
        let result = live.persist_provider_events([Event::InputAccepted(SessionInput {
            id: "retryable-input".into(),
            origin: "owner".into(),
            text: "work".into(),
            reply_to: None,
            resume_work: false,
        })
        .into()]);
        assert!(result.is_err(), "an enqueue is not a durable acceptance");
        assert_eq!(
            live.events(),
            history,
            "failed input must not suppress redelivery"
        );
        assert_eq!(live.frame(), frame);
        assert!(writer.flush().is_err());
    }
}
