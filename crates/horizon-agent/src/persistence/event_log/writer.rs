use std::{
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::persistence::projection::duckdb::{ApplyRecordsReport, DuckdbStoreHandle, Store};

use super::{
    cursor_line_matches, health::WriterHealth, read, read_from, LogCursor, ReadReport, Record,
};

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
/// The fix enforced at the call site (`horizon-agentd`'s `open_persistence`,
/// the only place that constructs a production `WriterHandle`) is a
/// process-global cache: every agent session hosted by that agentd
/// process shares one `WriterHandle` (held on `AgentdState` and cloned
/// out per session), i.e. one thread and one open file, and appends are
/// serialized through that thread's channel. Within a process this makes
/// concurrent appends impossible by construction rather than by locking.
/// See `AgentdState::writer`/`set_writer` for the caller-side enforcement
/// and why per-session log files were rejected as the alternative.
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<AgentEventLogWriterCommand>,
    health: WriterHealth,
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
    /// DuckDB rebuild-or-open this module's own `open_silently` drives)
    /// doesn't read the file a second time.
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
    /// `horizon-agentd`'s startup only, which already prints its own
    /// differently-prefixed summary right after this call's `init_rx`
    /// resolves (`horizon-agentd::main::open_persistence`). Without this,
    /// agentd's stderr got the same summary twice per startup, once from
    /// each of the two call sites. Every other caller (Horizon's own tests,
    /// and any future direct caller) keeps getting the line via
    /// [`Self::open`]/[`Self::open_with_reader`].
    ///
    /// `duckdb_path`, when `Some`, is also this call's seam onto the live
    /// DuckDB projection (`docs/agent-duckdb-state-design.md`'s "Runtime
    /// Boundary" and "Incremental startup" addenda). The background thread
    /// opens the projection *before* the startup read and picks one of two
    /// modes:
    ///
    /// - **Incremental**: the projection carries the current format stamp
    ///   and a log cursor whose line still matches the log
    ///   ([`super::cursor_line_matches`]). Only the bytes after the cursor
    ///   are read; that tail is caught up into the projection, the store is
    ///   delivered on the second receiver, and only then is
    ///   [`WriterInit::Ready`] sent -- carrying just the tail, with
    ///   [`ReadReport::read_after`] set to the cursor.
    /// - **Full**: anything else. The whole log is read, `Ready` carries
    ///   every record, and the full rebuild runs *after* `Ready`, off the
    ///   readiness path, exactly as before the cursor existed.
    ///
    /// Either way the thread then *keeps* the `Store` open (behind an
    /// `Arc<Mutex<_>>` -- see [`rebuild_duckdb_projection`]'s doc comment
    /// for why a second independent open of the same path is unsound, not
    /// just redundant) for the rest of the process's life, projecting every
    /// subsequent [`Self::append`] right after its JSONL line is durably
    /// written -- see [`run_writer`]. The decision (`Some(store)`, or
    /// `None` if there's nothing to share) is delivered exactly once on the
    /// returned second [`Receiver`]; a caller that needs to hand the same
    /// live store to more than one consumer (recall tools, the rig
    /// provider's history replay) should drain it into a shared,
    /// multi-reader cell (`persistence::projection::duckdb::
    /// SharedDuckdbStore`) rather than relying on this channel's
    /// single-delivery semantics directly. This is `horizon-agentd`'s only
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

        let health = WriterHealth::default();
        let worker_health = health.clone();
        thread::spawn(move || {
            let _exit = super::health::WriterExit(worker_health.clone());
            let mut store = duckdb_path.as_deref().and_then(open_duckdb_store);
            if let Some(opened) = store.take() {
                match start_up_incrementally(&path, opened) {
                    IncrementalStartup::Ready(started, store) => {
                        // The tail is already projected, so the store goes
                        // out first: a caller that sees `Ready` with
                        // `read_after` set can rely on the store's answer
                        // without waiting on anything else.
                        let store = DuckdbStoreHandle::new(store);
                        let _ = duckdb_tx.send(Some(store.clone()));
                        let StartedLog {
                            file,
                            report,
                            next_sequence,
                            end_offset,
                        } = started;
                        let _ = init_tx.send(WriterInit::Ready(report));
                        run_writer(
                            file,
                            &path,
                            rx,
                            next_sequence,
                            end_offset,
                            Some(store),
                            worker_health,
                        );
                        return;
                    }
                    IncrementalStartup::Rebuild(opened) => store = Some(opened),
                    IncrementalStartup::Unavailable => {}
                }
            }
            match start_up(&path, reader, log_skipped_summary) {
                Ok(StartedLog {
                    file,
                    report,
                    next_sequence,
                    end_offset,
                }) => {
                    // Seed the rebuild from this read's records *before* handing
                    // `report` to `WriterInit::Ready` below (which moves it) --
                    // readiness (this send) must not wait on the rebuild, so it
                    // fires first; the rebuild itself runs right after, still on
                    // this same background thread, before `run_writer` starts
                    // draining the channel (see this fn's doc comment and
                    // `run_writer`'s doc comment for why appends sent in that
                    // window queue harmlessly rather than racing anything).
                    let duckdb_seed = store.is_some().then(|| report.clone());
                    let _ = init_tx.send(WriterInit::Ready(report));
                    let duckdb_store = match (store, duckdb_seed) {
                        (Some(store), Some(seed)) => rebuild_duckdb_projection(store, seed),
                        _ => None,
                    };
                    let _ = duckdb_tx.send(duckdb_store.clone());
                    run_writer(
                        file,
                        &path,
                        rx,
                        next_sequence,
                        end_offset,
                        duckdb_store,
                        worker_health,
                    );
                }
                Err(error) => {
                    worker_health.fail(format!("{error:#}"));
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
            }
        });

        (Self { tx, health }, init_rx, duckdb_rx)
    }

    pub fn failure(&self) -> Option<String> {
        self.health.failure()
    }

    pub fn subscribe_failure(&self) -> super::FailureSubscription {
        self.health.subscribe()
    }

    pub fn append(&self, record: Record) -> Result<()> {
        if let Some(message) = self.failure() {
            anyhow::bail!("{message}");
        }
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
    /// own page cache — this is not an `fsync`). A write or flush failure is
    /// retained for every later flush; subsequent records are not written or
    /// projected until a new writer is opened.
    ///
    /// Used by tests to assert durability deterministically, and by
    /// `horizon-agentd`'s `flush_event_log_before_exit` — which that daemon
    /// calls on *both* of the exits it can intercept, so neither loses
    /// whatever is still sitting in the writer's buffer: the hub's `drain`
    /// (right before `std::process::exit(0)`) and its SIGTERM hook (a plain
    /// `kill`, which returns from the accept loop rather than exiting from
    /// under it). Only what no handler can run ahead of bypasses this —
    /// SIGKILL and a crash — and that can still leave a torn final line,
    /// which `event_log::read` tolerates (see
    /// `ReadReport::ignored_partial_line`).
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(AgentEventLogWriterCommand::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }
}

enum AgentEventLogWriterCommand {
    Append(Box<Record>),
    Flush(Sender<Result<()>>),
}

/// What a successful startup hands to [`run_writer`]: the open append
/// handle, the read's report, the next sequence to assign, and the file
/// length after any torn tail was discarded (the offset the next appended
/// line starts at).
struct StartedLog {
    file: std::fs::File,
    report: ReadReport,
    next_sequence: u64,
    end_offset: u64,
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
) -> Result<StartedLog> {
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
        .max_known_sequence
        .map(|sequence| sequence + 1)
        .unwrap_or(0);
    let (file, end_offset) = open_for_append(path, report.ignored_partial_line)?;
    Ok(StartedLog {
        file,
        report,
        next_sequence,
        end_offset,
    })
}

/// Opens the log for appending, first discarding an unterminated tail the
/// read excluded, and returns the resulting file length.
fn open_for_append(path: &Path, discard_partial: bool) -> Result<(std::fs::File, u64)> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open agent event log for writing {}", path.display()))?;
    if discard_partial {
        discard_partial_tail(&mut file).with_context(|| {
            format!("discard unfinished agent event log tail {}", path.display())
        })?;
    }
    let end_offset = file
        .metadata()
        .with_context(|| format!("stat agent event log {}", path.display()))?
        .len();
    Ok((file, end_offset))
}

