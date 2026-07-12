use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::persistence::projection::duckdb::{DuckdbStoreHandle, Store};

use super::{read, ReadReport, Record};

/// A single append-only JSONL event log file must have **at most one**
/// `WriterHandle` alive per process. Each handle owns its own background
/// thread, its own `File` (opened in append mode) and its own `BufWriter`;
/// two independent handles targeting the same path are two independent
/// writers racing on the same inode with no coordination between them.
///
/// That is exactly how `/tmp/horizon-agent-events.jsonl` got torn lines in
/// practice: two agent sessions opened moments apart each created their own
/// `WriterHandle`, so two threads flushed buffered writes to the same file
/// out of step with each other, and a write larger than the kernel's atomic
/// write threshold (commonly cited as `PIPE_BUF`, 4KiB on Linux — reasoning
/// deltas and tool payloads routinely exceed that) landed as two interleaved
/// halves instead of one atomic line.
///
/// The fix enforced at the call site (`app::runtime::agent`, the only place
/// that constructs a `WriterHandle` outside of tests) is a process-global
/// cache: every agent session in this process shares one `WriterHandle`,
/// i.e. one thread and one open file, and appends are serialized through
/// that thread's channel. Within a process this makes concurrent appends
/// impossible by construction rather than by locking. See the doc comment
/// on `AGENT_EVENT_LOG_WRITER` for the caller-side enforcement and why
/// per-session log files were rejected as the alternative.
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<AgentEventLogWriterCommand>,
}

/// The outcome of a [`WriterHandle`]'s one-time startup read, delivered
/// exactly once on the [`Receiver`] returned by [`WriterHandle::open`].
///
/// Before this arrives, [`WriterHandle::append`] and [`WriterHandle::flush`]
/// already work correctly — they just enqueue onto the same channel the
/// background thread eventually drains once it has finished the read (see
/// the "Ordering guarantee" section on [`WriterHandle::open`]), so a caller
/// never has to wait for this to decide whether it's safe to start
/// appending.
pub enum WriterInit {
    /// The startup read succeeded. Carries the same [`ReadReport`] `read`
    /// produced, so a caller that also needs the log's contents (the
    /// DuckDB replay in `app::runtime::agent`) doesn't read the file a
    /// second time.
    Ready(ReadReport),
    /// The startup read, or opening the file for appending, failed. The
    /// background thread exits without ever draining its command channel,
    /// so every [`WriterHandle::append`]/[`WriterHandle::flush`] call made
    /// from this point on returns `Err` immediately (the channel's only
    /// receiver is gone) instead of queuing forever with nothing to drain
    /// it, or hanging on a flush reply nobody will ever send.
    Failed(anyhow::Error),
}

impl WriterHandle {
    /// Opens (or creates) the event log at `path` without blocking the
    /// calling thread on the read needed to compute the log's next
    /// sequence number: the returned handle's channel exists immediately,
    /// and a single background thread performs that (potentially
    /// expensive — see the module doc) read before it starts draining
    /// [`Self::append`]/[`Self::flush`] commands. The read's outcome
    /// arrives exactly once on the returned [`Receiver`] (see
    /// [`WriterInit`]); callers that don't need it (i.e. every call after
    /// the process's first, per `open_agent_event_log`'s cache) can simply
    /// drop the receiver.
    ///
    /// ## Ordering guarantee
    ///
    /// [`Self::append`] no longer computes a sequence number itself the
    /// way the previous (synchronous-open) design did, via a shared
    /// `Arc<Mutex<u64>>` — it just enqueues the record (with a placeholder
    /// `sequence`) onto the channel and returns, regardless of whether the
    /// startup read has finished. The background thread is the *only*
    /// place a sequence number is ever assigned:
    ///
    /// 1. It reads the file and computes `next_sequence` (one past the
    ///    highest sequence already on disk). Every `append` call made
    ///    while this is in progress — by any number of caller threads —
    ///    just piles up, in send order, inside the channel: an unbounded
    ///    channel's `send` never blocks on a slow or absent receiver, so
    ///    no caller (including the UI thread) is ever stalled waiting for
    ///    the read to finish.
    /// 2. Once the read finishes, the thread starts receiving from the
    ///    channel. For each `Append` it dequeues, it assigns the record
    ///    the current `next_sequence` and increments a local counter —
    ///    one thread processing one channel strictly in order, so this
    ///    needs no lock at all (unlike the previous design's mutex shared
    ///    across every caller thread).
    ///
    /// Every record — whether it was sent a microsecond after `open`
    /// returned or an hour later — passes through this same
    /// single-threaded assignment point exactly once, so sequence numbers
    /// stay unique and strictly increasing in whatever order the writer
    /// thread happens to dequeue them. That dequeue order, across multiple
    /// concurrent caller threads, is some valid interleaving of their
    /// sends — exactly the same nondeterminism the old mutex-based design
    /// had in deciding which of two racing callers got the lower sequence
    /// number, just resolved by channel delivery order instead of lock
    /// acquisition order.
    pub fn open(path: impl AsRef<Path>) -> (Self, Receiver<WriterInit>) {
        Self::open_with_reader(path, |path| read(path))
    }

