//! The event file source: opening `events.jsonl`, taking the shared lock,
//! and handing the text to the tolerant decoder.
//!
//! Reads need no daemon. JSONL is world-readable, a single writer (logd)
//! plus atomic appends make direct reads safe, and boards have no
//! projection — so a read is a locked fold of the file.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::event::{self, ReadReport};
use crate::store::types::StoreError;

/// Advisory shared lock via `flock(2)`. Reads use this so a concurrent writer
/// (logd, with its exclusive lock) does not starve readers.
fn lock_shared(file: &File) -> std::io::Result<()> {
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Releases any advisory lock on `file`.
fn unlock(file: &File) {
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

/// Reads and tolerantly decodes the event log. Returns an empty report
/// when the file doesn't exist yet (first invocation).
pub fn read(path: &Path) -> std::io::Result<ReadReport> {
    if !path.exists() {
        return Ok(ReadReport::default());
    }
    Ok(event::read_text(&std::fs::read_to_string(path)?))
}

/// [`read`], with a shared lock held across it.
pub(crate) fn read_locked(path: &Path) -> Result<ReadReport, StoreError> {
    if !path.exists() {
        return Ok(ReadReport::default());
    }
    let file = File::open(path)?;
    lock_shared(&file)?;
    let report = read(path)?;
    unlock(&file);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{SCHEMA, VERSION};

    #[test]
    fn reader_keeps_unknown_event_high_water_and_physical_positions() {
        let path = std::env::temp_dir().join(format!(
            "horizon-board-reader-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let good = serde_json::json!({"schema":SCHEMA,"version":VERSION,"at":0,"type":"import-high-water","id":3});
        let unknown = serde_json::json!({"schema":SCHEMA,"version":VERSION,"at":0,"type":"future-record","id":77});
        std::fs::write(
            &path,
            format!("{good}\n{unknown}\nnot-json\n{good}\n{{torn"),
        )
        .unwrap();
        let report = read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(report.max_id, Some(77));
        assert_eq!(report.sequences, vec![1, 4]);
        assert_eq!(report.skipped_count, 1);
        assert_eq!(report.corrupt_count, 1);
        assert!(report.torn_trailing);
    }
}
