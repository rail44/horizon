use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use duckdb::Connection;

use crate::contract::SessionId;

mod append;
mod catch_up;
mod import;
mod projection;
mod query;
mod records;
mod schema;

use schema::INITIALIZE_SCHEMA_SQL;

#[cfg(test)]
use records::AgentStoredEvent;

#[cfg(test)]
pub(crate) use records::{
    AgentStoredApproval, AgentStoredMessage, AgentStoredSession, AgentStoredSessionSnapshot,
    AgentStoredToolCall, AgentStoredToolResult, AgentStoredTurn, AppendEvent,
};
pub(crate) use records::{RecallEntry, RecallEntryKind, RecallSearchReport, RecallSessionSummary};

/// The projection of the session logs in one directory, kept in one DuckDB
/// file that no process holds open (`docs/agent-stdio-child-design.md`,
/// "The DuckDB projection"). Each query first catches the file up with the
/// session logs (opened for writing), then closes it and runs on it opened
/// read-only. Either open waits while another process holds the file.
#[derive(Clone, Debug)]
pub struct Projection {
    source: Source,
}

#[derive(Clone, Debug)]
enum Source {
    Files {
        sessions_dir: PathBuf,
        db_path: PathBuf,
    },
    /// A store a test fills itself.
    #[cfg(test)]
    Held(DuckdbStoreHandle),
}

/// How long an open waits for another process to close the file.
const OPEN_WAIT: Duration = Duration::from_secs(30);

impl Projection {
    /// The projection at `db_path` of the session logs in `sessions_dir`.
    pub fn new(sessions_dir: impl Into<PathBuf>, db_path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::Files {
                sessions_dir: sessions_dir.into(),
                db_path: db_path.into(),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn held(store: Store) -> Self {
        Self {
            source: Source::Held(DuckdbStoreHandle::new(store)),
        }
    }

    /// The store a test holds.
    #[cfg(test)]
    pub(crate) fn held_store(&self) -> DuckdbStoreHandle {
        match &self.source {
            Source::Held(store) => store.clone(),
            Source::Files { .. } => panic!("this projection reads files"),
        }
    }

    /// Brings the projection level with the session logs and closes it,
    /// for a reader outside this crate (a test, an analysis) that opens the
    /// file itself.
    pub fn catch_up(&self) -> Result<()> {
        self.query(|_| Ok(()))
    }

    pub(crate) fn query<T>(&self, query: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        match &self.source {
            Source::Files {
                sessions_dir,
                db_path,
            } => {
                {
                    let store = Store::open_waiting(db_path, Access::ReadWrite)?;
                    let report = store.catch_up_from_sessions(sessions_dir)?;
                    if report.skipped > 0 {
                        eprintln!(
                            "horizon-agent: history search skipped {} record(s) it could not \
                             project ({})",
                            report.skipped,
                            report.first_skip_error.as_deref().unwrap_or("no detail")
                        );
                    }
                }
                let store = Store::open_waiting(db_path, Access::ReadOnly)?;
                query(&store)
            }
            #[cfg(test)]
            Source::Held(store) => query(&store.lock().unwrap_or_else(PoisonError::into_inner)),
        }
    }
}

/// A store a test holds open across queries.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct DuckdbStoreHandle(std::sync::Arc<std::sync::Mutex<Store>>);

#[cfg(test)]
impl std::fmt::Debug for DuckdbStoreHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DuckdbStoreHandle")
    }
}

#[cfg(test)]
impl DuckdbStoreHandle {
    pub(crate) fn new(store: Store) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(store)))
    }

    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Store>> {
        self.0.lock()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Access {
    ReadWrite,
    ReadOnly,
}

pub(crate) struct Store {
    conn: Connection,
}

impl Store {
    pub(crate) fn open_in_memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().context("open in-memory DuckDB agent store")?,
            Access::ReadWrite,
        )
    }

    #[cfg(test)]
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path.as_ref(), Access::ReadWrite)
    }

    fn open_with(path: &Path, access: Access) -> Result<Self> {
        if access == Access::ReadWrite
            && let Some(parent) = path.parent()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create DuckDB agent store directory {}", parent.display())
            })?;
        }
        let config = match access {
            Access::ReadWrite => duckdb::Config::default(),
            Access::ReadOnly => {
                duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?
            }
        };
        let conn = Connection::open_with_flags(path, config)
            .with_context(|| format!("open DuckDB agent store {}", path.display()))?;
        Self::from_connection(conn, access)
    }

    /// [`Self::open_with`], waiting while another process holds the file
    /// (DuckDB refuses every other open, read-only too, while one process
    /// has it open for writing).
    pub(crate) fn open_waiting(path: &Path, access: Access) -> Result<Self> {
        let deadline = Instant::now() + OPEN_WAIT;
        loop {
            match Self::open_with(path, access) {
                Ok(store) => return Ok(store),
                Err(error) if held_elsewhere(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn from_connection(conn: Connection, access: Access) -> Result<Self> {
        if access == Access::ReadWrite {
            conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        }
        Ok(Self { conn })
    }
}

/// Whether opening failed because another process holds the file's lock.
fn held_elsewhere(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("Could not set lock on file")
}

fn session_id_text(session_id: SessionId) -> Result<String> {
    let value = serde_json::to_value(session_id).context("serialize session id")?;
    Ok(value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bench_probe;