    /// Same as [`Self::open`], but suppresses this module's own
    /// skipped-lines summary line to stderr (see [`start_up`]) -- for
    /// `horizon-sessiond`'s startup only, which already prints its own
    /// differently-prefixed summary right after this call's `init_rx`
    /// resolves (`horizon-sessiond::main::open_persistence`). Without this,
    /// sessiond's stderr got the same summary twice per startup, once from
    /// each of the two call sites. Every other caller (Horizon's own tests,
    /// and any future direct caller) keeps getting the line via
    /// [`Self::open`]/[`Self::open_with_reader`].
    ///
    /// `duckdb_path`, when `Some`, is also this call's seam onto the live
    /// DuckDB projection (`docs/agent-duckdb-state-design.md`'s "Runtime
    /// Boundary" addendum): once the startup read finishes and
    /// [`WriterInit::Ready`] has been sent, the background thread opens
    /// (and, if needed, rebuilds) the projection at that path and *keeps*
    /// the `Store` open (behind an `Arc<Mutex<_>>` -- see
    /// [`rebuild_and_open_duckdb_projection`]'s doc comment for why a
    /// second independent open of the same path is unsound, not just
    /// redundant) for the rest of the process's life, projecting every
    /// subsequent [`Self::append`] right after its JSONL line is durably
    /// written -- see [`run_writer`]. The decision (`Some(store)`, or
    /// `None` if there's nothing to share) is delivered exactly once on the
    /// returned second [`Receiver`]; a caller that needs to hand the same
    /// live store to more than one consumer (recall tools, the rig
    /// provider's history replay) should drain it into a shared,
    /// multi-reader cell (`persistence::projection::duckdb::
    /// SharedDuckdbStore`) rather than relying on this channel's
    /// single-delivery semantics directly. This is `horizon-sessiond`'s only
    /// real caller of `duckdb_path`; every other caller of this module
    /// (Horizon's own tests, [`Self::open`]/[`Self::open_with_reader`])
    /// passes `None` and gets the exact pre-recall behavior (JSONL only,
    /// DuckDB never touched here) plus an immediately-`None` second
    /// receiver.
    pub fn open_silently(
        path: impl AsRef<Path>,
        duckdb_path: Option<PathBuf>,
    ) -> (
        Self,
        Receiver<WriterInit>,
        Receiver<Option<DuckdbStoreHandle>>,
    ) {
        Self::open_inner(path, |path| read(path), false, duckdb_path)
    }

    /// Same mechanism as [`Self::open`], but lets a caller substitute the
    /// function that performs the startup read. Production code always
    /// goes through [`Self::open`] (which passes the real [`read`]); tests
    /// use this directly to gate the read behind a barrier — proving
    /// appends queued during the wait still land with correct sequences —
    /// or to observe which thread it runs on — proving it isn't the
    /// caller's.
    pub fn open_with_reader(
        path: impl AsRef<Path>,
        reader: impl FnOnce(&Path) -> Result<ReadReport> + Send + 'static,
    ) -> (Self, Receiver<WriterInit>) {
        let (handle, init_rx, _duckdb_rx) = Self::open_inner(path, reader, true, None);
        (handle, init_rx)
    }

    fn open_inner(
        path: impl AsRef<Path>,
        reader: impl FnOnce(&Path) -> Result<ReadReport> + Send + 'static,
        log_skipped_summary: bool,
        duckdb_path: Option<PathBuf>,
    ) -> (
        Self,
        Receiver<WriterInit>,
        Receiver<Option<DuckdbStoreHandle>>,
    ) {
        let path = path.as_ref().to_path_buf();
        let (tx, rx) = unbounded();
        let (init_tx, init_rx) = unbounded();
        let (duckdb_tx, duckdb_rx) = unbounded();

        thread::spawn(move || match start_up(&path, reader, log_skipped_summary) {
            Ok((file, report, next_sequence)) => {
                // Seed the rebuild from this read's records *before* handing
                // `report` to `WriterInit::Ready` below (which moves it) --
                // readiness (this send) must not wait on the rebuild, so it
                // fires first; the rebuild itself runs right after, still on
                // this same background thread, before `run_writer` starts
                // draining the channel (see this fn's doc comment and
                // `run_writer`'s doc comment for why appends sent in that
                // window queue harmlessly rather than racing anything).
                let duckdb_seed_records = duckdb_path.is_some().then(|| report.records.clone());
                let _ = init_tx.send(WriterInit::Ready(report));
                let duckdb_store = match (duckdb_path.as_deref(), duckdb_seed_records) {
                    (Some(duckdb_path), Some(records)) => {
                        rebuild_and_open_duckdb_projection(duckdb_path, &records)
                    }
                    _ => None,
                };
                let _ = duckdb_tx.send(duckdb_store.clone());
                run_writer(file, &path, rx, next_sequence, duckdb_store);
            }
            Err(error) => {
                let _ = init_tx.send(WriterInit::Failed(error));
                // No writer loop: dropping `rx` here (it's only captured by
                // this closure) makes every `append`/`flush` sent from now
                // on fail fast with a disconnected-channel error instead of
                // queuing forever with nothing to drain it. `duckdb_tx` is
                // dropped too without ever sending -- callers of the
                // returned `duckdb_rx` observe a disconnected channel
                // (`recv()` returns `Err`), which they treat the same as an
                // explicit `None` (nothing to share).
            }
        });

        (Self { tx }, init_rx, duckdb_rx)
    }

