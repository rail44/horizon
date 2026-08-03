use anyhow::Result;

use crate::persistence::event_log::Record;

use super::{schema::CLEAR_ALL_AGENT_STATE_SQL, Store};

/// How many records one [`Store::apply_records`] transaction covers.
///
/// The whole batch used to run inside a single transaction. It no longer
/// can: DuckDB aborts an explicit transaction on the *first* failing
/// statement ("Current transaction is aborted (please ROLLBACK)"), and its
/// `COMMIT` then silently discards everything, so one unprojectable record
/// anywhere in a real log threw away the entire rebuild. Chunking bounds
/// what a failure costs -- a failed chunk is rolled back and retried in
/// smaller pieces, so only the genuinely bad record is skipped (see
/// [`Store::apply_chunk`]).
///
/// The size is a throughput/blast-radius trade: a transaction per record
/// makes a rebuild take minutes rather than seconds (see
/// [`Store::apply_records`]'s doc comment for the fsync-per-statement
/// measurement that motivated batching in the first place). This many
/// records per commit keeps a real ~121k-record log at ~120 commits, which
/// measured within ~5% of the previous one-transaction-for-everything
/// shape (23.1s vs 24.4s over that log's first 10k records, debug build)
/// -- the per-record projection work dominates either way.
const APPLY_CHUNK_RECORDS: usize = 1024;

/// Outcome of a batched [`Store::append_record`] pass ([`Store::
/// replace_from_event_log_records`]'s full rebuild, or [`Store::
/// catch_up_from_event_log_records`]'s incremental tail) -- lets the caller
/// (`event_log::writer::rebuild_and_open_duckdb_projection`) print one
/// summary line instead of the per-record noise the individual pieces used
/// to print themselves.
#[derive(Default)]
pub(crate) struct ApplyRecordsReport {
    /// Records that actually landed in the projection.
    pub applied: usize,
    /// Records that failed to project even on their own and were skipped
    /// (see [`Store::apply_chunk`]); `first_skip_error` carries the first
    /// one's message so the caller's one-line summary can name a cause.
    pub skipped: usize,
    pub first_skip_error: Option<String>,
}

impl ApplyRecordsReport {
    fn record_skip(&mut self, error: &anyhow::Error) {
        self.skipped += 1;
        if self.first_skip_error.is_none() {
            self.first_skip_error = Some(format!("{error}"));
        }
    }
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

    /// Appends every record in `records`, in order, in
    /// [`APPLY_CHUNK_RECORDS`]-sized DuckDB transactions -- via
    /// [`Store::append_record_uncommitted`], not [`Store::append_record`]
    /// itself, since DuckDB has no nested-transaction support and this
    /// method supplies the transaction each chunk runs inside (see that
    /// method's doc comment). The transaction is load-bearing for more
    /// than atomicity: each record's append issues several individual
    /// statements (an `agent_events` insert, an `agent_sessions` upsert,
    /// and a projection-table insert), and without an explicit transaction
    /// each of those auto-commits -- and fsyncs -- on its own. Measured
    /// against a real ~16k-record archived event log, that made a full
    /// rebuild take minutes rather than seconds, which in practice meant
    /// `horizon-agentd` was routinely restarted before a rebuild ever
    /// reached a durable, checkpointed state -- so the next boot's
    /// freshness check (`event_log::writer::duckdb_projection_currency`)
    /// never found a matching mark and rebuilt again, every single time.
    /// Batching the loop into transactions turns that into a handful of
    /// seconds.
    ///
    /// Never fails on a single bad record: a chunk whose transaction errors
    /// is retried in smaller pieces so only the offending record is skipped
    /// (counted in [`ApplyRecordsReport::skipped`], reported once by the
    /// caller) -- see [`Self::apply_chunk`].
    fn apply_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<ApplyRecordsReport> {
        let mut report = ApplyRecordsReport::default();
        let mut chunk = Vec::with_capacity(APPLY_CHUNK_RECORDS);
        for record in records {
            chunk.push(record);
            if chunk.len() == APPLY_CHUNK_RECORDS {
                self.apply_chunk(&chunk, &mut report);
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            self.apply_chunk(&chunk, &mut report);
        }
        Ok(report)
    }

    /// Applies one chunk, preferring the fast path (all of it in one
    /// transaction) and, when that transaction fails, splitting the chunk
    /// in half and retrying each half the same way until the failure is
    /// cornered on a single record, which is then skipped and counted.
    ///
    /// The fallback is not optional bookkeeping: DuckDB aborts an explicit
    /// transaction on the first failing statement and every later statement
    /// in it -- including the `COMMIT` -- then does nothing, so a chunk
    /// that errored has projected *none* of its records and there is no way
    /// to skip the bad one in place. Rolling back and re-running smaller
    /// pieces reprojects everything that is fine and isolates whatever is
    /// not.
    ///
    /// Halving rather than replaying record-by-record keeps the fallback
    /// proportional to the damage: a lone bad record costs ~2 log2(chunk)
    /// extra transactions rather than one per record in the chunk -- the
    /// fsync-per-statement shape the batching exists to avoid. The
    /// degenerate case (every record bad) is still bounded at ~2
    /// transactions per record, since a full binary split of n records is
    /// 2n-1 nodes.
    ///
    /// Retrying a record in a smaller batch is not merely a retry, either:
    /// a record can fail specifically *because* of what the same
    /// transaction wrote before it (the DuckDB optimizer assertion
    /// [`super::projection::Store::most_recent_pending_approval`]
    /// documents needed uncommitted rows in the table to trigger), so a
    /// narrower retry is also where such a record legitimately succeeds.
    fn apply_chunk(&self, chunk: &[Record], report: &mut ApplyRecordsReport) {
        if chunk.is_empty() {
            return;
        }
        let error = match self.apply_chunk_in_one_transaction(chunk) {
            Ok(()) => {
                report.applied += chunk.len();
                return;
            }
            Err(error) => error,
        };
        if chunk.len() == 1 {
            // A one-record transaction is exactly what the live append path
            // runs, so this record cannot be projected at all.
            report.record_skip(&error);
            return;
        }
        let (head, tail) = chunk.split_at(chunk.len() / 2);
        self.apply_chunk(head, report);
        self.apply_chunk(tail, report);
    }

    /// The fast path of [`Self::apply_chunk`]: the whole chunk in one
    /// transaction, rolled back in full on the first error.
    fn apply_chunk_in_one_transaction(&self, chunk: &[Record]) -> Result<()> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        for record in chunk {
            if let Err(error) = self.append_record_uncommitted(record) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    fn clear_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(CLEAR_ALL_AGENT_STATE_SQL)?;
        Ok(())
    }
}
