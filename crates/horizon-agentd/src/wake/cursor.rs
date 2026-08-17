//! Cursor persistence: the wake subscriber's last-**delivered** board event
//! seq, stored as a plain text file next to the board's `events.jsonl`.
//!
//! The cursor is a single `u64` — the 1-based line number of the last event
//! whose wake-triggered turn has **ended** (delivery completion). On daemon
//! restart, the subscriber loads this value and re-subscribes from
//! `since = Some(cursor)`, re-reading any events that were accumulated but
//! not yet delivered — so a restart mid-accumulate does not lose them.
//!
//! The file also persists the last keeper's author string
//! (`session:<uuid>`), so the self-author filter survives a restart: the
//! last keeper's own past comments are filtered during re-read instead of
//! waking a fresh turn (the None-window fix, board #40/#41 residual).
//!
//! ## File format
//!
//! Two lines:
//!
//! ```text
//! <cursor>
//! <keeper_author>
//! ```
//!
//! Line 2 is empty when no keeper has run yet. Legacy files (a single bare
//! number) load with `keeper_author = None`.
//!
//! The file lives at `<board-dir>/wake-cursor`, sibling to `events.jsonl`,
//! so it tracks the same board and moves with it. A missing file means
//! "start from the beginning" (cursor 0, no keeper author). Writes are
//! atomic (write-to-temp-then-rename) so a crash mid-write never leaves a
//! truncated value.

use std::io;
use std::path::{Path, PathBuf};

/// The cursor file's name, sibling to `events.jsonl`.
const CURSOR_FILE_NAME: &str = "wake-cursor";

/// The persisted wake-subscriber state: the last-delivered seq and the last
/// keeper's author string.
pub(crate) struct CursorState {
    /// The last-**delivered** seq — the seq of the last event whose
    /// wake-triggered turn has ended. Events past this seq are re-read on
    /// restart.
    pub cursor: u64,
    /// The author string (`session:<uuid>`) of the most recent keeper
    /// session, used to filter that keeper's own comments during re-read.
    /// `None` when no keeper has ever run.
    pub keeper_author: Option<String>,
}

/// Resolves the cursor file path from the board store's events path.
/// Returns `None` if the events path has no parent (shouldn't happen in
/// practice, but defensive).
pub(crate) fn cursor_path(events_path: &Path) -> Option<PathBuf> {
    events_path.parent().map(|dir| dir.join(CURSOR_FILE_NAME))
}

/// Loads the persisted cursor state. Returns a zero cursor with no keeper
/// author if the file doesn't exist (first run) or is unreadable — the
/// subscriber starts from the beginning, which is safe (at worst it
/// re-processes already-seen events, which the policy's idempotent cursor
/// check skips).
pub(crate) fn load(cursor_path: &Path) -> CursorState {
    match std::fs::read_to_string(cursor_path) {
        Ok(text) => {
            let mut lines = text.lines();
            let cursor = lines
                .next()
                .and_then(|l| l.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let keeper_author = lines
                .next()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty());
            CursorState {
                cursor,
                keeper_author,
            }
        }
        Err(_) => CursorState {
            cursor: 0,
            keeper_author: None,
        },
    }
}

/// Persists the cursor state atomically (write-to-temp-then-rename). A
/// failure is logged to stderr but does not propagate — a stale cursor only
/// means the subscriber re-processes some events on restart, which the
/// policy handles idempotently.
pub(crate) fn save(cursor_path: &Path, state: &CursorState) {
    if let Some(dir) = cursor_path.parent() {
        if let Err(e) = write_atomic(cursor_path, dir, state) {
            eprintln!("horizon-agentd: failed to persist wake cursor: {e}");
        }
    }
}

/// Writes `state` to a temp file in `dir`, then renames it to `path`.
fn write_atomic(path: &Path, dir: &Path, state: &CursorState) -> io::Result<()> {
    let tmp = dir.join(format!(".{CURSOR_FILE_NAME}.tmp"));
    let keeper_author = state.keeper_author.as_deref().unwrap_or("");
    let text = format!("{}\n{}\n", state.cursor, keeper_author);
    std::fs::write(&tmp, text)?;
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
        let state = load(&path);
        assert_eq!(state.cursor, 0);
        assert!(state.keeper_author.is_none());
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

        save(
            &path,
            &CursorState {
                cursor: 42,
                keeper_author: Some("session:abc-123".to_string()),
            },
        );
        let state = load(&path);
        assert_eq!(state.cursor, 42);
        assert_eq!(state.keeper_author.as_deref(), Some("session:abc-123"));

        save(
            &path,
            &CursorState {
                cursor: 100,
                keeper_author: None,
            },
        );
        let state = load(&path);
        assert_eq!(state.cursor, 100);
        assert!(state.keeper_author.is_none());
    }

    #[test]
    fn load_legacy_single_number_format() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-cursor-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CURSOR_FILE_NAME);
        // Legacy format: a single bare number with no second line.
        std::fs::write(&path, "57").unwrap();
        let state = load(&path);
        assert_eq!(state.cursor, 57);
        assert!(state.keeper_author.is_none());
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
        let state = load(&path);
        assert_eq!(state.cursor, 0);
        assert!(state.keeper_author.is_none());
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
