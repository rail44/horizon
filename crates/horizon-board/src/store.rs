//! The board store: reads are flock-serialised file folds; writes are socket
//! calls to `horizon-logd`.
//!
//! The write path (exclusive flock + read-fold + id/rank computation + append)
//! moved to `horizon-logd` in the logd v1 split (`docs/logd-design.md`). The
//! library's write methods are now thin socket clients: connect-or-spawn logd,
//! `hello`, send one `ingest` rtc call, return the result. The direct flock
//! append path is gone (owner decision — no fallback). Reads stay file folds
//! with a shared lock: JSONL is world-readable, a single writer (logd) plus
//! atomic appends make direct reads safe, and boards have no projection.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{self, ReadReport};
use crate::model::{fold, sorted_by_rank, Item};
use crate::wire::{log_client_hello, IngestReply, IngestRequest, LogError, LogHub, LogHubClient};

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

/// Where to place a new or moved item in the rank queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// The board store. Owns the events.jsonl path (for reads) and the logd
/// socket path (for writes). Every read opens the file, takes a shared lock,
/// folds, and releases. Every write connects to logd and sends an `ingest`
/// rtc call.
pub struct Store {
    path: PathBuf,
    logd_socket: PathBuf,
}

impl Store {
    /// Resolves the store from the current directory's main git root.
    pub fn from_cwd() -> Result<Self, StoreError> {
        let cwd = std::env::current_dir()?;
        let root = crate::path::main_root(&cwd).ok_or(StoreError::NotInGitRepo)?;
        Ok(Self {
            path: crate::path::events_path(&root),
            logd_socket: horizon_wire::socket::default_logd_socket_path(),
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
            logd_socket: horizon_wire::socket::default_logd_socket_path(),
        })
    }

