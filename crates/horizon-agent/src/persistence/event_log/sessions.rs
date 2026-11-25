//! The event log as one file per session: `<session id>.jsonl` in one
//! directory, written only by the process that hosts the session
//! (`docs/agent-stdio-child-design.md`, "One log file per session").
//! A record's `sequence` counts within its session; what orders records of
//! different sessions is their time ([`read_all`]).

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::contract::{Event, ProviderId, SessionId, SessionState};
use crate::roles::RoleId;

use super::{Log, PersistedSessionContext, ReadReport, Record, RecordEnvelope, read};

/// The file a session's records are written to.
pub fn session_path(dir: &Path, session_id: SessionId) -> PathBuf {
    dir.join(format!("{}.jsonl", session_id.as_uuid()))
}

/// Every session with a file in `dir`, in no particular order.
pub fn session_ids(dir: &Path) -> Result<Vec<SessionId>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("list agent sessions in {}", dir.display()));
        }
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("list agent sessions in {}", dir.display()))?;
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".jsonl")) else {
            continue;
        };
        if let Ok(uuid) = uuid::Uuid::parse_str(stem) {
            sessions.push(SessionId::from_uuid(uuid));
        }
    }
    Ok(sessions)
}

/// One session's own records, oldest first.
pub fn read_session(dir: &Path, session_id: SessionId) -> Result<ReadReport> {
    read(session_path(dir, session_id))
}

/// Every session's records, ordered by time, then session, then sequence.
/// Counts are summed over the files; offsets are not kept, since they
/// belong to one file each.
pub fn read_all(dir: &Path) -> Result<ReadReport> {
    let mut all = ReadReport::default();
    for session_id in session_ids(dir)? {
        let report = read_session(dir, session_id)?;
        all.corrupt_line_count += report.corrupt_line_count;
        all.skipped_event_count += report.skipped_event_count;
        all.ignored_partial_line |= report.ignored_partial_line;
        all.records.extend(report.records);
    }
    all.records.sort_by(|a, b| {
        a.created_at_unix_ms
            .cmp(&b.created_at_unix_ms)
            .then_with(|| a.session_id.as_uuid().cmp(&b.session_id.as_uuid()))
            .then_with(|| a.sequence.cmp(&b.sequence))
    });
    Ok(all)
}

/// A session's branch ([`super::session_branch`]): its own records, and
/// for a fork, the records of the sessions it was forked from up to the
/// point it was forked at. Empty for a session with no file.
pub fn read_branch(dir: &Path, session_id: SessionId) -> Result<Vec<Record>> {
    let mut records = read_session(dir, session_id)?.records;
    let Some(last) = records.last() else {
        return Ok(Vec::new());
    };
    let last = last.event_id.clone();
    let mut read_sessions = HashSet::from([session_id]);
    let mut next = fork_source(&records, session_id);
    while let Some(source) = next.take() {
        let source = match source {
            ForkSource::Named(source) if !read_sessions.contains(&source) => source,
            // The fork names no source, or a source read already: the
            // parent can only be found by looking through every file.
            _ => {
                let mut all = read_all(dir)?.records;
                all.retain(|record| !read_sessions.contains(&record.session_id));
                records.append(&mut all);
                break;
            }
        };
        read_sessions.insert(source);
        let own = read_session(dir, source)?.records;
        next = fork_source(&own, source);
        records.extend(own);
    }
    let log = Log::new(records);
    Ok(log.branch_to(&last))
}

/// Where a fork's history continues, judged from its own first record.
enum ForkSource {
    /// The session its context says it was spawned from.
    Named(SessionId),
    /// A fork whose context names no source.
    Unnamed,
}

/// `None` when the session's first record's parent is its own (or it has
/// none): the session is not a fork.
fn fork_source(own: &[Record], session_id: SessionId) -> Option<ForkSource> {
    let first = own.first()?;
    let parent = first.parent_id.as_deref()?;
    if own.iter().any(|record| record.event_id == parent) {
        return None;
    }
    Some(
        match first
            .session_context
            .as_ref()
            .and_then(|context| context.spawn_source_session_id)
            .filter(|source| *source != session_id)
        {
            Some(source) => ForkSource::Named(source),
            None => ForkSource::Unnamed,
        },
    )
}

/// What a session's latest records say about it, read from the end of its
/// file.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    /// Its latest state-bearing event (`StateChanged` or `Exited`) ended
    /// it: `Terminated`, `Suspended`, or `Exited`.
    pub dead: bool,
    pub provider_id: Option<ProviderId>,
    pub role_id: Option<RoleId>,
    pub session_context: Option<PersistedSessionContext>,
    /// When its latest record was written.
    pub last_at_unix_ms: u64,
}

