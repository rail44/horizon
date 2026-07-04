use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use super::{read, Record};

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
pub(crate) struct WriterHandle {
    tx: Sender<AgentEventLogWriterCommand>,
    next_sequence: Arc<Mutex<u64>>,
}

impl WriterHandle {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("create agent event log directory {}", parent.display())
                })?;
            }
        }

        let report = read(path)?;
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
        let (tx, rx) = unbounded();
        let writer_path = path.to_path_buf();
        thread::spawn(move || run_writer(writer_path, rx));

        Ok(Self {
            tx,
            next_sequence: Arc::new(Mutex::new(next_sequence)),
        })
    }

    pub(crate) fn append(&self, mut record: Record) -> Result<()> {
        {
            let mut next_sequence = self
                .next_sequence
                .lock()
                .map_err(|_| anyhow::anyhow!("agent event log sequence lock poisoned"))?;
            record.sequence = *next_sequence;
            *next_sequence += 1;
        }
        self.tx
            .send(AgentEventLogWriterCommand::Append(Box::new(record)))
            .context("enqueue agent event log record")
    }

    /// Blocks until every record enqueued before this call has been handed
    /// to `serde_json` and the underlying `BufWriter` has been flushed to
    /// the OS. Because the writer thread processes its channel strictly in
    /// order, a reply on the returned channel guarantees everything sent
    /// via [`Self::append`] beforehand is now durable on disk (modulo the
    /// OS's own page cache — this is not an `fsync`).
    ///
    /// Used by tests to assert durability deterministically, and by
    /// `app::shutdown` (wired to floem's `AppEvent::WillTerminate`) so a
    /// normal app exit doesn't lose whatever is still sitting in the
    /// writer's buffer. A hard kill bypasses this and can still leave a
    /// torn final line — `event_log::read` tolerates that (see
    /// `ReadReport::ignored_partial_line`).
    pub(crate) fn flush(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(AgentEventLogWriterCommand::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }

    /// Test-only identity check: do these two handles share the same
    /// background writer thread? Used to assert that the process-global
    /// cache in `app::runtime::agent` really does hand out one shared
    /// writer instead of silently creating a second one.
    #[cfg(test)]
    pub(crate) fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

enum AgentEventLogWriterCommand {
    Append(Box<Record>),
    Flush(Sender<Result<()>>),
}

fn run_writer(path: PathBuf, rx: Receiver<AgentEventLogWriterCommand>) {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let mut writer = BufWriter::new(file);

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(record) => {
                if serde_json::to_writer(&mut writer, &record).is_ok() {
                    let _ = writer.write_all(b"\n");
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
