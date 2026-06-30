use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    agent::{
        agent_event_kind, AgentEvent, AgentMessageRole, AgentProviderEvent, AgentProviderId,
        AgentSessionState,
    },
    workspace::SessionId,
};

pub const AGENT_EVENT_LOG_SCHEMA: &str = "horizon.agent.event_log";
pub const AGENT_EVENT_LOG_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentEventLogRecord {
    pub schema: String,
    pub version: u32,
    pub event_id: String,
    pub sequence: u64,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<AgentProviderId>,
    pub event_kind: String,
    pub event: AgentEvent,
    pub provider_payload: Option<serde_json::Value>,
    pub created_at_unix_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentEventLogReadReport {
    pub records: Vec<AgentEventLogRecord>,
    pub corrupt_line_count: usize,
    pub ignored_partial_line: bool,
}

#[derive(Clone)]
pub struct AgentEventLogWriterHandle {
    tx: Sender<AgentEventLogWriterCommand>,
    next_sequence: Arc<Mutex<u64>>,
}

impl AgentEventLogWriterHandle {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("create agent event log directory {}", parent.display())
                })?;
            }
        }

        let next_sequence = read_agent_event_log(path)?
            .records
            .iter()
            .map(|record| record.sequence)
            .max()
            .map(|sequence| sequence + 1)
            .unwrap_or(0);
        let (tx, rx) = unbounded();
        let writer_path = path.to_path_buf();
        thread::spawn(move || run_writer(writer_path, rx));

        Ok(Self {
            tx,
            next_sequence: Arc::new(Mutex::new(next_sequence)),
        })
    }

    pub fn append(&self, mut record: AgentEventLogRecord) -> Result<()> {
        {
            let mut next_sequence = self
                .next_sequence
                .lock()
                .map_err(|_| anyhow::anyhow!("agent event log sequence lock poisoned"))?;
            record.sequence = *next_sequence;
            *next_sequence += 1;
        }
        self.tx
            .send(AgentEventLogWriterCommand::Append(record))
            .context("enqueue agent event log record")
    }

    #[cfg(test)]
    pub fn flush_for_tests(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(AgentEventLogWriterCommand::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }
}

enum AgentEventLogWriterCommand {
    Append(AgentEventLogRecord),
    #[cfg(test)]
    Flush(Sender<Result<()>>),
}

fn run_writer(path: PathBuf, rx: Receiver<AgentEventLogWriterCommand>) {
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let mut writer = BufWriter::new(file);

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(record) => {
                if serde_json::to_writer(&mut writer, &record).is_ok() {
                    let _ = writer.write_all(b"\n");
                }
            }
            #[cfg(test)]
            AgentEventLogWriterCommand::Flush(reply) => {
                let result = writer
                    .flush()
                    .with_context(|| format!("flush agent event log {}", path.display()));
                let _ = reply.send(result);
            }
        }
    }

    let _ = writer.flush();
}

pub fn read_agent_event_log(path: impl AsRef<Path>) -> Result<AgentEventLogReadReport> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(AgentEventLogReadReport::default());
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
        match serde_json::from_str::<AgentEventLogRecord>(line) {
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
    Ok(AgentEventLogReadReport {
        records,
        corrupt_line_count,
        ignored_partial_line,
    })
}

pub struct AgentEventLogAppender {
    writer: AgentEventLogWriterHandle,
    session_id: SessionId,
    provider_id: Option<AgentProviderId>,
    turn_tracker: AgentTurnTracker,
}

impl AgentEventLogAppender {
    pub fn new(
        writer: AgentEventLogWriterHandle,
        session_id: SessionId,
        provider_id: Option<AgentProviderId>,
    ) -> Self {
        Self {
            writer,
            session_id,
            provider_id,
            turn_tracker: AgentTurnTracker::new(),
        }
    }

