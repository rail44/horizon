use anyhow::Result;

use crate::persistence::event_log::Record;

use super::{schema::CLEAR_ALL_AGENT_STATE_SQL, Store};

/// Outcome of a batched [`Store::append_record`] pass ([`Store::
/// replace_from_event_log_records`]'s full rebuild, or [`Store::
/// catch_up_from_event_log_records`]'s incremental tail) -- lets the caller
/// (`event_log::writer::rebuild_and_open_duckdb_projection`) print one
/// summary line instead of the per-record noise the individual pieces used
/// to print themselves. See [`super::projection::Store::project_event`]'s
/// doc comment for what `turn_id_missing` counts.
pub(crate) struct ApplyRecordsReport {
    pub applied: usize,
    pub turn_id_missing: usize,
}

impl Store {
    /// Full rebuild: clears every durable/derived agent table and reinserts
    /// `records` from scratch. Used when there is no existing high-water
    /// mark to catch up from (an empty store), the mark is ahead of the
    /// log's own tail (a signal something is wrong, not just behind), or a
    /// schema migration just invalidated the existing projection's rows --
    /// see `event_log::writer::rebuild_and_open_duckdb_projection`.
    pub(crate) fn replace_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<ApplyRecordsReport> {
        self.clear_all_agent_state()?;
        self.apply_records(records)
    }

    /// Incremental catch-up: appends `records` -- expected to already be
    /// filtered to those beyond the projection's existing high-water mark
    /// (see [`Store::max_last_sequence`]) -- without clearing any existing
    /// state first. Used when the mark is merely behind the log's tail, the
    /// common case on every restart after the first: projecting just the
    /// tail is what makes a restart against a large real corpus cheap
    /// instead of re-doing the whole history every time.
    pub(crate) fn catch_up_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<ApplyRecordsReport> {
        self.apply_records(records)
    }

    /// Appends every record in `records`, in order, inside one DuckDB
    /// transaction -- via [`Store::append_record_uncommitted`], not
    /// [`Store::append_record`] itself, since DuckDB has no
    /// nested-transaction support and this method supplies the one
    /// transaction the whole batch runs inside (see that method's doc
    /// comment). The transaction is load-bearing for more than atomicity:
    /// each record's append issues several individual statements (an
    /// `agent_events` insert, an `agent_sessions` upsert, and a
    /// projection-table insert), and without an explicit transaction each
    /// of those auto-commits -- and fsyncs -- on its own. Measured against a
    /// real ~16k-record archived event log, that made a full rebuild take
    /// minutes rather than seconds, which in practice meant `horizon-sessiond`
    /// was routinely restarted before a rebuild ever reached a durable,
    /// checkpointed state -- so the next boot's freshness check
    /// (`event_log::writer::duckdb_projection_currency`) never found a
    /// matching mark and rebuilt again, every single time. Wrapping the loop
    /// in one transaction turns that into a handful of seconds.
    fn apply_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<ApplyRecordsReport> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        let mut applied = 0usize;
        let mut turn_id_missing = 0usize;
        for record in records {
            match self.append_record_uncommitted(&record) {
                Ok(true) => {
                    applied += 1;
                    turn_id_missing += 1;
                }
                Ok(false) => applied += 1,
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(ApplyRecordsReport {
            applied,
            turn_id_missing,
        })
    }

    fn clear_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(CLEAR_ALL_AGENT_STATE_SQL)?;
        Ok(())
    }
}
