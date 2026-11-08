//! Incremental reads of the agent event log and the pending-delivery projection
//! the outbox works from.

use horizon_agent::contract::{Event, SessionId, SessionInput, SessionInputOutcome};
use horizon_agent::persistence::event_log::Record;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

pub(crate) struct Tail {
    path: PathBuf,
    offset: u64,
}
impl Tail {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }
    pub(crate) fn read(&mut self) -> Result<Vec<Record>, String> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        if file.metadata().map_err(|e| e.to_string())?.len() < self.offset {
            return Err("Agent log shrank while outbox delivery was running".into());
        }
        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        let mut offset = self.offset;
        loop {
            let mut line = String::new();
            let size = reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if size == 0 || !line.ends_with('\n') {
                break;
            }
            offset += size as u64;
            if let Ok(record) = serde_json::from_str(&line) {
                records.push(record);
            }
        }
        // Return the batch and advance together. A read error after earlier
        // complete records must not drop those records on the next retry.
        self.offset = offset;
        Ok(records)
    }
}

#[derive(Clone)]
pub(crate) enum Pending {
    Answer {
        sequence: u64,
        outcome: SessionInputOutcome,
        at: u64,
    },
    Send {
        sequence: u64,
        target: SessionId,
        input: SessionInput,
    },
}
impl Pending {
    fn sequence(&self) -> u64 {
        match self {
            Self::Answer { sequence, .. } | Self::Send { sequence, .. } => *sequence,
        }
    }
}
#[derive(Default)]
pub(crate) struct Index {
    pub(crate) sessions: HashMap<SessionId, Record>,
    pub(crate) projects: HashMap<SessionId, PathBuf>,
    pub(crate) accepted: HashSet<(SessionId, String)>,
    pub(crate) pending: HashMap<(SessionId, String), Pending>,
    acknowledged: HashSet<(SessionId, String)>,
}
impl Index {
    /// The writer's global sequence preserves source conversation order even
    /// when the in-memory index was rebuilt after restart.
    pub(crate) fn ordered_pending(&self) -> Vec<((SessionId, String), Pending)> {
        let mut pending: Vec<_> = self
            .pending
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        pending.sort_by(|(a_key, a), (b_key, b)| {
            a.sequence()
                .cmp(&b.sequence())
                .then_with(|| a_key.0.as_uuid().cmp(&b_key.0.as_uuid()))
                .then_with(|| a_key.1.cmp(&b_key.1))
        });
        pending
    }
    pub(crate) fn fold(&mut self, record: Record) {
        let source = record.session_id;
        match &record.event {
            Event::EnvironmentActivated(identity) => {
                self.projects.insert(source, identity.repository.clone());
            }
            Event::InputAccepted(input) => {
                self.accepted.insert((source, input.id.clone()));
            }
            Event::InputOutcome(outcome) => {
                let key = (source, outcome.delivery_id.clone());
                if !self.acknowledged.contains(&key) {
                    self.pending.entry(key).or_insert(Pending::Answer {
                        sequence: record.sequence,
                        outcome: outcome.clone(),
                        at: record.created_at_unix_ms,
                    });
                }
            }
            Event::SessionInputSent { session_id, input } => {
                let key = (source, input.id.clone());
                if !self.acknowledged.contains(&key) {
                    self.pending.entry(key).or_insert(Pending::Send {
                        sequence: record.sequence,
                        target: *session_id,
                        input: input.clone(),
                    });
                }
            }
            Event::DeliveryAcknowledged(id) => {
                let key = (source, id.clone());
                self.pending.remove(&key);
                self.acknowledged.insert(key);
            }
            _ => {}
        }
        self.sessions.insert(source, record);
    }
}
