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
pub(crate) use writer::WriterHandle;

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
        let writer = WriterHandle::open(&path).expect("writer");
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
        writer.flush_for_tests().expect("flush");

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
