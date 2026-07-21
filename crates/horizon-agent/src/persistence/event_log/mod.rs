use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::contract::{Event, ProviderId, SessionId};
use crate::roles::RoleId;

mod appender;
mod turn;
mod writer;

pub use appender::Appender;
use turn::TurnTracker;
pub use writer::{WriterHandle, WriterInit};

pub const AGENT_EVENT_LOG_SCHEMA: &str = "horizon.agent.event_log";
pub const AGENT_EVENT_LOG_VERSION: u32 = 1;

/// Host-resolved session placement needed to restore the same confinement
/// after `horizon-sessiond` restarts. This is deliberately event-log
/// metadata rather than a conversational [`Event`]: every newly appended
/// record carries the latest authoritative value, while old records decode
/// with `Record::session_context == None` and retain their legacy resume
/// behavior.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct PersistedSessionContext {
    pub workspace_root: Option<PathBuf>,
    pub isolated_worktree: bool,
    pub parent_session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Record {
    pub schema: String,
    pub version: u32,
    pub event_id: String,
    pub sequence: u64,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<ProviderId>,
    /// Mirrors `provider_id` exactly: `None` for a role-less session, and
    /// `#[serde(default)]` so a log record written before this field
    /// existed (schema/version unchanged -- this is additive, unlike the
    /// wire's breaking `SessionNew` change) still parses, reading back as
    /// `None` -- a resumed pre-existing session simply resumes role-less.
    #[serde(default)]
    pub role_id: Option<RoleId>,
    /// Added without changing the JSONL schema version: old records simply
    /// lack the host-only confinement metadata and resume through the legacy
    /// compatibility path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context: Option<PersistedSessionContext>,
    pub event_kind: String,
    pub event: Event,
    pub provider_payload: Option<serde_json::Value>,
    pub created_at_unix_ms: u64,
}

/// [`Record`] with the `event` payload left raw — the first stage of
/// [`decode_record_tolerantly`]'s two-stage decode. Field-for-field the
/// same envelope as [`Record`], so a line whose *envelope* is intact is
/// never lost just because its `event` is from a newer build.
#[derive(Deserialize)]
struct RecordEnvelope {
    schema: String,
    version: u32,
    event_id: String,
    sequence: u64,
    session_id: SessionId,
    turn_id: Option<String>,
    provider_id: Option<ProviderId>,
    #[serde(default)]
    role_id: Option<RoleId>,
    #[serde(default)]
    session_context: Option<PersistedSessionContext>,
    event_kind: String,
    #[allow(dead_code)]
    event: serde_json::Value,
    provider_payload: Option<serde_json::Value>,
    created_at_unix_ms: u64,
}

