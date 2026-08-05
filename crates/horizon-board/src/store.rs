//! The board store: flock-serialised append + in-memory fold.
//!
//! Each CLI invocation is a separate short-lived process, so cross-process
//! serialisation is done with an advisory exclusive lock (`flock` via `fs4`)
//! rather than the single-writer-thread model the agent event log uses
//! (that model only works within one long-lived daemon process). Reads use
//! a shared lock; writes use an exclusive lock held across the
//! read-fold-append sequence so that `claim` is atomic against concurrent
//! claims from other processes.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use crate::event::{self, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};
use crate::model::{fold, sorted_by_rank, Item};
use crate::rank;

/// Advisory exclusive lock via `flock(2)`. Held until the file is dropped
/// (the kernel releases it on close). Used across the read-fold-append
/// sequence so concurrent CLI processes serialise on the same events file.
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Advisory shared lock via `flock(2)`.
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

/// Where to place a new or moved item in the rank queue.
#[derive(Debug, Clone)]
pub enum Position {
    Top,
    After(u64),
    Before(u64),
    Bottom,
}

/// The result of `list`: filtered items, the full set of statuses seen
/// across all items (for vocabulary-drift visibility), and an optional
/// skipped-line summary from the tolerant reader.
#[derive(Debug)]
pub struct ListResult {
    pub items: Vec<Item>,
    pub statuses: Vec<String>,
    pub skipped: Option<String>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ItemNotFound(u64),
    RankExhausted,
    NotInGitRepo,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::ItemNotFound(id) => write!(f, "item {id} not found"),
            Self::RankExhausted => write!(f, "rank space exhausted (rebalance needed)"),
            Self::NotInGitRepo => write!(f, "not inside a git repository"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// The board store. Owns only the path; every operation opens the file,
/// locks, reads-folds, (maybe) appends, and releases.
pub struct Store {
    path: PathBuf,
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

impl Store {
    /// Resolves the store from the current directory's main git root.
    pub fn from_cwd() -> Result<Self, StoreError> {
        let cwd = std::env::current_dir()?;
        let root = crate::path::main_root(&cwd).ok_or(StoreError::NotInGitRepo)?;
        Ok(Self {
            path: crate::path::events_path(&root),
        })
    }

    /// Resolves the store from an explicit directory inside a git repo --
    /// the main git root is resolved from `dir` (so a linked worktree maps
    /// to the same store as the main checkout), exactly as `from_cwd` does
    /// but without depending on the process cwd. The GUI shell uses this so
    /// a board modal reading the active session's `workspace_root` reads the
    /// same store the board CLI (`from_cwd`) reads.
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, StoreError> {
        let root = crate::path::main_root(dir).ok_or(StoreError::NotInGitRepo)?;
        Ok(Self {
            path: crate::path::events_path(&root),
        })
    }

    /// Opens a store at an explicit path (for testing).
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    // -- internals ------------------------------------------------------

    /// Opens the file for writing (create + append), acquires an exclusive
    /// lock, and reads the current event log. The lock is held until the
    /// returned `File` is dropped.
    fn open_locked(&self) -> Result<(File, ReadReport), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        lock_exclusive(&file)?;
        let report = event::read(&self.path)?;
        Ok((file, report))
    }

    /// Opens the file for reading with a shared lock.
    fn read_locked(&self) -> Result<ReadReport, StoreError> {
        if !self.path.exists() {
            return Ok(ReadReport::default());
        }
        let file = File::open(&self.path)?;
        lock_shared(&file)?;
        let report = event::read(&self.path)?;
        unlock(&file);
        Ok(report)
    }

    fn append(file: &mut File, env: &Envelope) -> Result<(), StoreError> {
        serde_json::to_writer(&mut *file, env)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    /// Computes the rank for a new/moved item at `position`, given the
    /// current folded items.
    fn compute_rank(items: &HashMap<u64, Item>, position: &Position) -> Result<String, StoreError> {
        let sorted = sorted_by_rank(items);
        match position {
            Position::Top => {
                let hi = sorted.first().map(|i| i.rank.as_str());
                rank::between(None, hi).ok_or(StoreError::RankExhausted)
            }
            Position::Bottom => {
                let lo = sorted.last().map(|i| i.rank.as_str());
                rank::between(lo, None).ok_or(StoreError::RankExhausted)
            }
            Position::After(id) => {
                let item = items.get(id).ok_or(StoreError::ItemNotFound(*id))?;
                let idx = sorted
                    .iter()
                    .position(|i| i.id == *id)
                    .expect("item in map but not in sorted list");
                let lo = Some(item.rank.as_str());
                let hi = sorted.get(idx + 1).map(|i| i.rank.as_str());
                rank::between(lo, hi).ok_or(StoreError::RankExhausted)
            }
            Position::Before(id) => {
                let item = items.get(id).ok_or(StoreError::ItemNotFound(*id))?;
                let idx = sorted
                    .iter()
                    .position(|i| i.id == *id)
                    .expect("item in map but not in sorted list");
                let lo = if idx > 0 {
                    Some(sorted[idx - 1].rank.as_str())
                } else {
                    None
                };
                let hi = Some(item.rank.as_str());
                rank::between(lo, hi).ok_or(StoreError::RankExhausted)
            }
        }
    }

    // -- public operations ----------------------------------------------

    /// Creates a new item. If `parent` is set, a follow-up `item-updated`
    /// event is appended under the same lock so the item appears with its
    /// parent on the next read.
    pub fn add(
        &self,
        title: &str,
        body: &str,
        parent: Option<u64>,
        position: Position,
    ) -> Result<Item, StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        let id = report.max_id.map_or(1, |m| m + 1);
        let rank = Self::compute_rank(&items, &position)?;

        let env = make_envelope(BoardEvent::ItemCreated {
            id,
            title: title.to_string(),
            body: body.to_string(),
            rank: rank.clone(),
        });
        Self::append(&mut file, &env)?;

        if let Some(pid) = parent {
            let upd = make_envelope(BoardEvent::ItemUpdated {
                id,
                status: None,
                rank: None,
                assignee: None,
                parent: Some(Some(pid)),
                depends_on: None,
                links: None,
                title: None,
                body: None,
            });
            Self::append(&mut file, &upd)?;
        }

        Ok(Item {
            id,
            title: title.to_string(),
            body: body.to_string(),
            rank,
            parent,
            ..Item::default()
        })
    }

    /// Lists items in rank order, optionally filtered by status. Always
    /// returns the full set of statuses across all items (for vocabulary-
    /// drift visibility).
    pub fn list(&self, status_filter: Option<&str>) -> Result<ListResult, StoreError> {
        let report = self.read_locked()?;
        let items = fold(&report.envelopes);
        let sorted = sorted_by_rank(&items);

        let statuses: Vec<String> = {
            let mut s: Vec<String> = sorted
                .iter()
                .map(|i| i.status.clone())
                .filter(|s| !s.is_empty())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            s.dedup();
            s
        };

        let items: Vec<Item> = match status_filter {
            Some(filter) => sorted
                .into_iter()
                .filter(|i| i.status == filter)
                .cloned()
                .collect(),
            None => sorted.into_iter().cloned().collect(),
        };

        Ok(ListResult {
            items,
            statuses,
            skipped: report.skipped_summary(),
        })
    }

    /// Returns the full item (with comments) or `None` if the id doesn't exist.
    pub fn show(&self, id: u64) -> Result<Option<Item>, StoreError> {
        let report = self.read_locked()?;
        let items = fold(&report.envelopes);
        Ok(items.get(&id).cloned())
    }

    /// Appends a comment to item `id`.
    pub fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        if !items.contains_key(&id) {
            return Err(StoreError::ItemNotFound(id));
        }
        let env = make_envelope(BoardEvent::CommentAdded {
            id,
            author: author.to_string(),
            text: text.to_string(),
        });
        Self::append(&mut file, &env)
    }

    /// Sets the status of item `id`. The status is a free-form string
    /// (recommended vocabulary: proposed / ready / in-progress / review /
    /// done / blocked).
    pub fn set_status(&self, id: u64, status: &str) -> Result<(), StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        if !items.contains_key(&id) {
            return Err(StoreError::ItemNotFound(id));
        }
        let env = make_envelope(BoardEvent::ItemUpdated {
            id,
            status: Some(status.to_string()),
            rank: None,
            assignee: None,
            parent: None,
            depends_on: None,
            links: None,
            title: None,
            body: None,
        });
        Self::append(&mut file, &env)
    }

