use anyhow::{Context, Result};
use duckdb::params;
#[cfg(test)]
use duckdb::OptionalExt;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::contract::SessionId;
#[cfg(test)]
use crate::contract::{event_kind, Event, ProviderId};
use crate::persistence::event_log::{LogCursor, Record};

use super::{projection::EventRecordRef, session_id_text, Store};
#[cfg(test)]
use super::{AgentStoredEvent, AppendEvent};

impl Store {
    /// [`Self::append_record_uncommitted`], wrapped in its own transaction
    /// -- the entry point for the *live* per-event append path
    /// (`event_log::writer::run_writer`, one call per record right after
    /// its JSONL line is durably written). The transaction matters here for
    /// more than the batch case's speed: without it, a process killed
    /// between this method's `agent_events` insert and its
    /// `agent_sessions` upsert leaves the two tables inconsistent --
    /// `agent_events` durably has the row (DuckDB's own auto-commit already
    /// landed it) but `agent_sessions.last_sequence` doesn't yet reflect
    /// it. That specific inconsistency is fatal to incremental catch-up
    /// (`import::apply_records`'s other caller,
    /// [`Self::catch_up_from_event_log`]): it trusts the log cursor, written
    /// in this same transaction, to mean "everything up to this line is
    /// already fully present", so a cursor that disagreed with the rows
    /// would make the next catch-up insert the same `event_id` again and
    /// fail on `agent_events`'s primary key -- reproduced (against the
    /// sequence high-water mark that preceded the cursor) by
    /// `horizon-agentd`'s own e2e suite (`stale_log_triggers_duckdb_
    /// rebuild_on_respawn`) once a resumed session's own live thread
    /// appended a record while a hard `SIGKILL` landed nearby. Wrapping
    /// this one record's several statements and the cursor in a transaction
    /// makes them atomic: either every table sees it, or none does.
    pub(crate) fn append_record_at(
        &self,
        record: &Record,
        cursor: Option<&LogCursor>,
    ) -> Result<()> {
        self.append_records_atomic(std::slice::from_ref(record), cursor)
    }

    /// Test-only: [`Self::append_record_at`] without a log position.
    #[cfg(test)]
    pub(crate) fn append_record(&self, record: &Record) -> Result<()> {
        self.append_records_atomic(std::slice::from_ref(record), None)
    }

