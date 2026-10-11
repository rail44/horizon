use anyhow::Result;

use crate::persistence::event_log::{LogCursor, ReadReport, Record};

use super::{
    schema::{DROP_ALL_AGENT_STATE_SQL, INITIALIZE_SCHEMA_SQL, PROJECTION_SCHEMA_VERSION},
    Store,
};

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

/// Outcome of a batched append pass ([`Store::
/// replace_from_event_log_records`]'s full rebuild, or [`Store::
/// catch_up_from_event_log`]'s incremental tail) -- lets the caller
/// (`event_log::writer`'s startup) print one
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
    /// Full rebuild: drops and recreates every durable/derived agent table
    /// and reinserts `records` from scratch. Used when the projection's
    /// stamp or cursor does not match the log -- see
    /// `event_log::writer::rebuild_duckdb_projection`. Records
    /// passed here carry no log offsets, so no cursor is written; the
    /// writer's startup uses [`Self::replace_from_event_log`] instead.
    pub(crate) fn replace_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<ApplyRecordsReport> {
        self.replace_positioned(records.into_iter().map(|record| (record, None)))
    }

    /// [`Self::replace_from_event_log_records`] over a whole-file read,
    /// committing a log cursor with every chunk so a later start can verify
    /// one line instead of re-reading the log.
    pub(crate) fn replace_from_event_log(&self, report: ReadReport) -> Result<ApplyRecordsReport> {
        self.replace_positioned(positioned(report))
    }

    fn replace_positioned(
        &self,
        records: impl IntoIterator<Item = (Record, Option<u64>)>,
    ) -> Result<ApplyRecordsReport> {
        self.drop_all_agent_state()?;
        let report = self.apply_records(records)?;
        if report.skipped == 0 {
            self.conn.execute(
                "INSERT INTO agent_projection_format VALUES (true, ?, ?)",
                [
                    crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                    PROJECTION_SCHEMA_VERSION,
                ],
            )?;
        }
        Ok(report)
    }

    /// Incremental catch-up: appends a tail read (the records after the
    /// projection's verified cursor) without clearing any existing state,
    /// advancing the cursor with every chunk.
    pub(crate) fn catch_up_from_event_log(&self, report: ReadReport) -> Result<ApplyRecordsReport> {
        self.apply_records(positioned(report))
    }

    /// Removes the format stamp, so the next start rebuilds instead of
    /// trusting the cursor. For a catch-up that skipped a record: chunks
    /// after the skipped one still committed, with their cursor.
    pub(crate) fn invalidate_format(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM agent_projection_format;")?;
        Ok(())
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
    /// freshness check never found a matching mark and rebuilt again,
    /// every single time.
    /// Batching the loop into transactions turns that into a handful of
    /// seconds.
    ///
    /// Never fails on a single bad record: a chunk whose transaction errors
    /// is retried in smaller pieces so only the offending record is skipped
    /// (counted in [`ApplyRecordsReport::skipped`], reported once by the
    /// caller) -- see [`Self::apply_chunk`].
    fn apply_records(
        &self,
        records: impl IntoIterator<Item = (Record, Option<u64>)>,
    ) -> Result<ApplyRecordsReport> {
        let mut report = ApplyRecordsReport::default();
        let mut chunk = Vec::with_capacity(APPLY_CHUNK_RECORDS);
        let mut line_ends = Vec::with_capacity(APPLY_CHUNK_RECORDS);
        for (record, line_end) in records {
            chunk.push(record);
            line_ends.push(line_end);
            if chunk.len() == APPLY_CHUNK_RECORDS {
                self.apply_chunk(&chunk, &line_ends, &mut report);
                chunk.clear();
                line_ends.clear();
            }
        }
        if !chunk.is_empty() {
            self.apply_chunk(&chunk, &line_ends, &mut report);
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
    ///
    /// Each transaction also commits the cursor of its own last record (when
    /// that record's log offset is known), so the cursor always names a
    /// record whose rows are committed with it.
    fn apply_chunk(
        &self,
        chunk: &[Record],
        line_ends: &[Option<u64>],
        report: &mut ApplyRecordsReport,
    ) {
        if chunk.is_empty() {
            return;
        }
        let cursor =
            chunk
                .last()
                .zip(line_ends.last().copied().flatten())
                .map(|(record, offset)| LogCursor {
                    offset,
                    sequence: record.sequence,
                    event_id: record.event_id.clone(),
                });
        let error = match self.append_records_atomic(chunk, cursor.as_ref()) {
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
        let middle = chunk.len() / 2;
        let (head, tail) = chunk.split_at(middle);
        let (head_ends, tail_ends) = line_ends.split_at(middle);
        self.apply_chunk(head, head_ends, report);
        self.apply_chunk(tail, tail_ends, report);
    }

    fn drop_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(DROP_ALL_AGENT_STATE_SQL)?;
        self.conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        Ok(())
    }
}

/// Pairs each record of a read with its line's end offset.
fn positioned(report: ReadReport) -> impl Iterator<Item = (Record, Option<u64>)> {
    let line_ends = report.record_line_ends;
    report
        .records
        .into_iter()
        .enumerate()
        .map(move |(index, record)| (record, line_ends.get(index).copied()))
}