    pub fn append(&self, record: Record) -> Result<()> {
        self.tx
            .send(AgentEventLogWriterCommand::Append(Box::new(record)))
            .context("enqueue agent event log record")
    }

    /// Blocks until every record enqueued before this call has been handed
    /// to `serde_json` and the underlying `BufWriter` has been flushed to
    /// the OS. Because the writer thread performs the startup read before
    /// it ever looks at the channel, and then processes the channel
    /// strictly in order, a reply on the returned channel guarantees both
    /// that the startup read has finished and that everything sent via
    /// [`Self::append`] beforehand is now durable on disk (modulo the OS's
    /// own page cache — this is not an `fsync`).
    ///
    /// Used by tests to assert durability deterministically, and by
    /// `app::shutdown` (wired to floem's `AppEvent::WillTerminate`) so a
    /// normal app exit doesn't lose whatever is still sitting in the
    /// writer's buffer. A hard kill bypasses this and can still leave a
    /// torn final line — `event_log::read` tolerates that (see
    /// `ReadReport::ignored_partial_line`).
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(AgentEventLogWriterCommand::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }

    /// Identity check: do these two handles share the same background
    /// writer thread? Used to assert that the process-global cache in
    /// `horizon`'s `app::runtime::agent` really does hand out one shared
    /// writer instead of silently creating a second one. Not `cfg(test)`
    /// even though it's only ever used by that regression test — the test
    /// lives in a downstream crate (`horizon`) whose test build can't
    /// trigger this crate's own `cfg(test)`.
    pub fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

enum AgentEventLogWriterCommand {
    Append(Box<Record>),
    Flush(Sender<Result<()>>),
}

/// Creates the log's parent directory if needed, performs the startup read
/// via `reader` (the real [`read`] in production; a test-substitutable
/// closure in tests — see [`WriterHandle::open_with_reader`]), computes the
/// sequence number one past the highest already on disk, and opens the file
/// for appending. Everything here runs on the writer's background thread,
/// never on the caller of `open`. `log_skipped_summary` gates this
/// function's own stderr line -- `false` only for
/// [`WriterHandle::open_silently`] (see its doc comment for why).
fn start_up(
    path: &Path,
    reader: impl FnOnce(&Path) -> Result<ReadReport>,
    log_skipped_summary: bool,
) -> Result<(std::fs::File, ReadReport, u64)> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create agent event log directory {}", parent.display())
            })?;
        }
    }

    let report = reader(path)?;
    if log_skipped_summary {
        if let Some(summary) = report.skipped_summary() {
            eprintln!(
                "horizon agent event log: {summary} while opening {}",
                path.display()
            );
        }
    }
    let next_sequence = report
        .records
        .iter()
        .map(|record| record.sequence)
        .max()
        .map(|sequence| sequence + 1)
        .unwrap_or(0);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open agent event log for writing {}", path.display()))?;

    Ok((file, report, next_sequence))
}