    /// Opens a store at an explicit path (for testing).
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            logd_socket: horizon_wire::socket::default_logd_socket_path(),
        }
    }

    /// Opens a store at an explicit path with an explicit logd socket (for
    /// tests that spawn logd on an isolated socket).
    pub fn at_with_socket(path: PathBuf, logd_socket: PathBuf) -> Self {
        Self { path, logd_socket }
    }

    /// The events.jsonl path this store reads from and tells logd to append to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    // -- reads (file folds, unchanged) ----------------------------------

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

    // -- writes (socket client → horizon-logd) --------------------------

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
        let reply = self.ingest(IngestRequest::Add {
            title: title.to_string(),
            body: body.to_string(),
            parent,
            position,
        })?;
        match reply {
            IngestReply::Item(item) => Ok(item),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Appends a comment to item `id`.
    pub fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), StoreError> {
        let reply = self.ingest(IngestRequest::Comment {
            id,
            author: author.to_string(),
            text: text.to_string(),
        })?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Sets the status of item `id`. The status is a free-form string
    /// (recommended vocabulary: proposed / ready / in-progress / review /
    /// done / blocked).
    pub fn set_status(&self, id: u64, status: &str) -> Result<(), StoreError> {
        let reply = self.ingest(IngestRequest::SetStatus {
            id,
            status: status.to_string(),
        })?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Assigns item `id` to `who` (empty string = unassign).
    pub fn assign(&self, id: u64, who: &str) -> Result<(), StoreError> {
        let reply = self.ingest(IngestRequest::Assign {
            id,
            who: who.to_string(),
        })?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Re-ranks item `id` to a new position in the queue.
    pub fn move_item(&self, id: u64, position: Position) -> Result<String, StoreError> {
        let reply = self.ingest(IngestRequest::MoveItem { id, position })?;
        match reply {
            IngestReply::Rank(rank) => Ok(rank),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Atomically claims the first ready+unassigned item (by rank order):
    /// sets `status = in-progress` and `assignee = who` under the exclusive
    /// lock, so two concurrent claims never grab the same item.
    pub fn claim(&self, who: &str) -> Result<Option<Item>, StoreError> {
        let reply = self.ingest(IngestRequest::Claim {
            who: who.to_string(),
        })?;
        match reply {
            IngestReply::MaybeItem(item) => Ok(item),
            _ => Err(Self::type_mismatch()),
        }
    }

    // -- internals ------------------------------------------------------

    #[cold]
    fn type_mismatch() -> StoreError {
        StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "logd returned an unexpected reply type for this request",
        ))
    }

    /// Connects to logd (spawning it if necessary), hellos, and sends one
    /// `ingest` call. Each write method wraps this with the matching
    /// `IngestRequest`/`IngestReply` variant.
    fn ingest(&self, request: IngestRequest) -> Result<IngestReply, StoreError> {
        let socket = self.logd_socket.clone();
        let path = self.path.to_string_lossy().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
        runtime.block_on(async move {
            let stream = horizon_wire::spawn::connect_or_spawn_logd_retrying(&socket)
                .await
                .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
            let (hub, conn_task) = horizon_wire::spawn::connect_hub_client::<
                LogHubClient<horizon_wire::WireCodec>,
            >(stream)
            .await
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;

            hub.hello(log_client_hello(concat!(
                "horizon-board/",
                env!("CARGO_PKG_VERSION")
            )))
            .await
            .map_err(hub_error_to_store)?;

            let reply = hub
                .ingest(path, request)
                .await
                .map_err(log_error_to_store)?;

            conn_task.abort();
            Ok::<IngestReply, StoreError>(reply)
        })
    }
}

/// Maps a `HubError` from the `hello` call to `StoreError`. A `hello` failure
/// is a protocol/transport problem (version mismatch, lost connection), not a
/// board-domain error.
fn hub_error_to_store(err: horizon_wire::HubError) -> StoreError {
    StoreError::Io(std::io::Error::other(err.to_string()))
}

/// Maps a `LogError` from the `ingest` call to `StoreError`, preserving the
/// typed domain errors (`ItemNotFound`, `RankExhausted`).
fn log_error_to_store(err: LogError) -> StoreError {
    match err {
        LogError::ItemNotFound(id) => StoreError::ItemNotFound(id),
        LogError::RankExhausted => StoreError::RankExhausted,
        LogError::Io(msg) => StoreError::Io(std::io::Error::other(msg)),
        LogError::Call(msg) => StoreError::Io(std::io::Error::other(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{BoardEvent, Envelope, SCHEMA, VERSION};
    use std::io::Write;
    use std::path::PathBuf;

    /// A throwaway events.jsonl path for a test. No daemon, no socket — the
    /// unit tests seed the file directly and read it back with `Store::show`/
    /// `Store::list` (file folds that need no daemon). The write path (id
    /// assignment, rank computation, claim) is tested end-to-end in
    /// `crates/horizon-logd/tests/e2e.rs` against the real daemon.
    fn tmp_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-board-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join("events.jsonl")
    }

    /// Seeds an `item-created` event at `at` ms, returning the id.
    fn seed_item(path: &std::path::Path, id: u64, title: &str, rank: &str, at: u64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event: BoardEvent::ItemCreated {
                id,
                title: title.to_string(),
                body: String::new(),
                rank: rank.to_string(),
            },
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        serde_json::to_writer(&mut file, &env).unwrap();
        file.write_all(b"\n").unwrap();
    }

    /// Seeds an `item-updated` event.
    fn seed_update(
        path: &std::path::Path,
        id: u64,
        at: u64,
        status: Option<&str>,
        rank: Option<&str>,
        assignee: Option<&str>,
        parent: Option<Option<u64>>,
    ) {
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event: BoardEvent::ItemUpdated {
                id,
                status: status.map(String::from),
                rank: rank.map(String::from),
                assignee: assignee.map(String::from),
                parent,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            },
        };
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, &env).unwrap();
        file.write_all(b"\n").unwrap();
    }

    /// Seeds a `comment-added` event.
    fn seed_comment(path: &std::path::Path, id: u64, author: &str, text: &str, at: u64) {
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event: BoardEvent::CommentAdded {
                id,
                author: author.to_string(),
                text: text.to_string(),
            },
        };
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, &env).unwrap();
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn fold_roundtrip_create_update_comment() {
        let path = tmp_path();
        seed_item(&path, 1, "Task", "n", 1000);
        seed_update(&path, 1, 2000, Some("ready"), None, Some("owner"), None);
        seed_comment(&path, 1, "owner", "Starting now", 3000);

        let store = Store::at(path);
        let shown = store.show(1).unwrap().unwrap();
        assert_eq!(shown.title, "Task");
        assert_eq!(shown.status, "ready");
        assert_eq!(shown.assignee, "owner");
        assert_eq!(shown.comments.len(), 1);
        assert_eq!(shown.comments[0].text, "Starting now");
    }

    #[test]
    fn list_rank_order_and_statuses() {
        let path = tmp_path();
        // C at rank "a" (top), A at "n", B at "s" (bottom).
        seed_item(&path, 1, "A", "n", 1000);
        seed_item(&path, 2, "B", "s", 2000);
        seed_item(&path, 3, "C", "a", 3000);

        let store = Store::at(path.clone());
        let result = store.list(None).unwrap();
        assert_eq!(result.items.len(), 3);
        // Sorted by rank: C (a), A (n), B (s).
        assert_eq!(result.items[0].id, 3);
        assert_eq!(result.items[1].id, 1);
        assert_eq!(result.items[2].id, 2);
        assert!(result.statuses.is_empty());

        seed_update(&path, 1, 4000, Some("proposed"), None, None, None);
        seed_update(&path, 2, 5000, Some("ready"), None, None, None);

        let result = store.list(None).unwrap();
        assert_eq!(result.statuses, vec!["proposed", "ready"]);

        let result = store.list(Some("ready")).unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, 2);
    }

    #[test]
    fn claim_sees_ready_unassigned_items() {
        let path = tmp_path();
        seed_item(&path, 1, "A", "n", 1000);
        seed_item(&path, 2, "B", "s", 2000);
        // Both ready.
        seed_update(&path, 1, 3000, Some("ready"), None, None, None);
        seed_update(&path, 2, 4000, Some("ready"), None, None, None);

        // The fold (read path) should show both as ready+unassigned.
        let store = Store::at(path);
        let result = store.list(Some("ready")).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(result.items[0].assignee.is_empty());
        assert!(result.items[1].assignee.is_empty());
    }

    #[test]
    fn unknown_status_and_author_dont_break() {
        let path = tmp_path();
        seed_item(&path, 1, "A", "n", 1000);
        seed_update(
            &path,
            1,
            2000,
            Some("weird-custom-status"),
            None,
            None,
            None,
        );
        seed_comment(&path, 1, "session:abc-123", "a note", 3000);

        let store = Store::at(path);
        let item = store.show(1).unwrap().unwrap();
        assert_eq!(item.status, "weird-custom-status");
        assert_eq!(item.comments[0].author, "session:abc-123");

        let result = store.list(None).unwrap();
        assert_eq!(result.statuses, vec!["weird-custom-status"]);
    }

    #[test]
    fn add_with_parent() {
        let path = tmp_path();
        seed_item(&path, 1, "Parent", "n", 1000);
        seed_item(&path, 2, "Child", "s", 2000);
        seed_update(&path, 2, 3000, None, None, None, Some(Some(1)));

        let store = Store::at(path);
        let shown = store.show(2).unwrap().unwrap();
        assert_eq!(shown.parent, Some(1));
    }

    #[test]
    fn show_nonexistent_returns_none() {
        let store = Store::at(tmp_path());
        assert!(store.show(99).unwrap().is_none());
    }

    #[test]
    fn corrupt_lines_are_skipped_and_reported() {
        let path = tmp_path();
        seed_item(&path, 1, "A", "n", 1000);
        // Append a corrupt line directly to the file
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"this is corrupt\n")
            .unwrap();
        seed_item(&path, 2, "B", "s", 2000);

        let store = Store::at(path);
        let result = store.list(None).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(result.skipped.is_some());
        assert!(result.skipped.as_ref().unwrap().contains("corrupt"));
    }
}