/// The reader excludes an unterminated tail. Remove those same bytes before
/// appending, so the next record cannot join them or promote them into history.
fn discard_partial_tail(file: &mut std::fs::File) -> std::io::Result<()> {
    let mut end = file.metadata()?.len();
    let mut buffer = [0; 8192];
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let chunk = &mut buffer[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            return file.set_len(start + index as u64 + 1);
        }
        end = start;
    }
    file.set_len(0)
}

/// Opens (creating parent directories as needed) the DuckDB projection at
/// `duckdb_path`. Failure is reported to stderr and returns `None`: a
/// rebuildable, non-authoritative derived view failing here must never take
/// down the JSONL writer thread it shares a process with -- JSONL stays the
/// source of truth regardless, and the next restart's rebuild reconciles.
fn open_duckdb_store(duckdb_path: &Path) -> Option<Store> {
    if let Some(parent) = duckdb_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "horizon-agentd: failed to create DuckDB projection directory {} ({error}); \
                     live projection disabled for this run",
                    parent.display()
                );
                return None;
            }
        }
    }

    match Store::open(duckdb_path) {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!(
                "horizon-agentd: DuckDB projection unavailable ({error}); live projection \
                 disabled for this run"
            );
            None
        }
    }
}

/// The incremental mode's outcome (see [`WriterHandle::open_silently`]).
// `Ready` carries the whole startup; it is built and consumed once.
#[allow(clippy::large_enum_variant)]
enum IncrementalStartup {
    /// The cursor matched and the tail is projected: `Ready` may go out.
    Ready(StartedLog, Store),
    /// The projection cannot vouch for the log; read everything and rebuild
    /// it after `Ready`.
    Rebuild(Store),
    /// The tail contained a record the projection cannot take. A rebuild
    /// would skip the same record, so run the full read without one, the
    /// same outcome a skipped record has always had.
    Unavailable,
}

