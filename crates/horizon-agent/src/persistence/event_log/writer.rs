use std::{
    io::{BufWriter, Write},
    path::Path,
    thread,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

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
        let path = path.as_ref().to_path_buf();
        let (tx, rx) = unbounded();
        let (init_tx, init_rx) = unbounded();

        thread::spawn(move || match start_up(&path, reader) {
            Ok((file, report, next_sequence)) => {
                let _ = init_tx.send(WriterInit::Ready(report));
                run_writer(file, &path, rx, next_sequence);
            }
            Err(error) => {
                let _ = init_tx.send(WriterInit::Failed(error));
                // No writer loop: dropping `rx` here (it's only captured by
                // this closure) makes every `append`/`flush` sent from now
                // on fail fast with a disconnected-channel error instead of
                // queuing forever with nothing to drain it.
            }
        });

        (Self { tx }, init_rx)
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
/// never on the caller of `open`.
fn start_up(
    path: &Path,
    reader: impl FnOnce(&Path) -> Result<ReadReport>,
) -> Result<(std::fs::File, ReadReport, u64)> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create agent event log directory {}", parent.display())
            })?;
        }
    }

    let report = reader(path)?;
    if let Some(summary) = report.skipped_summary() {
        eprintln!(
            "horizon agent event log: {summary} while opening {}",
            path.display()
        );
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

/// Drains `rx` for the lifetime of the writer, assigning each `Append`
/// command the next sequence number in `next_sequence` (seeded by
/// [`start_up`] from the startup read) — see the "Ordering guarantee"
/// section on [`WriterHandle::open`] for why this single-threaded loop is
/// the only place that assignment ever happens.
fn run_writer(
    file: std::fs::File,
    path: &Path,
    rx: Receiver<AgentEventLogWriterCommand>,
    mut next_sequence: u64,
) {
    let mut writer = BufWriter::new(file);

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(mut record) => {
                record.sequence = next_sequence;
                next_sequence += 1;
                if serde_json::to_writer(&mut writer, &record).is_ok() {
                    let _ = writer.write_all(b"\n");
                    // Flushed immediately (not just batched in `BufWriter`'s
                    // in-memory buffer) so a hard kill (SIGKILL, crash --
                    // `horizon-agentd` has no signal handler for it and runs
                    // no destructors) can only ever lose events that hadn't
                    // arrived on this channel yet, never ones already
                    // appended. This is what makes `docs/agent-runtime-
                    // split-design.md` step 4's "agentd restart: read own
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
            event_kind: "state_changed".to_string(),
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            created_at_unix_ms: sequence + 1,
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
}