/// Opens (creating parent directories as needed) and reconciles the DuckDB
/// projection at `duckdb_path` against `records` -- this is where
/// `horizon-sessiond`'s old `main::rebuild_duckdb_projection` (a separate,
/// short-lived `Store::open` spawned only after readiness) now lives,
/// folded into the event-log writer's own startup so the `Store` returned
/// here can be *kept* by [`run_writer`] afterward instead of being dropped
/// right after the rebuild -- that's what makes every subsequent append
/// project live instead of only at the next restart (see
/// `docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum).
///
/// Three outcomes, decided by [`duckdb_projection_currency`] (unless
/// [`Store::migrated_legacy_schema`] just ran, which invalidates the
/// projection's own high-water mark and forces a full rebuild
/// unconditionally -- see that method's doc comment):
///
/// - **Current**: the mark already matches the log's tail. No work; prints
///   "already current, skipping rebuild".
/// - **Behind**: the mark is behind the tail by a known amount. Only the
///   records beyond it are projected ([`Store::catch_up_from_event_log_records`]);
///   prints "caught up incrementally (N record(s))". Falls back to a full
///   rebuild if the catch-up itself errors.
/// - **RebuildNeeded** (mark ahead of the tail, or absent while the log is
///   non-empty) or a freshness-check error: a full rebuild
///   ([`Store::replace_from_event_log_records`]); prints
///   "rebuilt (N record(s))" -- the original, pre-incremental-catch-up
///   line, kept unchanged so operators (and `horizon-sessiond`'s own e2e
///   tests, which poll for these exact strings) see the same signal for
///   this case.
///
/// Either apply path can also skip a legacy `TurnEnded` event with no
/// `turn_id` (see [`Store::project_event`]'s doc comment); rather than one
/// stderr line per such event, [`log_turn_id_missing_summary`] prints one
/// combined count after the apply.
///
/// Failure (creating the parent directory, opening the store, or the
/// rebuild itself) is reported to stderr and returns `None`: a rebuildable,
/// non-authoritative derived view failing here must never take down the
/// JSONL writer thread it shares a process with -- JSONL stays the source
/// of truth regardless, and the next restart's rebuild reconciles.
///
/// Returns the store wrapped in `Arc<Mutex<_>>`, not a bare `Store`: a
/// second, independent `Store::open` against the same path from elsewhere
/// in this process (a recall tool's lookup, the rig provider's history
/// replay, an external `duckdb` CLI invocation) does *not* share this
/// instance's state the way "same path, same process" might suggest --
/// `duckdb-rs`'s `Connection::open` has no instance cache and opens a
/// wholly separate database instance every time, and DuckDB's relaxed
/// durability means this instance's own committed appends can sit in *its*
/// in-memory WAL well before (or without ever, until a checkpoint) landing
/// in the on-disk file a second instance would read -- confirmed in
/// practice as a second same-path open seeing zero rows for a session with
/// substantial real history. The only sound way to give more than one part
/// of the process a live view is to hand out clones of this *one* `Arc`
/// (see `persistence::projection::duckdb::SharedDuckdbStore`, and this
/// function's caller in [`WriterHandle::open_inner`]) rather than letting
/// anyone open the file again.
fn rebuild_and_open_duckdb_projection(
    duckdb_path: &Path,
    records: &[Record],
) -> Option<DuckdbStoreHandle> {
    if let Some(delay) = test_duckdb_rebuild_delay() {
        thread::sleep(delay);
    }

    if let Some(parent) = duckdb_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "horizon-sessiond: failed to create DuckDB projection directory {} ({error}); \
                     live projection disabled for this run",
                    parent.display()
                );
                return None;
            }
        }
    }

    let store = match Store::open(duckdb_path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "horizon-sessiond: DuckDB projection unavailable ({error}); live projection \
                 disabled for this run"
            );
            return None;
        }
    };

    if !store.migrated_legacy_schema() {
        match duckdb_projection_currency(&store, records) {
            Ok(ProjectionCurrency::Current) => {
                eprintln!("horizon-sessiond: DuckDB projection already current, skipping rebuild");
                return Some(Arc::new(Mutex::new(store)));
            }
            Ok(ProjectionCurrency::Behind(mark)) => {
                let tail = records
                    .iter()
                    .filter(|record| record.sequence as i64 > mark)
                    .cloned();
                match store.catch_up_from_event_log_records(tail) {
                    Ok(report) => {
                        log_turn_id_missing_summary(report.turn_id_missing);
                        eprintln!(
                            "horizon-sessiond: DuckDB projection caught up incrementally \
                             ({} record(s))",
                            report.applied
                        );
                        return Some(Arc::new(Mutex::new(store)));
                    }
                    Err(error) => eprintln!(
                        "horizon-sessiond: DuckDB incremental catch-up failed ({error}), \
                         falling back to a full rebuild"
                    ),
                }
            }
            Ok(ProjectionCurrency::RebuildNeeded) => {}
            Err(error) => eprintln!(
                "horizon-sessiond: DuckDB projection freshness check failed ({error}), rebuilding"
            ),
        }
    }

    match store.replace_from_event_log_records(records.iter().cloned()) {
        Ok(report) => {
            log_turn_id_missing_summary(report.turn_id_missing);
            eprintln!(
                "horizon-sessiond: DuckDB projection rebuilt ({} record(s))",
                report.applied
            );
            Some(Arc::new(Mutex::new(store)))
        }
        Err(error) => {
            eprintln!(
                "horizon-sessiond: DuckDB projection rebuild failed ({error}); live projection \
                 disabled for this run"
            );
            None
        }
    }
}

/// One combined stderr line for however many legacy no-`turn_id`
/// `TurnEnded` events a rebuild or incremental catch-up skipped -- a no-op
/// when `count` is zero. Replaces what used to be one
/// "TurnEnded ... has no turn_id; skipping agent_turns projection" line per
/// event (see [`Store::project_event`]'s doc comment), which drowned out
/// everything else in stderr against a real archived log with thousands of
/// pre-`turn_id` events.
fn log_turn_id_missing_summary(count: usize) {
    if count > 0 {
        eprintln!(
            "horizon-agent: skipped agent_turns projection for {count} legacy TurnEnded \
             event(s) with no turn_id"
        );
    }
}

