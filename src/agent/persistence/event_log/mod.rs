use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    agent::contract::{Event, ProviderId},
    session::SessionId,
};

mod appender;
mod turn;
mod writer;

pub(crate) use appender::Appender;
use turn::TurnTracker;
pub(crate) use writer::{WriterHandle, WriterInit};

pub(crate) const AGENT_EVENT_LOG_SCHEMA: &str = "horizon.agent.event_log";
pub(crate) const AGENT_EVENT_LOG_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct Record {
    pub(crate) schema: String,
    pub(crate) version: u32,
    pub(crate) event_id: String,
    pub(crate) sequence: u64,
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: Option<String>,
    pub(crate) provider_id: Option<ProviderId>,
    pub(crate) event_kind: String,
    pub(crate) event: Event,
    pub(crate) provider_payload: Option<serde_json::Value>,
    pub(crate) created_at_unix_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReadReport {
    pub(crate) records: Vec<Record>,
    pub(crate) corrupt_line_count: usize,
    pub(crate) ignored_partial_line: bool,
}

impl ReadReport {
    /// A short human-readable summary of lines `read` had to skip, or
    /// `None` when the file parsed cleanly. Every consumer of the raw JSONL
    /// (the writer's own startup re-read in `event_log::writer`, the DuckDB
    /// replay in `app::runtime::agent`) reports this instead of silently
    /// discarding evidence that the file has corrupt or torn lines.
    pub(crate) fn skipped_summary(&self) -> Option<String> {
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

pub(crate) fn read(path: impl AsRef<Path>) -> Result<ReadReport> {
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
        match serde_json::from_str::<Record>(line) {
            Ok(record)
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
mod tests {
    use super::*;
    use crate::agent::contract::{
        Event, Message, MessageDelta, MessageRole, ProviderEvent, SessionState,
    };
    use uuid::Uuid;

    #[test]
    fn writes_and_reads_jsonl_records() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let (writer, _init_rx) = WriterHandle::open(&path);
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(ProviderId("test.provider".to_string())),
        );

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

    /// Models the app's normal-exit shutdown path (`app::shutdown`, wired to
    /// floem's `AppEvent::WillTerminate` in `main.rs`): flush the writer
    /// before the process tears the background thread down, and confirm
    /// whatever was enqueued beforehand actually reached disk.
    #[test]
    fn flush_makes_pending_records_durable_before_shutdown() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let (writer, _init_rx) = WriterHandle::open(&path);
        let mut appender = Appender::new(writer.clone(), session_id, None);

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
    /// shared by every session (see the doc comment on `WriterHandle` and on
    /// `AGENT_EVENT_LOG_WRITER` in `app::runtime::agent`) cannot tear lines
    /// no matter how many "sessions" hammer it concurrently, because all
    /// appends funnel through one channel to one thread with one open file.
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