/// Tries the incremental mode: a current format stamp, a log cursor, and a
/// matching line at the cursor's offset let startup read only the tail.
/// Prints the same "already current" / "caught up incrementally" lines the
/// whole-log reconciliation used to, so operators keep the same signal.
fn start_up_incrementally(path: &Path, store: Store) -> IncrementalStartup {
    let cursor = match projection_cursor(path, &store) {
        Ok(Ok(cursor)) => cursor,
        Ok(Err(reason)) => {
            eprintln!("horizon-agentd: DuckDB projection {reason}; rebuilding from the event log");
            return IncrementalStartup::Rebuild(store);
        }
        Err(error) => {
            eprintln!(
                "horizon-agentd: DuckDB projection cursor check failed ({error:#}); rebuilding \
                 from the event log"
            );
            return IncrementalStartup::Rebuild(store);
        }
    };
    let started = match read_tail(path, &cursor) {
        Ok(started) => started,
        Err(error) => {
            // The full read surfaces the same error (or succeeds) on its own.
            eprintln!(
                "horizon-agentd: reading the event log after the DuckDB cursor failed \
                 ({error:#}); reading the whole log"
            );
            return IncrementalStartup::Rebuild(store);
        }
    };
    if started.report.records.is_empty() {
        eprintln!("horizon-agentd: DuckDB projection already current, skipping rebuild");
        return IncrementalStartup::Ready(started, store);
    }
    match store.catch_up_from_event_log(started.report.clone()) {
        Ok(report) if report.skipped == 0 => {
            eprintln!(
                "horizon-agentd: DuckDB projection caught up incrementally ({} record(s))",
                report.applied
            );
            IncrementalStartup::Ready(started, store)
        }
        Ok(report) => {
            log_skipped_record_summary(&report);
            // Records after the skipped one may have committed, moving the
            // cursor past a hole. Drop the stamp so no later start trusts it.
            if let Err(error) = store.invalidate_format() {
                eprintln!(
                    "horizon-agentd: failed to invalidate the DuckDB projection after a \
                     skipped catch-up ({error}); the next start may hand it out"
                );
            }
            IncrementalStartup::Unavailable
        }
        Err(error) => {
            eprintln!(
                "horizon-agentd: DuckDB incremental catch-up failed ({error}), falling back to a \
                 full rebuild"
            );
            IncrementalStartup::Rebuild(store)
        }
    }
}

/// The projection's cursor when the boundary check passes, or the reason it
/// cannot be trusted.
fn projection_cursor(path: &Path, store: &Store) -> Result<Result<LogCursor, &'static str>> {
    // An unreadable stamp (a file from before the shape stamp existed) is
    // just as stale as a mismatching one.
    if !store.has_current_format().unwrap_or(false) {
        return Ok(Err("is not stamped with the current format"));
    }
    let Some(cursor) = store.log_cursor()? else {
        return Ok(Err("has no event log cursor"));
    };
    if !path.exists() || !cursor_line_matches(path, &cursor)? {
        return Ok(Err("cursor does not match the event log"));
    }
    Ok(Ok(cursor))
}

/// Reads only the bytes after `cursor` and opens the log for appending.
/// Sequences continue past both the cursor's record and anything the tail
/// holds, including lines whose event this build cannot decode.
fn read_tail(path: &Path, cursor: &LogCursor) -> Result<StartedLog> {
    let mut report = read_from(path, cursor.offset)?;
    report.read_after = Some(cursor.clone());
    let next_sequence = report
        .max_known_sequence
        .map_or(cursor.sequence, |sequence| sequence.max(cursor.sequence))
        + 1;
    let (file, end_offset) = open_for_append(path, report.ignored_partial_line)?;
    Ok(StartedLog {
        file,
        report,
        next_sequence,
        end_offset,
    })
}

/// The full mode's projection step: drops and rebuilds every table from the
/// whole-log read, committing a cursor with each chunk. Runs after
/// [`WriterInit::Ready`], so readiness never waits on it. Prints "rebuilt
/// (N record(s))" -- the original line, kept unchanged so operators (and
/// `horizon-agentd`'s own e2e tests, which poll for these exact strings)
/// see the same signal.
///
/// Failure is reported to stderr and returns `None`, for the same reason
/// [`open_duckdb_store`] does.
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
fn rebuild_duckdb_projection(store: Store, report: ReadReport) -> Option<DuckdbStoreHandle> {
    if let Some(delay) = test_duckdb_rebuild_delay() {
        thread::sleep(delay);
    }

    match store.replace_from_event_log(report) {
        Ok(report) => {
            log_skipped_record_summary(&report);
            if report.skipped != 0 {
                return None;
            }
            eprintln!(
                "horizon-agentd: DuckDB projection rebuilt ({} record(s))",
                report.applied
            );
            Some(DuckdbStoreHandle::new(store))
        }
        Err(error) => {
            eprintln!(
                "horizon-agentd: DuckDB projection rebuild failed ({error}); live projection \
                 disabled for this run"
            );
            None
        }
    }
}

