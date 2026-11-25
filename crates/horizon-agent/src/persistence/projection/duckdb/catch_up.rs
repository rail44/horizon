//! Bringing the projection level with the session logs: each file is read
//! from where the projection last stopped in it (its cursor), and a
//! projection stamped with another format is rebuilt from every file.

use std::path::Path;

use anyhow::Result;

use crate::contract::SessionId;
use crate::persistence::event_log::{
    AGENT_EVENT_LOG_VERSION, cursor_line_matches, read, read_from, session_ids, session_path,
};

use super::{
    Store,
    import::{ApplyRecordsReport, positioned},
    schema::PROJECTION_SCHEMA_VERSION,
};

impl Store {
    /// Catches the projection up with every session log in `dir`. A
    /// projection stamped with another format, or a file that no longer
    /// holds the line its cursor names (a file replaced underneath it), is
    /// rebuilt from every file.
    pub(crate) fn catch_up_from_sessions(&self, dir: &Path) -> Result<ApplyRecordsReport> {
        if !self.has_current_format().unwrap_or(false) {
            return self.rebuild_from_sessions(dir);
        }
        let mut report = ApplyRecordsReport::default();
        for session_id in session_ids(dir)? {
            let path = session_path(dir, session_id);
            let tail = match self.log_cursor(session_id)? {
                Some(cursor) if cursor_line_matches(&path, &cursor)? => {
                    read_from(&path, cursor.offset)?
                }
                Some(_) => return self.rebuild_from_sessions(dir),
                None => read(&path)?,
            };
            report.absorb(self.apply_records(positioned(tail))?);
        }
        Ok(report)
    }

    /// Drops everything and projects every session log in `dir`, stamping
    /// the current format once nothing was skipped.
    pub(crate) fn rebuild_from_sessions(&self, dir: &Path) -> Result<ApplyRecordsReport> {
        self.drop_all_agent_state()?;
        let mut report = ApplyRecordsReport::default();
        let mut sessions: Vec<SessionId> = session_ids(dir)?;
        sessions.sort_by_key(|session| session.as_uuid());
        for session_id in sessions {
            let file = read(session_path(dir, session_id))?;
            report.absorb(self.apply_records(positioned(file))?);
        }
        if report.skipped == 0 {
            self.conn.execute(
                "INSERT INTO agent_projection_format VALUES (true, ?, ?)",
                [AGENT_EVENT_LOG_VERSION, PROJECTION_SCHEMA_VERSION],
            )?;
        }
        Ok(report)
    }
}

impl ApplyRecordsReport {
    fn absorb(&mut self, other: ApplyRecordsReport) {
        self.applied += other.applied;
        self.skipped += other.skipped;
        if self.first_skip_error.is_none() {
            self.first_skip_error = other.first_skip_error;
        }
    }
}

#[cfg(test)]
mod tests;