    /// Assigns item `id` to `who` (empty string = unassign).
    pub fn assign(&self, id: u64, who: &str) -> Result<(), StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        if !items.contains_key(&id) {
            return Err(StoreError::ItemNotFound(id));
        }
        let env = make_envelope(BoardEvent::ItemUpdated {
            id,
            status: None,
            rank: None,
            assignee: Some(who.to_string()),
            parent: None,
            depends_on: None,
            links: None,
            title: None,
            body: None,
        });
        Self::append(&mut file, &env)
    }

    /// Re-ranks item `id` to a new position in the queue.
    pub fn move_item(&self, id: u64, position: Position) -> Result<String, StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        if !items.contains_key(&id) {
            return Err(StoreError::ItemNotFound(id));
        }
        let rank = Self::compute_rank(&items, &position)?;
        let env = make_envelope(BoardEvent::ItemUpdated {
            id,
            status: None,
            rank: Some(rank.clone()),
            assignee: None,
            parent: None,
            depends_on: None,
            links: None,
            title: None,
            body: None,
        });
        Self::append(&mut file, &env)?;
        Ok(rank)
    }

    /// Atomically claims the first ready+unassigned item (by rank order):
    /// sets `status = in-progress` and `assignee = who` under the exclusive
    /// lock, so two concurrent claims never grab the same item.
    pub fn claim(&self, who: &str) -> Result<Option<Item>, StoreError> {
        let (mut file, report) = self.open_locked()?;
        let items = fold(&report.envelopes);
        let sorted = sorted_by_rank(&items);

        let found = sorted
            .into_iter()
            .find(|i| i.status == "ready" && i.assignee.is_empty());

        let Some(mut item) = found.cloned() else {
            return Ok(None);
        };

        let env = make_envelope(BoardEvent::ItemUpdated {
            id: item.id,
            status: Some("in-progress".to_string()),
            rank: None,
            assignee: Some(who.to_string()),
            parent: None,
            depends_on: None,
            links: None,
            title: None,
            body: None,
        });
        Self::append(&mut file, &env)?;

        item.status = "in-progress".to_string();
        item.assignee = who.to_string();
        Ok(Some(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "horizon-board-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Store::at(dir.join("events.jsonl"))
    }

    #[test]
    fn fold_roundtrip_create_update_comment() {
        let store = tmp_store();

        let item = store
            .add("Task", "Do the thing", None, Position::Bottom)
            .unwrap();
        assert_eq!(item.id, 1);
        assert_eq!(item.rank, "n");

        store.set_status(1, "ready").unwrap();
        store.assign(1, "owner").unwrap();
        store.comment(1, "owner", "Starting now").unwrap();

        let shown = store.show(1).unwrap().unwrap();
        assert_eq!(shown.title, "Task");
        assert_eq!(shown.body, "Do the thing");
        assert_eq!(shown.status, "ready");
        assert_eq!(shown.assignee, "owner");
        assert_eq!(shown.comments.len(), 1);
        assert_eq!(shown.comments[0].text, "Starting now");
    }

    #[test]
    fn list_rank_order_and_statuses() {
        let store = tmp_store();

        let a = store.add("A", "", None, Position::Bottom).unwrap();
        let b = store.add("B", "", None, Position::Bottom).unwrap();
        let c = store.add("C", "", None, Position::Top).unwrap();

        let result = store.list(None).unwrap();
        // C was inserted at top, then A, then B at bottom
        assert_eq!(result.items.len(), 3);
        assert_eq!(result.items[0].id, c.id);
        assert_eq!(result.items[1].id, a.id);
        assert_eq!(result.items[2].id, b.id);

        // No statuses set yet
        assert!(result.statuses.is_empty());

        // Set some statuses
        store.set_status(a.id, "proposed").unwrap();
        store.set_status(b.id, "ready").unwrap();

        let result = store.list(None).unwrap();
        assert_eq!(result.statuses, vec!["proposed", "ready"]);

        // Filter by status
        let result = store.list(Some("ready")).unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, b.id);
    }

    #[test]
    fn claim_serialization_two_claims_get_different_items() {
        let store = tmp_store();

        store.add("A", "", None, Position::Bottom).unwrap();
        store.add("B", "", None, Position::Bottom).unwrap();

        store.set_status(1, "ready").unwrap();
        store.set_status(2, "ready").unwrap();

        // First claim gets item 1 (rank "n", first in queue)
        let first = store.claim("alice").unwrap().unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(first.status, "in-progress");
        assert_eq!(first.assignee, "alice");

        // Second claim gets item 2
        let second = store.claim("bob").unwrap().unwrap();
        assert_eq!(second.id, 2);
        assert_eq!(second.status, "in-progress");
        assert_eq!(second.assignee, "bob");
    }

    #[test]
    fn claim_returns_none_when_no_ready_unassigned() {
        let store = tmp_store();

        store.add("A", "", None, Position::Bottom).unwrap();
        // Status is empty (not "ready"), so no claim
        assert!(store.claim("alice").unwrap().is_none());

        store.set_status(1, "ready").unwrap();
        store.assign(1, "bob").unwrap();
        // Ready but assigned, so no claim
        assert!(store.claim("alice").unwrap().is_none());

        store.assign(1, "").unwrap(); // unassign
                                      // Now ready and unassigned
        let claimed = store.claim("alice").unwrap().unwrap();
        assert_eq!(claimed.id, 1);
    }

    #[test]
    fn unknown_status_and_author_dont_break() {
        let store = tmp_store();

        store.add("A", "", None, Position::Bottom).unwrap();
        store.set_status(1, "weird-custom-status").unwrap();
        store.comment(1, "session:abc-123", "a note").unwrap();

        let item = store.show(1).unwrap().unwrap();
        assert_eq!(item.status, "weird-custom-status");
        assert_eq!(item.comments[0].author, "session:abc-123");

        let result = store.list(None).unwrap();
        assert_eq!(result.statuses, vec!["weird-custom-status"]);
    }

    #[test]
    fn move_item_changes_rank() {
        let store = tmp_store();

        let a = store.add("A", "", None, Position::Bottom).unwrap();
        let _b = store.add("B", "", None, Position::Bottom).unwrap();
        let _c = store.add("C", "", None, Position::Bottom).unwrap();

        // Move A to top
        let new_rank = store.move_item(a.id, Position::Top).unwrap();
        assert!(new_rank.as_str() < "n"); // A's new rank should be before the first item

        let result = store.list(None).unwrap();
        assert_eq!(result.items[0].id, a.id);
    }

    #[test]
    fn add_with_parent() {
        let store = tmp_store();

        let parent = store.add("Parent", "", None, Position::Bottom).unwrap();
        let child = store
            .add("Child", "", Some(parent.id), Position::Bottom)
            .unwrap();

        let shown = store.show(child.id).unwrap().unwrap();
        assert_eq!(shown.parent, Some(parent.id));
    }

    #[test]
    fn item_not_found_errors() {
        let store = tmp_store();
        store.add("A", "", None, Position::Bottom).unwrap();

        assert!(matches!(
            store.set_status(99, "ready"),
            Err(StoreError::ItemNotFound(99))
        ));
        assert!(matches!(
            store.comment(99, "x", "y"),
            Err(StoreError::ItemNotFound(99))
        ));
        assert!(matches!(
            store.move_item(99, Position::Top),
            Err(StoreError::ItemNotFound(99))
        ));
    }

    #[test]
    fn show_nonexistent_returns_none() {
        let store = tmp_store();
        store.add("A", "", None, Position::Bottom).unwrap();
        assert!(store.show(99).unwrap().is_none());
    }

    #[test]
    fn corrupt_lines_are_skipped_and_reported() {
        let store = tmp_store();

        store.add("A", "", None, Position::Bottom).unwrap();
        // Append a corrupt line directly to the file
        std::fs::OpenOptions::new()
            .append(true)
            .open(&store.path)
            .unwrap()
            .write_all(b"this is corrupt\n")
            .unwrap();
        store.add("B", "", None, Position::Bottom).unwrap();

        let result = store.list(None).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(result.skipped.is_some());
        assert!(result.skipped.as_ref().unwrap().contains("corrupt"));
    }
}