    /// Shared transaction boundary for live appends and rebuild chunks.
    /// Every event, session mark, derived row, and the log cursor (when
    /// given) commits together; the first failed projection rolls the whole
    /// batch back and preserves its error.
    pub(super) fn append_records_atomic(
        &self,
        records: &[Record],
        cursor: Option<&LogCursor>,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        let mut result = records
            .iter()
            .try_for_each(|record| self.append_record_uncommitted(record));
        if let (Ok(()), Some(cursor)) = (&result, cursor) {
            result = self.write_log_cursor(cursor);
        }
        if let Err(error) = result {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    fn write_log_cursor(&self, cursor: &LogCursor) -> Result<()> {
        let offset = i64::try_from(cursor.offset).context("agent event log offset overflow")?;
        let sequence = i64::try_from(cursor.sequence).context("agent event sequence overflow")?;
        self.conn.execute(
            "INSERT INTO agent_log_cursor (singleton, log_offset, sequence, event_id)
             VALUES (true, ?, ?, ?)
             ON CONFLICT (singleton) DO UPDATE SET
                log_offset = excluded.log_offset,
                sequence = excluded.sequence,
                event_id = excluded.event_id",
            params![offset, sequence, &cursor.event_id],
        )?;
        Ok(())
    }

    /// The actual insert/upsert/project body -- real `sequence`, and
    /// `event_at` carried from `record.created_at_unix_ms` via `epoch_ms(?)`
    /// (never a `DEFAULT now()`/`now()` insert-time stamp) -- into
    /// `agent_events`/`agent_sessions`, and projected into the
    /// transcript/tool/approval tables via [`Store::project_event`]. Not
    /// wrapped in its own transaction: [`Self::append_record`] (the live
    /// per-event path) wraps a single call to this in its own transaction;
    /// [`super::import::apply_records`] (the batch rebuild/catch-up path)
    /// calls this directly, many times, inside *one* transaction spanning
    /// the whole batch -- DuckDB has no nested-transaction support, so this
    /// inner body must stay transaction-free itself.
    ///
    /// `agent_events`/`agent_sessions` are always updated here regardless of
    /// what [`Store::project_event`] does with the event -- a projection
    /// that deliberately ignores an event (no dedicated table wants it) has
    /// still *processed* it, so the session's `last_sequence` high-water
    /// mark advances either way, and the log cursor committed with the
    /// record's transaction covers it like any other.
    pub(super) fn append_record_uncommitted(&self, record: &Record) -> Result<()> {
        let session_id_text = session_id_text(record.session_id)?;
        let sequence = i64::try_from(record.sequence).context("agent event sequence overflow")?;
        let event_json = serde_json::to_string(&record.event).context("serialize agent event")?;
        let provider_id_text = record.provider_id.as_ref().map(|id| id.0.clone());
        let role_id_text = record.role_id.as_ref().map(|id| id.0.clone());
        let provider_payload_json = record
            .provider_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize provider payload")?;
        let event_at_unix_ms =
            i64::try_from(record.created_at_unix_ms).context("event timestamp overflow")?;
        let session_context_json = record
            .session_context
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize session context")?;

        self.conn.execute(
            "INSERT INTO agent_events (
                event_id,
                session_id,
                turn_id,
                sequence,
                event_kind,
                horizon_event_json,
                provider_id,
                role_id,
                provider_payload_json,
                event_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, epoch_ms(?))",
            params![
                &record.event_id,
                &session_id_text,
                record.turn_id.as_deref(),
                sequence,
                &record.event_kind,
                &event_json,
                provider_id_text.as_deref(),
                role_id_text.as_deref(),
                provider_payload_json.as_deref(),
                event_at_unix_ms,
            ],
        )?;

        self.upsert_session(
            &session_id_text,
            provider_id_text.as_deref(),
            role_id_text.as_deref(),
            session_context_json.as_deref(),
            sequence,
        )?;
        self.project_event(EventRecordRef {
            event_id: &record.event_id,
            session_id: &session_id_text,
            turn_id: record.turn_id.as_deref(),
            sequence,
            event: &record.event,
        })
    }

    /// Test-only single-event append that models a single live append with
    /// no real-world `Record` to carry (unlike [`Self::append_record`], the
    /// runtime path): computes its own next sequence by querying the table
    /// and stamps `event_at = now()`, since there's no
    /// `created_at_unix_ms` to project here.
    #[cfg(test)]
    pub(crate) fn append_event(&self, record: AppendEvent) -> Result<AgentStoredEvent> {
        let session_id_text = session_id_text(record.session_id)?;
        let sequence = self.next_sequence(&session_id_text)?;
        let event_id = Uuid::new_v4().to_string();
        let event_kind = event_kind(&record.event).to_string();
        let event_json = serde_json::to_string(&record.event).context("serialize agent event")?;
        let provider_id_text = record.provider_id.as_ref().map(|id| id.0.clone());
        let role_id_text = record.role_id.as_ref().map(|id| id.0.clone());
        let provider_payload_json = record
            .provider_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize provider payload")?;

        self.conn.execute(
            "INSERT INTO agent_events (
                event_id,
                session_id,
                turn_id,
                sequence,
                event_kind,
                horizon_event_json,
                provider_id,
                role_id,
                provider_payload_json,
                event_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, now())",
            params![
                &event_id,
                &session_id_text,
                record.turn_id.as_deref(),
                sequence,
                &event_kind,
                &event_json,
                provider_id_text.as_deref(),
                role_id_text.as_deref(),
                provider_payload_json.as_deref(),
            ],
        )?;

        self.upsert_session(
            &session_id_text,
            provider_id_text.as_deref(),
            role_id_text.as_deref(),
            None,
            sequence,
        )?;
        self.project_event(EventRecordRef {
            event_id: &event_id,
            session_id: &session_id_text,
            turn_id: record.turn_id.as_deref(),
            sequence,
            event: &record.event,
        })?;

        Ok(AgentStoredEvent {
            event_id,
            session_id: record.session_id,
            turn_id: record.turn_id,
            sequence,
            event_kind,
            event: record.event,
            provider_id: record.provider_id,
            role_id: record.role_id,
            provider_payload: record.provider_payload,
        })
    }

    #[cfg(test)]
    pub(crate) fn append_events(
        &self,
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        events: impl IntoIterator<Item = Event>,
    ) -> Result<Vec<AgentStoredEvent>> {
        events
            .into_iter()
            .map(|event| {
                self.append_event(AppendEvent {
                    session_id,
                    turn_id: None,
                    provider_id: provider_id.clone(),
                    role_id: None,
                    event,
                    provider_payload: None,
                })
            })
            .collect()
    }

    #[cfg(test)]
    fn next_sequence(&self, session_id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0)
                 FROM agent_events
                 WHERE session_id = ?",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .context("query next agent event sequence")
    }

    pub(super) fn upsert_session(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        role_id: Option<&str>,
        session_context_json: Option<&str>,
        last_sequence: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_sessions (
                session_id, provider_id, role_id, session_context_json, last_sequence, updated_at
             )
             VALUES (?, ?, ?, ?, ?, now())
             ON CONFLICT (session_id) DO UPDATE SET
                provider_id = COALESCE(excluded.provider_id, agent_sessions.provider_id),
                role_id = COALESCE(excluded.role_id, agent_sessions.role_id),
                session_context_json = COALESCE(
                    excluded.session_context_json,
                    agent_sessions.session_context_json
                ),
                last_sequence = excluded.last_sequence,
                updated_at = now()",
            params![
                session_id,
                provider_id,
                role_id,
                session_context_json,
                last_sequence
            ],
        )?;
        Ok(())
    }
}