/// One combined stderr line -- a no-op when nothing was skipped -- for
/// records a rebuild or incremental catch-up could not project at all and
/// dropped
/// (`import::Store::apply_chunk`). Naming the first failure's message is
/// what distinguishes "one odd record in a long log" from a store-level
/// problem that skipped everything. Either outcome keeps the projection
/// unavailable; a partial rebuild must never answer history queries.
fn log_skipped_record_summary(report: &ApplyRecordsReport) {
    if report.skipped == 0 {
        return;
    }
    let cause = report
        .first_skip_error
        .as_deref()
        .unwrap_or("no error recorded");
    eprintln!(
        "horizon-agent: skipped {} unprojectable event log record(s) during the DuckDB \
         projection; first failure: {cause}",
        report.skipped
    );
}

/// Test-only hook, mirroring `horizon-agentd::main`'s own resume-delay hook
/// (`TEST_RESUME_DELAY_MS_VAR`): when set, artificially delays this thread's
/// full DuckDB rebuild (which runs *after* [`WriterInit::Ready`] has already
/// been sent -- see [`WriterHandle::open_inner`]) so a test can prove the
/// rebuild never sits on the readiness path `session_list`/`session_new`
/// block on. Never set outside `horizon-agentd`'s own e2e tests.
const TEST_DUCKDB_REBUILD_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_DUCKDB_REBUILD_DELAY_MS";