/// The summary of the session whose file is at `path`, read backwards only
/// as far as its latest state-bearing event and its latest record that
/// carries a session context (the provider, role and context are taken
/// from that one: the judge's verdicts carry none). `None` for a file with
/// no record.
pub fn session_summary(path: &Path) -> Result<Option<SessionSummary>> {
    let mut lines = BackwardLines::open(path)?;
    let mut summary: Option<SessionSummary> = None;
    let mut state_seen = false;
    let mut context_seen = false;
    while let Some(line) = lines.next_line()? {
        let Ok(record) = serde_json::from_slice::<Record>(&line) else {
            continue;
        };
        let summary = summary.get_or_insert(SessionSummary {
            session_id: record.session_id,
            dead: false,
            provider_id: None,
            role_id: None,
            session_context: None,
            last_at_unix_ms: record.created_at_unix_ms,
        });
        if !state_seen {
            match &record.event {
                Event::StateChanged(state) => {
                    state_seen = true;
                    summary.dead =
                        matches!(state, SessionState::Terminated | SessionState::Suspended);
                }
                Event::Exited(_) => {
                    state_seen = true;
                    summary.dead = true;
                }
                _ => {}
            }
        }
        if !context_seen && record.session_context.is_some() {
            context_seen = true;
            summary.provider_id = record.provider_id;
            summary.role_id = record.role_id;
            summary.session_context = record.session_context;
        }
        if state_seen && context_seen {
            break;
        }
    }
    Ok(summary)
}

/// The highest sequence among the complete lines at the end of the file at
/// `path`, read backwards to the first line whose envelope decodes.
pub(crate) fn last_sequence(path: &Path) -> Result<Option<u64>> {
    let mut lines = BackwardLines::open(path)?;
    while let Some(line) = lines.next_line()? {
        if let Ok(envelope) = serde_json::from_slice::<RecordEnvelope>(&line) {
            return Ok(Some(envelope.sequence));
        }
    }
    Ok(None)
}

/// A file's complete lines, last first; an unterminated tail is skipped.
struct BackwardLines {
    file: Option<File>,
    /// Where the bytes not read yet end.
    end: u64,
    /// Bytes read but not yet split into lines: the start of the file's
    /// unread part's last line.
    pending: Vec<u8>,
    started: bool,
}

impl BackwardLines {
    const CHUNK: u64 = 64 * 1024;

    fn open(path: &Path) -> Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    file: None,
                    end: 0,
                    pending: Vec::new(),
                    started: true,
                });
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open agent event log {}", path.display()));
            }
        };
        let end = file
            .metadata()
            .with_context(|| format!("stat agent event log {}", path.display()))?
            .len();
        Ok(Self {
            file: Some(file),
            end,
            pending: Vec::new(),
            started: false,
        })
    }

    /// Reads the chunk before what has been read.
    fn read_chunk(&mut self) -> Result<bool> {
        let Some(file) = self.file.as_mut() else {
            return Ok(false);
        };
        if self.end == 0 {
            return Ok(false);
        }
        let start = self.end.saturating_sub(Self::CHUNK);
        let mut chunk = vec![0u8; (self.end - start) as usize];
        file.seek(SeekFrom::Start(start))
            .context("seek agent event log")?;
        file.read_exact(&mut chunk)
            .context("read agent event log")?;
        chunk.append(&mut self.pending);
        self.pending = chunk;
        self.end = start;
        Ok(true)
    }

    fn next_line(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.started {
            self.started = true;
            // Drop an unterminated tail: it was never committed.
            loop {
                if let Some(index) = self.pending.iter().rposition(|byte| *byte == b'\n') {
                    self.pending.truncate(index + 1);
                    break;
                }
                if !self.read_chunk()? {
                    self.pending.clear();
                    return Ok(None);
                }
            }
        }
        loop {
            // `pending` ends with the newline of the line to return next.
            if self.pending.last() == Some(&b'\n') {
                self.pending.pop();
            }
            if let Some(index) = self.pending.iter().rposition(|byte| *byte == b'\n') {
                let line = self.pending.split_off(index + 1);
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Ok(Some(line));
            }
            if !self.read_chunk()? {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.pending);
                if line.iter().all(u8::is_ascii_whitespace) {
                    return Ok(None);
                }
                return Ok(Some(line));
            }
        }
    }
}

#[cfg(test)]
mod tests;