    pub fn append_provider_events(&mut self, events: Vec<AgentProviderEvent>) -> Result<()> {
        for event in events {
            let turn_id = self.turn_tracker.turn_id_for_event(&event.event);
            let record = AgentEventLogRecord {
                schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: AGENT_EVENT_LOG_VERSION,
                event_id: Uuid::new_v4().to_string(),
                sequence: 0,
                session_id: self.session_id,
                turn_id,
                provider_id: self.provider_id.clone(),
                event_kind: agent_event_kind(&event.event).to_string(),
                event: event.event,
                provider_payload: event.provider_payload,
                created_at_unix_ms: unix_time_ms(),
            };
            self.writer.append(record)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentTurnTracker {
    current_turn_id: Option<String>,
}

impl AgentTurnTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn turn_id_for_event(&mut self, event: &AgentEvent) -> Option<String> {
        if matches!(
            event,
            AgentEvent::MessageCommitted(message) if message.role == AgentMessageRole::User
        ) {
            self.current_turn_id = Some(Uuid::new_v4().to_string());
        }

        let turn_id = self.current_turn_id.clone();

        if matches!(
            event,
            AgentEvent::StateChanged(
                AgentSessionState::WaitingForUser
                    | AgentSessionState::WaitingForApproval
                    | AgentSessionState::Failed
                    | AgentSessionState::Terminated
            )
        ) {
            self.current_turn_id = None;
        }

        turn_id
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentMessage, AgentMessageDelta, AgentSessionState};

    #[test]
    fn writes_and_reads_jsonl_records() {
        let path = std::env::temp_dir().join(format!("horizon-agent-log-{}.jsonl", Uuid::new_v4()));
        let session_id = SessionId::new();
        let writer = AgentEventLogWriterHandle::open(&path).expect("writer");
        let mut appender = AgentEventLogAppender::new(
            writer.clone(),
            session_id,
            Some(AgentProviderId("test.provider".to_string())),
        );

        appender
            .append_provider_events(vec![AgentProviderEvent::with_provider_payload(
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "hello".to_string(),
                }),
                serde_json::json!({ "provider": true }),
            )])
            .expect("append");
        writer.flush_for_tests().expect("flush");

        let report = read_agent_event_log(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].sequence, 0);
        assert_eq!(report.records[0].session_id, session_id);
        assert_eq!(report.records[0].event_kind, "message_committed");
        assert_eq!(
            report.records[0].provider_id,
            Some(AgentProviderId("test.provider".to_string()))
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
        let record = AgentEventLogRecord {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: "event-1".to_string(),
            sequence: 0,
            session_id,
            turn_id: None,
            provider_id: None,
            event_kind: "state_changed".to_string(),
            event: AgentEvent::StateChanged(AgentSessionState::Running),
            provider_payload: None,
            created_at_unix_ms: 1,
        };
        let valid_line = serde_json::to_string(&record).expect("serialize");
        std::fs::write(
            &path,
            format!("{valid_line}\nnot json\n{{\"schema\":\"horizon.agent.event_log\""),
        )
        .expect("write");

        let report = read_agent_event_log(&path).expect("read");
        assert_eq!(report.records, vec![record]);
        assert_eq!(report.corrupt_line_count, 1);
        assert!(report.ignored_partial_line);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn turn_tracker_groups_events_until_waiting_state() {
        let mut tracker = AgentTurnTracker::new();
        assert_eq!(
            tracker.turn_id_for_event(&AgentEvent::StateChanged(AgentSessionState::Created)),
            None
        );

        let user_turn = tracker.turn_id_for_event(&AgentEvent::MessageCommitted(AgentMessage {
            role: AgentMessageRole::User,
            text: "question".to_string(),
        }));
        assert!(user_turn.is_some());

        assert_eq!(
            tracker.turn_id_for_event(&AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "thinking".to_string(),
            })),
            user_turn
        );
        assert_eq!(
            tracker.turn_id_for_event(&AgentEvent::StateChanged(AgentSessionState::WaitingForUser)),
            user_turn
        );
        assert_eq!(
            tracker.turn_id_for_event(&AgentEvent::StateChanged(AgentSessionState::Running)),
            None
        );
    }
}