fn test_duckdb_rebuild_delay() -> Option<Duration> {
    std::env::var(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Encode and flush the complete JSONL record before updating derived state.
/// Returns how many bytes the line took, so the caller can track where the
/// next one starts.
fn append_jsonl_record(writer: &mut impl Write, record: &Record) -> Result<u64> {
    let mut line = serde_json::to_vec(record).context("encode record")?;
    line.push(b'\n');
    writer.write_all(&line).context("write record")?;
    writer.flush().context("flush record")?;
    Ok(line.len() as u64)
}

/// A failed write may have left a partial record. Keep that failure until
/// restart: another successful flush cannot establish a complete history.
fn retain_write_failure<T>(result: &Result<T>, failure: &mut Option<String>) {
    if failure.is_none() {
        if let Err(error) = result {
            let message = format!("{error:#}");
            eprintln!("horizon agent event log: writer stopped after {message}");
            *failure = Some(message);
        }
    }
}

/// Drains `rx` for the lifetime of the writer, assigning each `Append`
/// command the next sequence number in `next_sequence` (seeded by
/// [`start_up`] from the startup read) — see the "Ordering guarantee"
/// section on [`WriterHandle::open`] for why this single-threaded loop is
/// the only place that assignment ever happens.
///
/// `duckdb_store`, when `Some` (only for a `horizon-agentd` writer opened
/// via [`WriterHandle::open_silently`] with a DuckDB path -- see
/// [`WriterHandle::open_inner`]), is locked and projected into right after
/// each record's own JSONL line is durably written, keeping the projection
/// live. `log_offset` (the file length when this loop starts) advances by
/// each written line, so every projected record commits its own log cursor.
/// The lock is only ever briefly held (one row's worth of inserts);
/// write volume is low enough (roughly 1-2 events/s) that contention with
/// another locker of the same handle is normally brief. The first projection
/// failure disables writes and queries on every clone until restart; JSONL
/// appends continue independently.
fn run_writer(
    file: impl Write,
    path: &Path,
    rx: Receiver<AgentEventLogWriterCommand>,
    mut next_sequence: u64,
    mut log_offset: u64,
    duckdb_store: Option<DuckdbStoreHandle>,
    health: WriterHealth,
) {
    let mut writer = BufWriter::new(file);
    let mut failure = None;

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(mut record) => {
                if failure.is_some() {
                    continue;
                }
                record.sequence = next_sequence;
                next_sequence += 1;
                // A parked session must not depend on future traffic to flush
                // its last event. This reaches the OS page cache, not fsync.
                let result = append_jsonl_record(&mut writer, &record)
                    .with_context(|| format!("append agent event log {}", path.display()));
                retain_write_failure(&result, &mut failure);
                if let Some(message) = &failure {
                    health.fail(message.clone());
                }
                let Ok(written) = result else {
                    continue;
                };
                log_offset += written;
                if let Some(store) = &duckdb_store {
                    let cursor = LogCursor {
                        offset: log_offset,
                        sequence: record.sequence,
                        event_id: record.event_id.clone(),
                    };
                    store.project(&record, Some(&cursor));
                }
            }
            AgentEventLogWriterCommand::Flush(reply) => {
                let result = match &failure {
                    Some(message) => Err(anyhow::anyhow!("{message}")),
                    None => writer
                        .flush()
                        .with_context(|| format!("flush agent event log {}", path.display())),
                };
                retain_write_failure(&result, &mut failure);
                if let Some(message) = &failure {
                    health.fail(message.clone());
                }
                let _ = reply.send(result);
            }
        }
    }

    if failure.is_none() {
        let result = writer.flush().context("flush closing agent event log");
        retain_write_failure(&result, &mut failure);
        if let Some(message) = &failure {
            health.fail(message.clone());
        }
    }
    // Do not let BufWriter::drop silently retry bytes after an uncertain write.
    let _ = writer.into_parts();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use uuid::Uuid;

    use super::*;
    use crate::contract::SessionId;
    use crate::contract::{Event, SessionState};

    pub(super) fn record_at(session_id: SessionId, sequence: u64) -> Record {
        Record {
            schema: super::super::AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: super::super::AGENT_EVENT_LOG_VERSION,
            event_id: Uuid::new_v4().to_string(),
            sequence,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            session_context: None,
            event_kind: "state_changed".to_string(),
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            created_at_unix_ms: sequence + 1,
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
    pub(super) fn message_record(session_id: SessionId, sequence: u64, text: &str) -> Record {
        Record {
            event_kind: "message_committed".to_string(),
            event: Event::MessageCommitted(crate::contract::Message {
                role: crate::contract::MessageRole::User,
                text: text.to_string(),
            }),
            ..record_at(session_id, sequence)
        }
    }

    fn assert_restart_discards_partial_tail(prefix: &[u8], tail: &[u8], next_sequence: u64) {
        let path =
            std::env::temp_dir().join(format!("horizon-agent-tail-{}.jsonl", Uuid::new_v4()));
        let mut contents = prefix.to_vec();
        contents.extend_from_slice(tail);
        std::fs::write(&path, contents).unwrap();
        let before = read(&path).expect("an unfinished tail is not a record");
        assert!(before.ignored_partial_line);
        let (writer, init_rx) = WriterHandle::open(&path);
        assert!(matches!(init_rx.recv().unwrap(), WriterInit::Ready(_)));
        let appended = record_at(SessionId::new(), 999);
        writer.append(appended.clone()).unwrap();
        writer.flush().unwrap();
        let after = read(&path).unwrap();
        assert_eq!(after.records.len(), before.records.len() + 1);
        assert_eq!(after.corrupt_line_count, before.corrupt_line_count);
        assert!(!after.ignored_partial_line);
        let last = after.records.last().unwrap();
        assert_eq!(last.event_id, appended.event_id);
        assert_eq!(last.sequence, next_sequence);
        assert!(std::fs::read(&path).unwrap().starts_with(prefix));
        drop(writer);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn restart_preserves_complete_lines_and_discards_a_long_partial_tail() {
        let record = serde_json::to_string(&record_at(SessionId::new(), 4)).unwrap();
        let prefix = format!("broken complete line\n{record}\n");
        assert_restart_discards_partial_tail(prefix.as_bytes(), &vec![b'x'; 20_000], 5);
    }

    #[test]
    fn restart_does_not_promote_an_unterminated_valid_record() {
        let tail = serde_json::to_vec(&record_at(SessionId::new(), 99)).unwrap();
        assert_restart_discards_partial_tail(b"", &tail, 0);
    }

    #[test]
    fn restart_discards_a_tail_split_inside_a_utf8_character() {
        let record = serde_json::to_string(&record_at(SessionId::new(), 0)).unwrap();
        assert_restart_discards_partial_tail(
            format!("{record}\n").as_bytes(),
            b"{\"text\":\"\xe6\x97",
            1,
        );
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

    struct FailOnce {
        on_flush: bool,
        failed: bool,
    }

    impl Write for FailOnce {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.on_flush && !self.failed {
                self.failed = true;
                return Err(std::io::Error::other("injected write failure"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.on_flush && !self.failed {
                self.failed = true;
                return Err(std::io::Error::other("injected flush failure"));
            }
            Ok(())
        }
    }

    fn assert_failed_append_stays_failed(on_flush: bool) {
        let store = DuckdbStoreHandle::new(Store::open_in_memory().unwrap());
        let projected = store.clone();
        let (tx, rx) = unbounded();
        let health = WriterHealth::default();
        let worker_health = health.clone();
        let writer = WriterHandle { tx, health };
        let worker = thread::spawn(move || {
            run_writer(
                FailOnce {
                    on_flush,
                    failed: false,
                },
                Path::new("injected.jsonl"),
                rx,
                0,
                0,
                Some(projected),
                worker_health,
            );
        });
        let session = SessionId::new();
        writer.append(record_at(session, 0)).unwrap();
        let first = writer.flush();
        assert!(writer.append(record_at(session, 1)).is_err());
        let second = writer.flush();
        drop(writer);
        worker.join().unwrap();
        assert!(
            first.is_err(),
            "a later flush must report the failed append"
        );
        assert!(
            second.is_err(),
            "the writer must not recover across an uncertain record boundary"
        );
        assert_eq!(
            store.lock().unwrap().max_last_sequence().unwrap(),
            None,
            "failed JSONL writes must not reach the derived projection"
        );
    }

    #[test]
    fn append_write_failure_is_retained_by_subsequent_flushes() {
        assert_failed_append_stays_failed(false);
    }

    #[test]
    fn append_flush_failure_is_retained_by_subsequent_flushes() {
        assert_failed_append_stays_failed(true);
    }

    #[test]
    fn failed_durable_input_does_not_enter_live_history() {
        use crate::contract::SessionInput;
        use crate::live::LiveState;

        for on_flush in [false, true] {
            let (tx, rx) = unbounded();
            let health = WriterHealth::default();
            let worker_health = health.clone();
            let worker = thread::spawn(move || {
                run_writer(
                    FailOnce {
                        on_flush,
                        failed: false,
                    },
                    Path::new("injected-failure"),
                    rx,
                    0,
                    0,
                    None,
                    worker_health,
                );
            });
            let writer = WriterHandle { tx, health };
            let history = vec![Event::StateChanged(SessionState::WaitingForUser)];
            let live = LiveState::with_event_log_and_history(
                SessionId::new(),
                None,
                None,
                writer.clone(),
                history.clone(),
            );
            let frame = live.frame();
            let result = live.persist_provider_events([Event::InputAccepted(SessionInput {
                id: "retryable-input".into(),
                origin: "owner".into(),
                text: "work".into(),
                reply_to: None,
                resume_work: false,
            })
            .into()]);
            assert!(result.is_err(), "an enqueue is not a durable acceptance");
            assert_eq!(
                live.events(),
                history,
                "failed input must not suppress redelivery"
            );
            assert_eq!(live.frame(), frame);
            assert!(writer.flush().is_err());
            drop(live);
            drop(writer);
            worker.join().unwrap();
        }
    }

    // --- startup modes: full rebuild vs. cursor-verified tail -------------

    /// One `open_silently` start against `log`/`db`, waiting for both of its
    /// outcomes.
    pub(super) struct Boot {
        pub(super) writer: WriterHandle,
        pub(super) report: ReadReport,
        pub(super) store: Option<DuckdbStoreHandle>,
    }

    pub(super) fn boot(log: &Path, db: &Path) -> Boot {
        let (writer, init_rx, duckdb_rx) = WriterHandle::open_silently(log, Some(db.to_path_buf()));
        let report = match init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(report) => report,
            WriterInit::Failed(error) => panic!("writer startup failed: {error:#}"),
        };
        let store = duckdb_rx.recv().ok().flatten();
        Boot {
            writer,
            report,
            store,
        }
    }

    /// Stops the writer thread and waits until it has exited, so its store
    /// is closed before the next boot opens the same file (two instances of
    /// one DuckDB file in a process are unsound -- see
    /// `rebuild_duckdb_projection`).
    pub(super) fn shut_down(boot: Boot) {
        let exited = boot.writer.subscribe_failure();
        drop(boot.store);
        drop(boot.writer);
        exited
            .receiver()
            .recv_timeout(Duration::from_secs(10))
            .expect("writer thread exits");
    }

    pub(super) fn write_log(path: &Path, records: &[Record]) {
        let mut contents = Vec::new();
        for record in records {
            serde_json::to_writer(&mut contents, record).unwrap();
            contents.push(b'\n');
        }
        std::fs::write(path, contents).unwrap();
    }

    fn append_raw(path: &Path, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn cursor_of(store: &DuckdbStoreHandle) -> Option<LogCursor> {
        store.query(|store| store.log_cursor()).unwrap()
    }

    fn texts(store: &DuckdbStoreHandle, session: SessionId) -> Vec<String> {
        store
            .query(|store| store.messages_for_session(session))
            .unwrap()
            .into_iter()
            .map(|message| message.text)
            .collect()
    }

    fn file_len(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    #[test]
    fn full_rebuild_commits_the_cursor_of_the_last_record() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        let records: Vec<_> = (0..3)
            .map(|seq| message_record(session, seq, &format!("m{seq}")))
            .collect();
        write_log(&log, &records);

        let boot = boot(&log, &db);
        assert_eq!(boot.report.read_after, None, "no cursor yet: full mode");
        assert_eq!(boot.report.records.len(), 3);
        let store = boot.store.clone().expect("rebuilt store");
        assert_eq!(
            cursor_of(&store),
            Some(LogCursor {
                offset: file_len(&log),
                sequence: 2,
                event_id: records[2].event_id.clone(),
            })
        );
        assert_eq!(cursor_of(&store), boot.report.last_record_cursor());
        assert!(store.query(|store| store.has_current_format()).unwrap());
        shut_down(boot);
    }

    #[test]
    fn a_clean_restart_reads_only_the_tail_and_catches_it_up_before_ready() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        let first: Vec<_> = (0..2)
            .map(|seq| message_record(session, seq, &format!("m{seq}")))
            .collect();
        write_log(&log, &first);
        let first_boot = boot(&log, &db);
        let first_cursor = cursor_of(first_boot.store.as_ref().unwrap()).unwrap();
        shut_down(first_boot);

        let second = message_record(session, 2, "m2");
        append_raw(
            &log,
            format!("{}\n", serde_json::to_string(&second).unwrap()).as_bytes(),
        );

        let second_boot = boot(&log, &db);
        assert_eq!(second_boot.report.read_after, Some(first_cursor));
        assert_eq!(second_boot.report.records, vec![second.clone()]);
        let store = second_boot.store.clone().expect("caught-up store");
        assert_eq!(texts(&store, session), ["m0", "m1", "m2"]);
        assert_eq!(
            cursor_of(&store),
            Some(LogCursor {
                offset: file_len(&log),
                sequence: 2,
                event_id: second.event_id.clone(),
            })
        );

        // Sequences continue past the tail.
        second_boot.writer.append(record_at(session, 999)).unwrap();
        second_boot.writer.flush().unwrap();
        assert_eq!(read(&log).unwrap().records.last().unwrap().sequence, 3);
        shut_down(second_boot);
    }

    #[test]
    fn a_live_append_commits_its_own_cursor_and_the_next_start_is_already_current() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "seed")]);

        let first_boot = boot(&log, &db);
        first_boot
            .writer
            .append(message_record(session, 999, "live"))
            .unwrap();
        first_boot.writer.flush().unwrap();
        let appended = read(&log).unwrap().records.pop().unwrap();
        let store = first_boot.store.clone().unwrap();
        assert_eq!(
            cursor_of(&store),
            Some(LogCursor {
                offset: file_len(&log),
                sequence: 1,
                event_id: appended.event_id,
            })
        );
        let updated_at = store.query(|store| store.sessions()).unwrap()[0]
            .updated_at
            .clone();
        drop(store);
        shut_down(first_boot);

        thread::sleep(Duration::from_millis(20));
        let second_boot = boot(&log, &db);
        assert!(second_boot.report.read_after.is_some());
        assert!(second_boot.report.records.is_empty());
        let store = second_boot.store.clone().unwrap();
        assert_eq!(texts(&store, session), ["seed", "live"]);
        assert_eq!(
            store.query(|store| store.sessions()).unwrap()[0].updated_at,
            updated_at,
            "an unchanged log must not touch the projection at all"
        );
        drop(store);
        shut_down(second_boot);
    }

    #[test]
    fn a_different_line_at_the_cursor_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "original")]);
        shut_down(boot(&log, &db));

        // Same sequence and line length, different identity.
        write_log(&log, &[message_record(session, 0, "replaced")]);
        let boot = boot(&log, &db);
        assert_eq!(boot.report.read_after, None);
        assert_eq!(texts(boot.store.as_ref().unwrap(), session), ["replaced"]);
        shut_down(boot);
    }

    #[test]
    fn a_log_shorter_than_the_cursor_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        let records = vec![
            message_record(session, 0, "a"),
            message_record(session, 1, "b"),
        ];
        write_log(&log, &records);
        shut_down(boot(&log, &db));

        write_log(&log, &records[..1]);
        let boot = boot(&log, &db);
        assert_eq!(boot.report.read_after, None);
        assert_eq!(
            texts(boot.store.as_ref().unwrap(), session),
            ["a"],
            "the rebuild must reflect only what the log still has"
        );
        shut_down(boot);
    }

    #[test]
    fn a_missing_cursor_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "a")]);
        shut_down(boot(&log, &db));
        duckdb::Connection::open(&db)
            .unwrap()
            .execute_batch("DELETE FROM agent_log_cursor;")
            .unwrap();

        let boot = boot(&log, &db);
        assert_eq!(boot.report.read_after, None);
        assert!(cursor_of(boot.store.as_ref().unwrap()).is_some());
        shut_down(boot);
    }

    #[test]
    fn an_old_projection_format_rebuilds_even_when_the_cursor_matches() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 10, "converted history")]);
        shut_down(boot(&log, &db));
        for stamp in [None, Some((2, 2)), Some((4, 1))] {
            {
                let conn = duckdb::Connection::open(&db).unwrap();
                conn.execute_batch(
                    "DELETE FROM agent_projection_format;
                    UPDATE agent_messages SET text = 'old cached history';",
                )
                .unwrap();
                if let Some((event_log_version, schema_version)) = stamp {
                    conn.execute(
                        "INSERT INTO agent_projection_format VALUES (true, ?, ?)",
                        [event_log_version, schema_version],
                    )
                    .unwrap();
                }
            }
            let boot = boot(&log, &db);
            assert_eq!(boot.report.read_after, None);
            assert_eq!(
                texts(boot.store.as_ref().unwrap(), session),
                ["converted history"]
            );
            shut_down(boot);
        }
    }

    /// The table shape before the cursor and the stored session context
    /// existed: the format table has no schema-version column, and
    /// `agent_sessions` has no `session_context_json`.
    const PRE_CURSOR_SCHEMA_SQL: &str = "
        CREATE TABLE agent_projection_format (
            singleton BOOLEAN PRIMARY KEY CHECK (singleton),
            event_log_version INTEGER NOT NULL
        );
        INSERT INTO agent_projection_format VALUES (true, 4);
        CREATE TABLE agent_sessions (
            session_id TEXT PRIMARY KEY,
            provider_id TEXT,
            role_id TEXT,
            last_sequence BIGINT NOT NULL DEFAULT -1,
            updated_at TIMESTAMP NOT NULL DEFAULT now()
        );
    ";

    #[test]
    fn a_projection_with_the_pre_cursor_schema_is_rebuilt_with_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        duckdb::Connection::open(&db)
            .unwrap()
            .execute_batch(PRE_CURSOR_SCHEMA_SQL)
            .unwrap();
        let session = SessionId::new();
        let context = crate::persistence::event_log::PersistedSessionContext {
            workspace_root: Some(PathBuf::from("/tmp/old-schema-root")),
            isolated_worktree: false,
            parent_session_id: None,
            filesystem_grants: Vec::new(),
        };
        let mut record = message_record(session, 0, "a");
        record.session_context = Some(context.clone());
        write_log(&log, &[record]);

        let first_boot = boot(&log, &db);
        assert_eq!(first_boot.report.read_after, None);
        let store = first_boot.store.clone().expect("rebuilt store");
        assert!(store.query(|store| store.has_current_format()).unwrap());
        assert_eq!(
            store.session_records(session, 0).unwrap()[0].session_context,
            Some(context),
            "the rebuilt schema must carry the new column, populated"
        );
        drop(store);
        shut_down(first_boot);

        let second_boot = boot(&log, &db);
        assert!(second_boot.report.read_after.is_some());
        shut_down(second_boot);
    }

    #[test]
    fn an_undecodable_tail_line_still_advances_the_next_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "a")]);
        shut_down(boot(&log, &db));
        let mut undecodable = serde_json::to_value(record_at(session, 50)).unwrap();
        undecodable["event"] = serde_json::json!({"NotARealVariant": {}});
        append_raw(&log, format!("{undecodable}\n").as_bytes());

        let boot = boot(&log, &db);
        assert!(boot.report.read_after.is_some());
        assert!(boot.report.records.is_empty());
        assert_eq!(boot.report.skipped_event_count, 1);
        assert_eq!(boot.report.max_known_sequence, Some(50));
        boot.writer.append(record_at(session, 999)).unwrap();
        boot.writer.flush().unwrap();
        assert_eq!(read(&log).unwrap().records.last().unwrap().sequence, 51);
        shut_down(boot);
    }

    #[test]
    fn a_torn_line_after_the_cursor_is_discarded_and_the_cursor_stays_valid() {
        let dir = tempfile::tempdir().unwrap();
        let (log, db) = (
            dir.path().join("events.jsonl"),
            dir.path().join("state.duckdb"),
        );
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "a")]);
        shut_down(boot(&log, &db));
        let intact_len = file_len(&log);
        append_raw(&log, b"{\"schema\":\"horizon.agent.event_log\",\"ver");

        let torn_boot = boot(&log, &db);
        assert!(torn_boot.report.read_after.is_some());
        assert!(torn_boot.report.ignored_partial_line);
        assert_eq!(file_len(&log), intact_len, "the torn bytes are discarded");
        torn_boot
            .writer
            .append(message_record(session, 999, "b"))
            .unwrap();
        torn_boot.writer.flush().unwrap();
        shut_down(torn_boot);

        let next_boot = boot(&log, &db);
        assert!(next_boot.report.read_after.is_some());
        assert!(next_boot.report.records.is_empty());
        assert_eq!(
            texts(next_boot.store.as_ref().unwrap(), session),
            ["a", "b"]
        );
        shut_down(next_boot);
    }

    #[test]
    fn an_unopenable_projection_falls_back_to_the_full_read_without_a_store() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let db = dir.path().join("not-a-database.duckdb");
        std::fs::write(&db, b"this is not a DuckDB file").unwrap();
        let session = SessionId::new();
        write_log(&log, &[message_record(session, 0, "a")]);

        let boot = boot(&log, &db);
        assert_eq!(boot.report.read_after, None);
        assert_eq!(boot.report.records.len(), 1);
        assert!(boot.store.is_none());
        boot.writer.append(record_at(session, 999)).unwrap();
        boot.writer.flush().unwrap();
        assert_eq!(read(&log).unwrap().records.len(), 2);
        shut_down(boot);
    }
}

#[cfg(test)]
#[path = "writer_contract_tests.rs"]
mod contract_tests;
