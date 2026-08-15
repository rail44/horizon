//! Cursor persistence: the wake subscriber's last-processed board event seq,
//! stored as a plain text file next to the board's `events.jsonl`.
//!
//! The cursor is a single `u64` — the 1-based line number of the last event
//! the subscriber has examined. On daemon restart, the subscriber loads this
//! value and re-subscribes from `since = Some(cursor)`, catching up on
//! anything that happened while the daemon was down.
//!
//! The file lives at `<board-dir>/wake-cursor`, sibling to `events.jsonl`,
//! so it tracks the same board and moves with it. A missing file means
//! "start from the beginning" (cursor 0). Writes are atomic
//! (write-to-temp-then-rename) so a crash mid-write never leaves a truncated
//! value.

use std::io;
use std::path::{Path, PathBuf};

/// The cursor file's name, sibling to `events.jsonl`.
const CURSOR_FILE_NAME: &str = "wake-cursor";

/// Resolves the cursor file path from the board store's events path.
/// Returns `None` if the events path has no parent (shouldn't happen in
/// practice, but defensive).
pub(crate) fn cursor_path(events_path: &Path) -> Option<PathBuf> {
    events_path.parent().map(|dir| dir.join(CURSOR_FILE_NAME))
}

/// Loads the persisted cursor. Returns `0` if the file doesn't exist (first
/// run) or is unreadable — the subscriber starts from the beginning, which is
/// safe (at worst it re-processes already-seen events, which the policy's
/// idempotent cursor check skips).
pub(crate) fn load(cursor_path: &Path) -> u64 {
    match std::fs::read_to_string(cursor_path) {
        Ok(text) => text.trim().parse::<u64>().unwrap_or(0),
        Err(_) => 0,
    }
}

/// Persists the cursor atomically (write-to-temp-then-rename). A failure is
/// logged to stderr but does not propagate — a stale cursor only means the
/// subscriber re-processes some events on restart, which the policy handles
/// idempotently.
pub(crate) fn save(cursor_path: &Path, cursor: u64) {
    if let Some(dir) = cursor_path.parent() {
        if let Err(e) = write_atomic(cursor_path, dir, cursor) {
            eprintln!("horizon-agentd: failed to persist wake cursor: {e}");
        }
    }
}

/// Writes `cursor` to a temp file in `dir`, then renames it to `path`.
fn write_atomic(path: &Path, dir: &Path, cursor: u64) -> io::Result<()> {
    let tmp = dir.join(format!(".{CURSOR_FILE_NAME}.tmp"));
    std::fs::write(&tmp, cursor.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_zero_for_missing_file() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-cursor-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join(CURSOR_FILE_NAME);
        assert_eq!(load(&path), 0);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-cursor-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CURSOR_FILE_NAME);

        save(&path, 42);
        assert_eq!(load(&path), 42);

        save(&path, 100);
        assert_eq!(load(&path), 100);
    }

    #[test]
    fn load_returns_zero_for_corrupt_file() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-cursor-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CURSOR_FILE_NAME);
        std::fs::write(&path, "not a number").unwrap();
        assert_eq!(load(&path), 0);
    }

    #[test]
    fn cursor_path_is_sibling_of_events() {
        let events = Path::new("/data/horizon/board/-home-proj/events.jsonl");
        let cursor = cursor_path(events).unwrap();
        assert_eq!(
            cursor,
            Path::new("/data/horizon/board/-home-proj/wake-cursor")
        );
    }
}
