//! Incremental reads of the agent event log and the pending-delivery projection
//! the outbox works from.

use horizon_agent::contract::{Event, SessionId, SessionInput, SessionInputOutcome};
use horizon_agent::persistence::event_log::{Record, session_ids, session_path};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Where the outbox has read each session log in a directory to.
pub struct Tail {
    dir: PathBuf,
    offsets: HashMap<SessionId, u64>,
}
impl Tail {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            offsets: HashMap::new(),
        }
    }

    /// The complete records written to every session log since the last
    /// read, ordered by time, then session, then sequence.
    pub fn read(&mut self) -> Result<Vec<Record>, String> {
        let mut records = Vec::new();
        for session_id in session_ids(&self.dir).map_err(|e| format!("{e:#}"))? {
            let offset = self.offsets.entry(session_id).or_insert(0);
            read_file(&session_path(&self.dir, session_id), offset, &mut records)?;
        }
        records.sort_by(|a: &Record, b: &Record| {
            a.created_at_unix_ms
                .cmp(&b.created_at_unix_ms)
                .then_with(|| a.session_id.as_uuid().cmp(&b.session_id.as_uuid()))
                .then_with(|| a.sequence.cmp(&b.sequence))
        });
        Ok(records)
    }
}

/// Appends the complete lines of `path` after `offset` to `records` and
/// advances `offset` past them.
fn read_file(path: &Path, offset: &mut u64, records: &mut Vec<Record>) -> Result<(), String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < *offset {
        return Err(format!(
            "Agent log {} shrank while outbox delivery was running",
            path.display()
        ));
    }
    if len == *offset {
        return Ok(());
    }
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(*offset))
        .map_err(|e| e.to_string())?;
    // The batch and the offset advance together: a read error after earlier
    // complete records leaves both, so the next read returns those records.
    let mut read = Vec::new();
    let mut end = *offset;
    loop {
        let mut line = String::new();
        let size = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if size == 0 || !line.ends_with('\n') {
            break;
        }
        end += size as u64;
        if let Ok(record) = serde_json::from_str(&line) {
            read.push(record);
        }
    }
    *offset = end;
    records.append(&mut read);
    Ok(())
}

#[derive(Clone)]
pub enum Pending {
    Answer {
        sequence: u64,
        outcome: SessionInputOutcome,
        at: u64,
    },
    Send {
        sequence: u64,
        target: SessionId,
        input: SessionInput,
        at: u64,
    },
}
impl Pending {
    /// When it was written, then its place in its session.
    fn order(&self) -> (u64, u64) {
        match self {
            Self::Answer { sequence, at, .. } | Self::Send { sequence, at, .. } => (*at, *sequence),
        }
    }
}
#[derive(Default)]
pub struct Index {
    pub sessions: HashMap<SessionId, Record>,
    pub projects: HashMap<SessionId, PathBuf>,
    pub accepted: HashSet<(SessionId, String)>,
    pub pending: HashMap<(SessionId, String), Pending>,
    acknowledged: HashSet<(SessionId, String)>,
}
impl Index {
    /// In the order they were written: by time across sessions, by sequence
    /// within one, the same after the index is rebuilt at a restart.
    pub fn ordered_pending(&self) -> Vec<((SessionId, String), Pending)> {
        let mut pending: Vec<_> = self
            .pending
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        pending.sort_by(|(a_key, a), (b_key, b)| {
            a.order()
                .0
                .cmp(&b.order().0)
                .then_with(|| a_key.0.as_uuid().cmp(&b_key.0.as_uuid()))
                .then_with(|| a.order().1.cmp(&b.order().1))
                .then_with(|| a_key.1.cmp(&b_key.1))
        });
        pending
    }
    pub fn fold(&mut self, record: Record) {
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
                        at: record.created_at_unix_ms,
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
