use std::{
    collections::HashMap,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::contract::SessionId;

use super::{
    Record,
    health::{WriterExit, WriterHealth},
    sessions::{last_sequence, session_path},
};

/// The writer of a directory of session logs (`sessions`): one background
/// thread that takes every record from one channel, in the order sent,
/// and appends it to its session's file. One process writes a directory
/// (the host holds it alone), and within the process every session shares
/// this one handle, so a file is never written by two writers at once.
///
/// A record's `sequence` is assigned here, one past the last in its
/// session's file: the thread is the only place a sequence is assigned, so
/// they stay unique and increasing within a session in whatever order the
/// thread takes records from the channel.
///
/// The first failure to write or flush any file stops every later write
/// (a failed write may have left a partial record) and is broadcast to
/// every session sharing the handle (`subscribe_failure`).
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<Command>,
    health: WriterHealth,
}

enum Command {
    Append(Box<Record>),
    Flush(Sender<Result<()>>),
}

impl WriterHandle {
    /// Starts the writer of the session logs in `dir`, created when the
    /// first record is written. Returns at once; files are opened by the
    /// thread as their sessions' first records arrive.
    pub fn open(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        Self::start(move |session_id| SessionFile::open(&dir, session_id))
    }

    /// [`Self::open`], with `before_first_write` run on the writer's thread
    /// before it writes anything: a test holds records in the channel,
    /// unwritten, for as long as it runs.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn open_after(
        dir: impl AsRef<Path>,
        before_first_write: impl FnOnce() + Send + 'static,
    ) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let mut before_first_write = Some(before_first_write);
        Self::start(move |session_id| {
            if let Some(before) = before_first_write.take() {
                before();
            }
            SessionFile::open(&dir, session_id)
        })
    }

    /// [`Self::open`] with the files opened by `open_file` (a test's
    /// failing file).
    fn start(open_file: impl FnMut(SessionId) -> Result<SessionFile> + Send + 'static) -> Self {
        let (tx, rx) = unbounded();
        let health = WriterHealth::default();
        let thread_health = health.clone();
        thread::spawn(move || {
            let _exit = WriterExit(thread_health.clone());
            run_writer(rx, open_file, thread_health);
        });
        Self { tx, health }
    }

    /// A writer that has already failed, for a test of what a session does
    /// when its log cannot be written.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn failed(reason: &str) -> Self {
        let writer = Self::start(|_| anyhow::bail!("this test's writer writes nothing"));
        writer.health.fail(reason.into());
        writer
    }

    pub fn failure(&self) -> Option<String> {
        self.health.failure()
    }

    pub fn subscribe_failure(&self) -> super::FailureSubscription {
        self.health.subscribe()
    }

    /// Enqueues `record` for its session's file. Its `sequence` is
    /// assigned by the writer.
    pub fn append(&self, record: Record) -> Result<()> {
        if let Some(message) = self.failure() {
            anyhow::bail!("{message}");
        }
        self.tx
            .send(Command::Append(Box::new(record)))
            .context("enqueue agent event log record")
    }

    /// Blocks until every record enqueued before this call is written and
    /// flushed to the OS (not `fsync`). A write or flush failure is
    /// returned by every later flush.
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(Command::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }
}

/// One session's open log.
struct SessionFile {
    path: PathBuf,
    writer: BufWriter<Box<dyn Write + Send>>,
    next_sequence: u64,
}

impl SessionFile {
    /// Opens (creating it and its directory) `session_id`'s file for
    /// appending, after discarding an unterminated tail: the reader
    /// ignores those bytes, and the next record must not join them.
    fn open(dir: &Path, session_id: SessionId) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create agent session log directory {}", dir.display()))?;
        let path = session_path(dir, session_id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open agent event log for writing {}", path.display()))?;
        discard_partial_tail(&mut file).with_context(|| {
            format!("discard unfinished agent event log tail {}", path.display())
        })?;
        let next_sequence = last_sequence(&path)?.map_or(0, |last| last + 1);
        Ok(Self {
            writer: BufWriter::new(Box::new(file)),
            path,
            next_sequence,
        })
    }
}

/// Removes the bytes after the file's last newline.
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

/// Encodes and flushes one complete line.
fn append_jsonl_record(writer: &mut impl Write, record: &Record) -> Result<()> {
    let mut line = serde_json::to_vec(record).context("encode record")?;
    line.push(b'\n');
    writer.write_all(&line).context("write record")?;
    writer.flush().context("flush record")
}

fn run_writer(
    rx: Receiver<Command>,
    mut open_file: impl FnMut(SessionId) -> Result<SessionFile>,
    health: WriterHealth,
) {
    let mut files: HashMap<SessionId, SessionFile> = HashMap::new();
    let mut failure: Option<String> = None;
    let fail = |error: anyhow::Error, failure: &mut Option<String>| {
        if failure.is_none() {
            let message = format!("{error:#}");
            eprintln!("horizon agent event log: writer stopped after {message}");
            health.fail(message.clone());
            *failure = Some(message);
        }
    };

    for command in rx {
        match command {
            Command::Append(mut record) => {
                if failure.is_some() || health.failure().is_some() {
                    continue;
                }
                let file = match files.entry(record.session_id) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        match open_file(record.session_id) {
                            Ok(file) => entry.insert(file),
                            Err(error) => {
                                fail(error, &mut failure);
                                continue;
                            }
                        }
                    }
                };
                record.sequence = file.next_sequence;
                // A parked session must not depend on later traffic to flush
                // its last event.
                match append_jsonl_record(&mut file.writer, &record)
                    .with_context(|| format!("append agent event log {}", file.path.display()))
                {
                    Ok(()) => file.next_sequence += 1,
                    Err(error) => fail(error, &mut failure),
                }
            }
            Command::Flush(reply) => {
                let result = match failure.clone().or_else(|| health.failure()) {
                    Some(message) => Err(anyhow::anyhow!("{message}")),
                    None => files.values_mut().try_for_each(|file| {
                        file.writer.flush().with_context(|| {
                            format!("flush agent event log {}", file.path.display())
                        })
                    }),
                };
                if let Err(error) = &result {
                    fail(anyhow::anyhow!("{error:#}"), &mut failure);
                }
                let _ = reply.send(result);
            }
        }
    }

    for file in files.values_mut() {
        if failure.is_none()
            && let Err(error) = file.writer.flush()
        {
            fail(
                anyhow::Error::from(error).context("flush closing agent event log"),
                &mut failure,
            );
        }
    }
    // Do not let BufWriter::drop retry bytes after an uncertain write.
    for (_, file) in files {
        let _ = file.writer.into_parts();
    }
}

#[cfg(test)]
mod tests;