/// Decodes one log line, degrading only as far as the damage goes:
///
/// 1. A fully decodable line is a plain [`Record`].
/// 2. A line whose envelope (schema, `sequence`, session, ...) is intact
///    but whose `event` this build can't decode — a payload-carrying
///    variant appended by a newer build, which serde_json's
///    `#[serde(other)]` cannot degrade on its own (it insists on unit
///    content) — becomes a [`Record`] carrying [`Event::Unknown`]. The
///    envelope is preserved, so `sequence` stays monotonic: the writer's
///    `next_sequence` counts this line, and a later resume can never
///    re-issue its sequence number (the rewind/duplication hazard the
///    review found).
/// 3. Only a line whose envelope itself is broken counts as corrupt.
fn decode_record_tolerantly(line: &str) -> Option<Record> {
    if let Ok(record) = serde_json::from_str::<Record>(line) {
        return Some(record);
    }
    let envelope = serde_json::from_str::<RecordEnvelope>(line).ok()?;
    Some(Record {
        schema: envelope.schema,
        version: envelope.version,
        event_id: envelope.event_id,
        sequence: envelope.sequence,
        session_id: envelope.session_id,
        turn_id: envelope.turn_id,
        provider_id: envelope.provider_id,
        role_id: envelope.role_id,
        session_context: envelope.session_context,
        event_kind: envelope.event_kind,
        event: Event::Unknown,
        provider_payload: envelope.provider_payload,
        created_at_unix_ms: envelope.created_at_unix_ms,
    })
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadReport {
    pub records: Vec<Record>,
    pub corrupt_line_count: usize,
    pub ignored_partial_line: bool,
}

impl ReadReport {
    /// A short human-readable summary of lines `read` had to skip, or
    /// `None` when the file parsed cleanly. Every consumer of the raw JSONL
    /// (the writer's own startup re-read in `event_log::writer`, the DuckDB
    /// rebuild `open_silently` drives, and `horizon-sessiond`'s
    /// `open_persistence`) reports this instead of silently discarding
    /// evidence that the file has corrupt or torn lines.
    pub fn skipped_summary(&self) -> Option<String> {
        if self.corrupt_line_count == 0 && !self.ignored_partial_line {
            return None;
        }
        let mut parts = Vec::new();
        if self.corrupt_line_count > 0 {
            parts.push(format!(
                "{} corrupt line{}",
                self.corrupt_line_count,
                if self.corrupt_line_count == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if self.ignored_partial_line {
            parts.push("a torn trailing line".to_string());
        }
        Some(format!("skipped {}", parts.join(" and ")))
    }
}

pub fn read(path: impl AsRef<Path>) -> Result<ReadReport> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ReadReport::default());
    }

    let mut file =
        File::open(path).with_context(|| format!("open agent event log {}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("read agent event log {}", path.display()))?;

    let ignored_partial_line = !text.is_empty() && !text.ends_with('\n');
    let mut lines = text.lines().collect::<Vec<_>>();
    if ignored_partial_line {
        lines.pop();
    }

    let mut records = Vec::new();
    let mut corrupt_line_count = 0;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match decode_record_tolerantly(line) {
            Some(record)
                if record.schema == AGENT_EVENT_LOG_SCHEMA
                    && record.version == AGENT_EVENT_LOG_VERSION =>
            {
                records.push(record);
            }
            _ => corrupt_line_count += 1,
        }
    }

    records.sort_by_key(|record| record.sequence);
    Ok(ReadReport {
        records,
        corrupt_line_count,
        ignored_partial_line,
    })
}

#[cfg(test)]
mod tolerant_read_tests {
    use super::*;
    use crate::contract::{Event, ProviderEvent, SessionState};
    use uuid::Uuid;

    /// A log line whose `event` names a variant this build doesn't know
    /// (with a payload — the case serde_json's `#[serde(other)]` cannot
    /// degrade by itself) must read back as a full record carrying
    /// `Event::Unknown`, envelope preserved — never as a corrupt line.
    #[test]
    fn a_future_event_variant_reads_back_as_an_unknown_record_with_its_envelope() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let parent_session_id = SessionId::new();
        let line = serde_json::json!({
            "schema": AGENT_EVENT_LOG_SCHEMA,
            "version": AGENT_EVENT_LOG_VERSION,
            "event_id": "evt-42",
            "sequence": 42,
            "session_id": session_id,
            "turn_id": "turn-1",
            "provider_id": "future.provider",
            "session_context": {
                "workspace_root": "/tmp/future-worktree",
                "isolated_worktree": true,
                "parent_session_id": parent_session_id,
            },
            "event_kind": "future_variant",
            "event": {"FutureVariant": {"x": 1}},
            "provider_payload": null,
            "created_at_unix_ms": 1,
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let report = read(&path).unwrap();
        assert_eq!(
            report.corrupt_line_count, 0,
            "an intact envelope must not count as corruption: {report:?}"
        );
        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.event, Event::Unknown);
        assert_eq!(record.sequence, 42, "the envelope must be preserved");
        assert_eq!(record.session_id, session_id);
        assert_eq!(record.event_kind, "future_variant");
        assert_eq!(
            record.session_context,
            Some(PersistedSessionContext {
                workspace_root: Some(PathBuf::from("/tmp/future-worktree")),
                isolated_worktree: true,
                parent_session_id: Some(parent_session_id),
            }),
            "tolerant event decoding must preserve host placement metadata"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The writer's `next_sequence` counts an unknown-event line: appends
    /// after it continue *past* its sequence instead of rewinding onto it
    /// (the duplicate-sequence hazard the intolerant read had).
    #[test]
    fn next_sequence_counts_an_unknown_event_line() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let line = serde_json::json!({
            "schema": AGENT_EVENT_LOG_SCHEMA,
            "version": AGENT_EVENT_LOG_VERSION,
            "event_id": "evt-7",
            "sequence": 7,
            "session_id": session_id,
            "turn_id": null,
            "provider_id": null,
            "event_kind": "future_variant",
            "event": {"FutureVariant": {"x": 1}},
            "provider_payload": null,
            "created_at_unix_ms": 1,
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let (writer, init_rx) = WriterHandle::open(&path);
        match init_rx.recv().unwrap() {
            WriterInit::Ready(report) => {
                assert_eq!(report.records.len(), 1, "the unknown line must be read");
                assert_eq!(
                    report.records[0].session_context, None,
                    "records predating session_context must keep decoding"
                );
            }
            WriterInit::Failed(error) => panic!("writer startup failed: {error}"),
        }
        let mut appender = Appender::new(writer.clone(), session_id, None, None);
        appender
            .append_provider_events(vec![ProviderEvent::from(Event::StateChanged(
                SessionState::Created,
            ))])
            .unwrap();
        writer.flush().unwrap();

        let report = read(&path).unwrap();
        let sequences: Vec<u64> = report.records.iter().map(|r| r.sequence).collect();
        assert_eq!(
            sequences,
            vec![7, 8],
            "the fresh append must continue past the unknown line's sequence"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A line whose *envelope* is broken still counts as corrupt — the
    /// tolerance is exactly one field deep.
    #[test]
    fn a_broken_envelope_still_counts_as_corrupt() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        std::fs::write(&path, "not valid json\n").unwrap();
        let report = read(&path).unwrap();
        assert_eq!(report.corrupt_line_count, 1);
        assert!(report.records.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        Event, Message, MessageDelta, MessageRole, ProviderEvent, ProviderRequestSent, SessionState,
    };
    use uuid::Uuid;

    #[test]
    fn writes_and_reads_jsonl_records() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_context = PersistedSessionContext {
            workspace_root: Some(PathBuf::from("/tmp/session-worktree")),
            isolated_worktree: true,
            parent_session_id: Some(SessionId::new()),
        };
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(ProviderId("test.provider".to_string())),
            None,
        )
        .with_session_context(session_context.clone());

        appender
            .append_provider_events(vec![ProviderEvent::with_provider_payload(
                Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "hello".to_string(),
                }),
                serde_json::json!({ "provider": true }),
            )])
            .expect("append");
        writer.flush().expect("flush");

        let report = read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].sequence, 0);
        assert_eq!(report.records[0].session_id, session_id);
        assert_eq!(report.records[0].event_kind, "message_committed");
        assert_eq!(
            report.records[0].provider_id,
            Some(ProviderId("test.provider".to_string()))
        );
        assert_eq!(
            report.records[0].provider_payload,
            Some(serde_json::json!({ "provider": true }))
        );
        assert_eq!(
            report.records[0].session_context,
            Some(session_context),
            "host-authored placement metadata must survive the JSONL round trip"
        );

        let _ = std::fs::remove_file(path);
    }

    /// Round-trips the provider-request lifecycle markers
    /// (`Event::ProviderRequestSent`/`ProviderRequestFirstToken`/
    /// `ProviderRequestFinished`) through the JSONL log: correct
    /// `event_kind` strings, the sent event's `model` field surviving
    /// serialization, and — since `TurnTracker` groups them like any other
    /// event — all three sharing the turn id opened by the preceding user
    /// message, so replay can attribute them to the turn they bracket.
    #[test]
    fn writes_and_reads_provider_request_lifecycle_events_with_shared_turn_id() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let (writer, _init_rx) = WriterHandle::open(&path);
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(ProviderId("builtin.agent.rig".to_string())),
            None,
        );

        appender
            .append_provider_events(vec![
                ProviderEvent::from(Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "hello".to_string(),
                })),
                ProviderEvent::from(Event::ProviderRequestSent(ProviderRequestSent {
                    model: "gpt-4o-mini".to_string(),
                })),
                ProviderEvent::from(Event::ProviderRequestFirstToken),
                ProviderEvent::from(Event::ProviderRequestFinished),
            ])
            .expect("append");
        writer.flush().expect("flush");

        let report = read(&path).expect("read");
        assert_eq!(report.records.len(), 4);

        let kinds: Vec<&str> = report
            .records
            .iter()
            .map(|record| record.event_kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message_committed",
                "provider_request_sent",
                "provider_request_first_token",
                "provider_request_finished",
            ]
        );
        assert_eq!(
            report.records[1].event,
            Event::ProviderRequestSent(ProviderRequestSent {
                model: "gpt-4o-mini".to_string(),
            })
        );

        let turn_id = report.records[0].turn_id.clone();
        assert!(turn_id.is_some(), "the user message must open a turn");
        assert!(
            report
                .records
                .iter()
                .all(|record| record.turn_id == turn_id),
            "provider request lifecycle markers must share the turn they bracket"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reader_skips_corrupt_and_partial_lines() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let record = Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: "event-1".to_string(),
            sequence: 0,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            session_context: None,
            event_kind: "state_changed".to_string(),
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            created_at_unix_ms: 1,
        };
        let valid_line = serde_json::to_string(&record).expect("serialize");
        std::fs::write(
            &path,
            format!("{valid_line}\nnot json\n{{\"schema\":\"horizon.agent.event_log\""),
        )
        .expect("write");

        let report = read(&path).expect("read");
        assert_eq!(report.records, vec![record]);
        assert_eq!(report.corrupt_line_count, 1);
        assert!(report.ignored_partial_line);

        let _ = std::fs::remove_file(path);
    }

    /// A record written before `role_id` existed has no such key in its
    /// JSON at all -- `#[serde(default)]` must still parse it (as `None`),
    /// not treat it as corrupt. Regression guard for resuming a log written
    /// by a pre-role build of `horizon-sessiond`.
    #[test]
    fn reads_a_pre_role_record_with_no_role_id_key() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let line = serde_json::json!({
            "schema": AGENT_EVENT_LOG_SCHEMA,
            "version": AGENT_EVENT_LOG_VERSION,
            "event_id": "event-pre-role",
            "sequence": 0,
            "session_id": session_id,
            "turn_id": null,
            "provider_id": null,
            "event_kind": "state_changed",
            "event": Event::StateChanged(SessionState::Running),
            "provider_payload": null,
            "created_at_unix_ms": 1,
        })
        .to_string();
        std::fs::write(&path, format!("{line}\n")).expect("write pre-role fixture");

        let report = read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].role_id, None);
        assert_eq!(report.corrupt_line_count, 0);

        let _ = std::fs::remove_file(path);
    }

    /// Fixture-style regression test for the real corruption this module was
    /// hardened against: a line torn in the *middle* of the file (an
    /// interleaved/truncated concurrent write, not just garbage text) and a
    /// torn *final* line (the app closing mid-write, no shutdown flush).
    /// `read` must skip both, keep the valid records either side of them,
    /// and report a skip count instead of failing the whole replay.
    #[test]
    fn read_reports_skip_counts_for_torn_middle_and_tail_lines() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let record_at = |sequence: u64, event_id: &str| Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: event_id.to_string(),
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
        };
        let first = record_at(0, "event-1");
        let second = record_at(1, "event-2");
        // A write that got interleaved with another writer mid-object: valid
        // JSON prefix, cut off before the closing brace, sitting between two
        // otherwise-valid lines.
        let torn_middle =
            "{\"schema\":\"horizon.agent.event_log\",\"version\":1,\"event_id\":\"torn-mid";
        // The final line of the file with no trailing newline, as if the
        // process was killed mid-write.
        let torn_tail =
            "{\"schema\":\"horizon.agent.event_log\",\"version\":1,\"event_id\":\"torn-tail\"";

        let contents = format!(
            "{}\n{}\n{}\n{}",
            serde_json::to_string(&first).expect("serialize first"),
            torn_middle,
            serde_json::to_string(&second).expect("serialize second"),
            torn_tail,
        );
        std::fs::write(&path, contents).expect("write fixture");

        let report = read(&path).expect("read");
        assert_eq!(report.records, vec![first, second]);
        assert_eq!(report.corrupt_line_count, 1);
        assert!(report.ignored_partial_line);
        assert_eq!(
            report.skipped_summary().as_deref(),
            Some("skipped 1 corrupt line and a torn trailing line")
        );

        let _ = std::fs::remove_file(path);
    }

    /// Models `horizon-sessiond`'s normal-exit shutdown path
    /// (`flush_event_log_before_exit`, called on `SessionControl::Drain`
    /// right before `std::process::exit(0)`): flush the writer before the
    /// process tears the background thread down, and confirm whatever was
    /// enqueued beforehand actually reached disk.
    #[test]
    fn flush_makes_pending_records_durable_before_shutdown() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let (writer, _init_rx) = WriterHandle::open(&path);
        let mut appender = Appender::new(writer.clone(), session_id, None, None);

        appender
            .append_provider_events(vec![ProviderEvent::from(Event::MessageCommitted(
                Message {
                    role: MessageRole::User,
                    text: "durable before shutdown".to_string(),
                },
            ))])
            .expect("append");

        // The shutdown signal: everything enqueued above must be on disk
        // once this returns, with no explicit `Drop` involved (the real
        // `WriterHandle` lives in a process-global static and is never
        // dropped during a normal run).
        writer.flush().expect("shutdown flush");

        let report = read(&path).expect("read after shutdown flush");
        assert_eq!(report.records.len(), 1);
        assert_eq!(
            report.records[0].event,
            Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "durable before shutdown".to_string(),
            })
        );

        let _ = std::fs::remove_file(path);
    }

    /// Proves the chosen design: a single process-global `WriterHandle`
    /// shared by every session hosted in a `horizon-sessiond` process (see
    /// the doc comment on `WriterHandle`) cannot tear lines no matter how
    /// many "sessions" hammer it concurrently, because all appends funnel
    /// through one channel to one thread with one open file.
    /// Payloads are sized well past the 4KiB `PIPE_BUF` figure cited in the
    /// real corruption report to exercise the same code path that tore
    /// lines when two independent writers raced on the same file.
    #[test]
    fn concurrent_appenders_share_one_writer_without_tearing() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let (writer, _init_rx) = WriterHandle::open(&path);

        let session_ids: Vec<SessionId> = (0..4).map(|_| SessionId::new()).collect();
        let events_per_session = 25_usize;
        let large_payload = "x".repeat(6_000);

        let handles: Vec<_> = session_ids
            .iter()
            .copied()
            .map(|session_id| {
                let writer = writer.clone();
                let large_payload = large_payload.clone();
                std::thread::spawn(move || {
                    let mut appender = Appender::new(
                        writer,
                        session_id,
                        Some(ProviderId("test.provider".to_string())),
                        None,
                    );
                    for index in 0..events_per_session {
                        appender
                            .append_provider_events(vec![ProviderEvent::from(
                                Event::AssistantTextDelta(MessageDelta {
                                    role: MessageRole::Assistant,
                                    text: format!("{large_payload}-{index}"),
                                }),
                            )])
                            .expect("append from concurrent session");
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("session writer thread panicked");
        }
        writer.flush().expect("flush");

        let report = read(&path).expect("read");
        assert_eq!(report.corrupt_line_count, 0);
        assert!(!report.ignored_partial_line);
        assert_eq!(report.records.len(), session_ids.len() * events_per_session);

        let mut sequences: Vec<u64> = report
            .records
            .iter()
            .map(|record| record.sequence)
            .collect();
        sequences.sort_unstable();
        sequences.dedup();
        assert_eq!(
            sequences.len(),
            report.records.len(),
            "every record must have a unique sequence number"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn turn_tracker_groups_events_until_waiting_state() {
        let mut tracker = TurnTracker::new();
        assert_eq!(
            tracker.turn_id_for_event(&Event::StateChanged(SessionState::Created)),
            None
        );

        let user_turn = tracker.turn_id_for_event(&Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: "question".to_string(),
        }));
        assert!(user_turn.is_some());

        assert_eq!(
            tracker.turn_id_for_event(&Event::ReasoningDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "thinking".to_string(),
            })),
            user_turn
        );
        assert_eq!(
            tracker.turn_id_for_event(&Event::StateChanged(SessionState::WaitingForUser)),
            user_turn
        );
        assert_eq!(
            tracker.turn_id_for_event(&Event::StateChanged(SessionState::Running)),
            None
        );
    }
}