/// How [`rebuild_and_open_duckdb_projection`] should reconcile `store`'s
/// existing high-water mark against the event log's own tail sequence.
enum ProjectionCurrency {
    /// The mark already matches the log's tail -- nothing to do.
    Current,
    /// The mark is behind the log's tail by a known amount: projecting only
    /// `sequence > mark` reaches the same end state as a full rebuild, at a
    /// fraction of the cost against a large real log.
    Behind(i64),
    /// The mark is ahead of the log's tail, or absent while the log is
    /// non-empty -- either way the projection's own state can't be trusted
    /// to be a prefix of the current log (a reset/corrupted store, or one
    /// built against a log that has since been replaced), so a full rebuild
    /// is the only safe path.
    RebuildNeeded,
}

/// Compares `store`'s existing high-water mark ([`Store::max_last_sequence`])
/// against `records`' own tail sequence to decide which
/// [`ProjectionCurrency`] applies. `records` is already sorted ascending by
/// `event_log::read`, so its last element carries the log's overall maximum
/// sequence.
fn duckdb_projection_currency(store: &Store, records: &[Record]) -> Result<ProjectionCurrency> {
    let log_final_sequence = records.last().map(|record| record.sequence as i64);
    let mark = store.max_last_sequence()?;
    Ok(match (mark, log_final_sequence) {
        (None, None) => ProjectionCurrency::Current,
        (Some(mark), Some(tail)) if mark == tail => ProjectionCurrency::Current,
        (Some(mark), Some(tail)) if mark < tail => ProjectionCurrency::Behind(mark),
        _ => ProjectionCurrency::RebuildNeeded,
    })
}

/// Test-only hook, mirroring `horizon-sessiond::main`'s own resume-delay hook
/// (`TEST_RESUME_DELAY_MS_VAR`): when set, artificially delays this thread's
/// DuckDB rebuild (which runs *after* [`WriterInit::Ready`] has already been
/// sent -- see [`WriterHandle::open_inner`]) so a test can prove the
/// rebuild never sits on the readiness path `session_list`/`session_new`
/// block on. Never set outside `horizon-sessiond`'s own e2e tests.
const TEST_DUCKDB_REBUILD_DELAY_MS_VAR: &str = "HORIZON_SESSIOND_TEST_DUCKDB_REBUILD_DELAY_MS";

