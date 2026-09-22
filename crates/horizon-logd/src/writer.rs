//! The board write path, moved from `horizon-board`'s `Store` to logd
//! (`docs/logd-design.md` v1). The exclusive-flock + read-fold + id/rank
//! computation + append sequence is unchanged; it just lives in the daemon
//! now instead of in each short-lived client process.

mod messages;
mod prepare;
mod relationships;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use horizon_board::wire::{IngestReply, IngestRequest, LogError};
use horizon_board::{read_events, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};

/// Advisory exclusive lock via `flock(2)`. Held until the file is dropped
/// (the kernel releases it on close). Used across the read-fold-append
/// sequence so concurrent clients serialise on the same events file.
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn make_envelope(event: BoardEvent) -> Envelope {
    Envelope {
        schema: SCHEMA.to_string(),
        version: VERSION,
        at: unix_ms(),
        event,
    }
}

/// Opens the file for writing (create + append), acquires an exclusive lock,
/// and reads the current event log. The lock is held until the returned
/// `File` is dropped.
fn open_locked(path: &Path) -> Result<(File, ReadReport), LogError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_err)?;
    lock_exclusive(&file).map_err(io_err)?;
    let report = read_events(path).map_err(io_err)?;
    Ok((file, report))
}

fn append(file: &mut File, env: &Envelope) -> Result<(), LogError> {
    serde_json::to_writer(&mut *file, env).map_err(json_err)?;
    file.write_all(b"\n").map_err(io_err)?;
    file.flush().map_err(io_err)?;
    Ok(())
}

fn io_err(e: std::io::Error) -> LogError {
    LogError::Io(e.to_string())
}

fn json_err(e: serde_json::Error) -> LogError {
    LogError::Io(e.to_string())
}

fn invalid(text: &str) -> LogError {
    LogError::InvalidOperation(text.into())
}

/// Serializes validation and append under the board's exclusive file lock.
pub fn perform(path: &Path, request: IngestRequest) -> Result<(IngestReply, Vec<u64>), LogError> {
    let (mut file, report) = open_locked(path)?;
    if report.corrupt_count > 0 || report.skipped_count > 0 || report.torn_trailing {
        return Err(invalid("Board contains unreadable or legacy records; import or repair an isolated copy before writing"));
    }
    let prepared = prepare::prepare(&report, request)?;
    let sequences = if let Some(event) = prepared.event {
        append(&mut file, &make_envelope(event))?;
        vec![report.line_count + 1]
    } else {
        vec![]
    };
    Ok((prepared.reply, sequences))
}