fn test_duckdb_rebuild_delay() -> Option<Duration> {
    std::env::var(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Drains `rx` for the lifetime of the writer, assigning each `Append`
/// command the next sequence number in `next_sequence` (seeded by
/// [`start_up`] from the startup read) — see the "Ordering guarantee"
/// section on [`WriterHandle::open`] for why this single-threaded loop is
/// the only place that assignment ever happens.
///
/// `duckdb_store`, when `Some` (only for a `horizon-sessiond` writer opened
/// via [`WriterHandle::open_silently`] with a DuckDB path -- see
/// [`WriterHandle::open_inner`]), is locked and projected into right after
/// each record's own JSONL line is durably written, keeping the projection
/// live. The lock is only ever briefly held (one row's worth of inserts);
/// write volume is low enough (roughly 1-2 events/s) that contention with
/// another locker of the *same* `Arc` (a recall tool's query, the rig
/// provider's history replay -- see `persistence::projection::duckdb::
/// SharedDuckdbStore`) is a non-issue. A projection failure only ever warns
/// once (a simple "warned already" latch, not per-event) to avoid log spam
/// -- the JSONL write it followed already succeeded regardless, and the
/// next restart's rebuild reconciles any projection rows this run couldn't
/// keep live.
fn run_writer(
    file: std::fs::File,
    path: &Path,
    rx: Receiver<AgentEventLogWriterCommand>,
    mut next_sequence: u64,
    duckdb_store: Option<DuckdbStoreHandle>,
) {
    let mut writer = BufWriter::new(file);
    let mut warned_duckdb_append_failure = false;
    let mut warned_duckdb_turn_id_missing = false;

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(mut record) => {
                record.sequence = next_sequence;
                next_sequence += 1;
                if serde_json::to_writer(&mut writer, &record).is_ok() {
                    let _ = writer.write_all(b"\n");
                    // Flushed immediately (not just batched in `BufWriter`'s
                    // in-memory buffer) so a hard kill (SIGKILL, crash --
                    // `horizon-sessiond` has no signal handler for it and runs
                    // no destructors) can only ever lose events that hadn't
                    // arrived on this channel yet, never ones already
                    // appended. This is what makes `docs/agent-runtime-
                    // split-design.md` step 4's "sessiond restart: read own
                    // log, mark turns that died mid-flight as cancelled"
                    // meaningful against a real `kill -9` — without this, a
                    // session parked indefinitely in `WaitingForApproval`
                    // (no further traffic to trigger a flush) could lose its
                    // whole transcript to the process's own internal
                    // buffering alone, with nothing to do with the kill
                    // itself. Still not an `fsync` (see `WriterHandle::
                    // flush`'s doc comment) -- a full machine crash / power
                    // loss can still lose an unsynced page-cache write; that
                    // tier of durability is out of scope here.
                    let _ = writer.flush();

                    if let Some(store) = &duckdb_store {
                        let store = store
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        match store.append_record(&record) {
                            Ok(true) if !warned_duckdb_turn_id_missing => {
                                // A live TurnEnded with no turn_id should not
                                // happen for a current provider (see
                                // `Store::project_event`'s doc comment) --
                                // rare enough that a warn-once latch, not a
                                // batched summary, is the right shape here.
                                eprintln!(
                                    "horizon-agent: a live TurnEnded event had no turn_id; its \
                                     agent_turns projection was skipped -- further occurrences \
                                     in this run won't be logged individually"
                                );
                                warned_duckdb_turn_id_missing = true;
                            }
                            Ok(_) => {}
                            Err(error) => {
                                if !warned_duckdb_append_failure {
                                    eprintln!(
                                        "horizon-sessiond: DuckDB projection append failed \
                                         ({error}); further append failures in this run won't \
                                         be logged individually -- the next restart's rebuild \
                                         reconciles"
                                    );
                                    warned_duckdb_append_failure = true;
                                }
                            }
                        }
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use uuid::Uuid;

    use super::*;
    use crate::contract::SessionId;
    use crate::contract::{Event, SessionState};

    fn record_at(session_id: SessionId, sequence: u64) -> Record {
        Record {
            schema: super::super::AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: super::super::AGENT_EVENT_LOG_VERSION,
            event_id: Uuid::new_v4().to_string(),
            sequence,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            event_kind: "state_changed".to_string(),
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            created_at_unix_ms: sequence + 1,
        }
    }

    /// A legacy pre-`turn_id` `TurnEnded` record -- the shape a real
    /// archived event log carries for sessions created before `turn_id`
    /// existed. Its `agent_turns` projection is always skipped (see
    /// `projection::Store::insert_turn`'s doc comment); this is the record
    /// this module's own currency/incremental tests use to prove that skip
    /// still advances the projection's high-water mark.
    fn legacy_turn_ended_record(session_id: SessionId, sequence: u64) -> Record {
        Record {
            turn_id: None,
            event_kind: "turn_ended".to_string(),
            event: Event::TurnEnded(crate::contract::TurnEndReason::Completed),
            ..record_at(session_id, sequence)
        }
    }

    /// A record carrying a distinctive, inspectable `MessageCommitted` body
    /// -- used by the incremental-catch-up/full-rebuild-fallback tests
    /// below to tell the two apply paths apart by their *effect* rather
    /// than by scraping stderr: a full rebuild clears and reinserts
    /// everything it's given, so a mutated payload at an old sequence
    /// number would visibly overwrite the original row, while an
    /// incremental catch-up only touches records past the existing mark and
    /// must leave earlier rows exactly as they were.
    fn message_record(session_id: SessionId, sequence: u64, text: &str) -> Record {
        Record {
            event_kind: "message_committed".to_string(),
            event: Event::MessageCommitted(crate::contract::Message {
                role: crate::contract::MessageRole::User,
                text: text.to_string(),
            }),
            ..record_at(session_id, sequence)
        }
    }

    /// The whole point of moving the startup read off the caller: prove it
    /// actually runs on a different thread. Regression guard for "New
    /// Agent" blocking pane render on the UI thread while the event log's
    /// startup read runs.
    #[test]
    fn open_runs_the_startup_read_on_a_background_thread_not_the_caller() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-writer-thread-{}.jsonl",
            Uuid::new_v4()
        ));
        let calling_thread = thread::current().id();
        let observed = Arc::new(Mutex::new(None));
        let observed_in_reader = observed.clone();

        let (_writer, init_rx) = WriterHandle::open_with_reader(&path, move |path| {
            *observed_in_reader.lock().unwrap() = Some(thread::current().id());
            read(path)
        });

        match init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(_) => {}
            WriterInit::Failed(error) => panic!("unexpected startup failure: {error}"),
        }

        let observed_thread = observed.lock().unwrap().expect("reader ran");
        assert_ne!(
            observed_thread, calling_thread,
            "the startup read must run on a background thread, not the caller of `open`"
        );

        let _ = std::fs::remove_file(path);
    }

    /// The ordering guarantee documented on `WriterHandle::open`: appends
    /// sent while the startup read is still in flight must queue up (not
    /// block, not race the read for a sequence number) and end up with
    /// correct, unique, monotonically increasing sequences once the read
    /// completes and the writer thread drains them.
    #[test]
    fn sequence_numbers_stay_correct_when_appends_race_the_startup_read() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-writer-race-{}.jsonl",
            Uuid::new_v4()
        ));
        let session_id = SessionId::new();

        // Pre-existing history on disk, so the correct starting sequence
        // (3) isn't the trivial "empty file" case of 0.
        let pre_existing: Vec<Record> = (0..3)
            .map(|sequence| record_at(session_id, sequence))
            .collect();
        let contents = pre_existing
            .iter()
            .map(|record| serde_json::to_string(record).expect("serialize"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, contents).expect("seed pre-existing log");

        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);
        let (writer, init_rx) = WriterHandle::open_with_reader(&path, move |path| {
            // Blocks until the test has queued its appends below, proving
            // they queue up rather than blocking on (or racing) this read.
            gate_rx.recv().expect("gate released");
            read(path)
        });

        let queued_before_read = 5;
        for _ in 0..queued_before_read {
            // The `sequence` passed here is a placeholder; only the writer
            // thread's post-read assignment is authoritative.
            writer
                .append(record_at(session_id, 999))
                .expect("append queues even while the startup read is gated");
        }

        gate_tx.send(()).expect("release the gated startup read");
        match init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(report) => assert_eq!(report.records.len(), 3),
            WriterInit::Failed(error) => panic!("unexpected startup failure: {error}"),
        }
        writer.flush().expect("flush queued appends");

        let final_report = read(&path).expect("read final log");
        assert_eq!(final_report.records.len(), 3 + queued_before_read);
        let mut sequences: Vec<u64> = final_report.records.iter().map(|r| r.sequence).collect();
        sequences.sort_unstable();
        let expected: Vec<u64> = (0..(3 + queued_before_read) as u64).collect();
        assert_eq!(
            sequences, expected,
            "appends queued during the startup read must still get correct, unique, \
             monotonically increasing sequence numbers once the read completes"
        );

        let _ = std::fs::remove_file(path);
    }

    /// A startup failure (here: the file the reader would have opened is
    /// actually a directory, so both the read and the write-mode open
    /// fail) must not hang a later `flush` — the writer thread exits
    /// without ever looping, dropping the command channel's receiver, so
    /// `flush` observes a disconnected channel and returns `Err` instead
    /// of blocking forever on a reply nobody will send.
    #[test]
    fn flush_fails_fast_instead_of_hanging_after_a_startup_failure() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-writer-startup-failure-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create directory standing in for the log path");

        let (writer, init_rx) = WriterHandle::open(&path);
        match init_rx.recv().expect("writer init outcome") {
            WriterInit::Failed(_) => {}
            WriterInit::Ready(_) => panic!("expected startup to fail against a directory path"),
        }

        assert!(
            writer.flush().is_err(),
            "flush after a startup failure must fail fast, not hang"
        );

        let _ = std::fs::remove_dir_all(path);
    }

    // --- currency check + incremental catch-up (backlog-32) ----------------

    fn temp_duckdb_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-agent-writer-{label}-{}.duckdb",
            Uuid::new_v4()
        ))
    }

    /// The bug this guards: `duckdb_projection_currency` must find the
    /// projection current even when the log it was built from contains a
    /// record whose own projection is deliberately skipped (a legacy
    /// no-`turn_id` `TurnEnded`) -- `agent_events`/`agent_sessions` are
    /// still updated for that record (see `append::Store::append_record`'s
    /// doc comment), so the high-water mark advances past it exactly like
    /// any other record.
    #[test]
    fn currency_check_is_current_after_seeding_with_a_skipped_record() {
        let store = Store::open_in_memory().expect("open in-memory store");
        let session_id = SessionId::new();
        let records = vec![
            record_at(session_id, 0),
            legacy_turn_ended_record(session_id, 1),
        ];

        let report = store
            .replace_from_event_log_records(records.clone())
            .expect("seed store");
        assert_eq!(
            report.turn_id_missing, 1,
            "the legacy TurnEnded's own projection must be skipped"
        );

        match duckdb_projection_currency(&store, &records).expect("currency check") {
            ProjectionCurrency::Current => {}
            _ => panic!(
                "a skipped record must still advance agent_sessions.last_sequence, so a \
                 re-check of the same log must read as current"
            ),
        }
    }

    /// End-to-end version of the check above, through the real entry point
    /// (`rebuild_and_open_duckdb_projection`) two "boots" in a row against
    /// an unchanged, skip-containing log: the second boot must not touch
    /// the store at all. Proven by `agent_sessions.updated_at`, which every
    /// write path (`Store::upsert_session`) stamps with `now()` -- if the
    /// second boot did any work (a rebuild or a catch-up), that timestamp
    /// would move; "already current" is the only outcome that leaves it
    /// alone.
    #[test]
    fn already_current_on_second_boot_after_a_skip_containing_log() {
        let duckdb_path = temp_duckdb_path("already-current");
        let session_id = SessionId::new();
        let records = vec![
            record_at(session_id, 0),
            legacy_turn_ended_record(session_id, 1),
        ];

        let first_boot =
            rebuild_and_open_duckdb_projection(&duckdb_path, &records).expect("first boot store");
        let first_updated_at = {
            let guard = first_boot.lock().unwrap();
            guard.sessions().expect("sessions")[0].updated_at.clone()
        };
        drop(first_boot);

        thread::sleep(Duration::from_millis(20));

        let second_boot =
            rebuild_and_open_duckdb_projection(&duckdb_path, &records).expect("second boot store");
        let second_updated_at = {
            let guard = second_boot.lock().unwrap();
            guard.sessions().expect("sessions")[0].updated_at.clone()
        };

        assert_eq!(
            first_updated_at, second_updated_at,
            "a second boot against an unchanged (skip-containing) log must skip the rebuild \
             entirely; any real write would have advanced agent_sessions.updated_at"
        );

        drop(second_boot);
        let _ = std::fs::remove_file(&duckdb_path);
        let _ = std::fs::remove_file(duckdb_path.with_extension("duckdb.wal"));
    }

    /// Work item 2: when the mark is behind the log's tail, only the tail
    /// is projected -- proven by mutating the payload of an *earlier*
    /// sequence between the two boots. A full rebuild clears and reinserts
    /// everything it's given, so it would pick up the mutation; an
    /// incremental catch-up never re-touches a sequence at or before the
    /// existing mark, so the original payload must survive.
    #[test]
    fn behind_mark_triggers_incremental_catch_up_that_preserves_earlier_rows() {
        let duckdb_path = temp_duckdb_path("incremental");
        let session_id = SessionId::new();

        let first_boot_records = vec![message_record(session_id, 0, "first")];
        let first_boot = rebuild_and_open_duckdb_projection(&duckdb_path, &first_boot_records)
            .expect("first boot store");
        drop(first_boot);

        // The log grew by one record; sequence 0's payload is deliberately
        // different from what boot 1 saw.
        let second_boot_records = vec![
            message_record(
                session_id,
                0,
                "MUTATED -- must not appear if this was incremental",
            ),
            message_record(session_id, 1, "second"),
        ];
        let second_boot = rebuild_and_open_duckdb_projection(&duckdb_path, &second_boot_records)
            .expect("second boot store");

        let guard = second_boot.lock().unwrap();
        let messages = guard.messages_for_session(session_id).expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].text, "first",
            "an incremental catch-up must not re-touch rows at or before the existing mark"
        );
        assert_eq!(messages[1].text, "second");
        drop(guard);

        drop(second_boot);
        let _ = std::fs::remove_file(&duckdb_path);
        let _ = std::fs::remove_file(duckdb_path.with_extension("duckdb.wal"));
    }

    /// Work item 2's other named fallback case: a missing store (nothing to
    /// catch up from) always does a full rebuild -- the ordinary first-boot
    /// path, asserted explicitly here since the incremental-catch-up branch
    /// above could otherwise plausibly regress it into the wrong shape.
    #[test]
    fn missing_store_falls_back_to_a_full_rebuild() {
        let duckdb_path = temp_duckdb_path("missing-store");
        let session_id = SessionId::new();
        let records = vec![message_record(session_id, 0, "hello")];

        let store =
            rebuild_and_open_duckdb_projection(&duckdb_path, &records).expect("rebuilt store");
        let guard = store.lock().unwrap();
        let messages = guard.messages_for_session(session_id).expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "hello");
        drop(guard);

        drop(store);
        let _ = std::fs::remove_file(&duckdb_path);
        let _ = std::fs::remove_file(duckdb_path.with_extension("duckdb.wal"));
    }

    /// Work item 2's other named fallback case: a mark *ahead* of the log's
    /// tail (a shorter log than what the projection was last built from --
    /// e.g. a truncated or replaced event log) must trigger a full rebuild,
    /// not an incremental catch-up (there is no well-defined "tail beyond
    /// the mark" when the mark is already past the end) and not a stale
    /// "already current". Proven by a row that only a full rebuild's own
    /// `clear_all_agent_state` would remove: if the second boot wrongly
    /// treated this as current or tried to catch up, the row from the
    /// now-absent sequence 1 would still be there.
    #[test]
    fn ahead_mark_falls_back_to_a_full_rebuild() {
        let duckdb_path = temp_duckdb_path("ahead-mark");
        let session_id = SessionId::new();

        let first_boot_records = vec![
            message_record(session_id, 0, "a"),
            message_record(session_id, 1, "b"),
        ];
        let first_boot = rebuild_and_open_duckdb_projection(&duckdb_path, &first_boot_records)
            .expect("first boot store");
        drop(first_boot);

        // Second "boot" sees a *shorter* log than what the projection was
        // built from -- the store's mark (1) is now ahead of this log's own
        // tail (0).
        let second_boot_records = vec![message_record(session_id, 0, "a")];
        let second_boot = rebuild_and_open_duckdb_projection(&duckdb_path, &second_boot_records)
            .expect("second boot store");

        let guard = second_boot.lock().unwrap();
        let messages = guard.messages_for_session(session_id).expect("messages");
        assert_eq!(
            messages.len(),
            1,
            "an ahead mark must trigger a full rebuild reflecting only what the log currently \
             has, not a stale extra row left over from the old state"
        );
        drop(guard);

        drop(second_boot);
        let _ = std::fs::remove_file(&duckdb_path);
        let _ = std::fs::remove_file(duckdb_path.with_extension("duckdb.wal"));
    }
}
