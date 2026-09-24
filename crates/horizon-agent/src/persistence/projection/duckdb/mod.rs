use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use duckdb::Connection;

use crate::contract::SessionId;

mod append;
mod import;
mod projection;
mod query;
mod records;
mod schema;
mod shared_store;

use schema::INITIALIZE_SCHEMA_SQL;

pub(crate) use import::ApplyRecordsReport;

use records::AgentStoredEvent;

#[cfg(test)]
pub(crate) use records::{
    AgentStoredApproval, AgentStoredMessage, AgentStoredSession, AgentStoredSessionSnapshot,
    AgentStoredToolCall, AgentStoredToolResult, AgentStoredTurn, AppendEvent,
};
pub(crate) use records::{RecallEntry, RecallEntryKind, RecallSearchReport};
pub use shared_store::SharedDuckdbStore;

/// A live `Store`, shared (behind a lock) by every in-process consumer that
/// needs it -- see [`SharedDuckdbStore`]'s doc comment for why a second,
/// independent `Store::open` of the same path is unsound rather than
/// merely redundant.
///
/// A newtype around `Arc<Mutex<Store>>`, not a plain alias: `Store` is
/// crate-internal (its query/append API has no external consumer -- see
/// the 2026-07-18 interface audit), but this handle itself is real API
/// `horizon-agentd` holds, clones, and threads through construction
/// (`SharedDuckdbStore`, `ToolSessionState`/`RecallContext`) -- a bare
/// `pub type` alias over a `pub(crate)` `Store` would leak the private type
/// into a public signature (`private_interfaces`). Only this crate's own
/// code, which actually queries the store, reaches inside via [`Self::
/// lock`]; `horizon-agentd` never does (confirmed by grep at the time of
/// this narrowing) -- it only clones and passes the handle along.
#[derive(Clone)]
pub struct DuckdbStoreHandle(Arc<Mutex<Store>>);

impl DuckdbStoreHandle {
    pub(crate) fn new(store: Store) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// Forwards to `Mutex::lock` verbatim (same `LockResult` return shape)
    /// so every existing `store.lock().unwrap_or_else(|poisoned| ...)`
    /// call site keeps working unchanged.
    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Store>> {
        self.0.lock()
    }
}

pub(crate) struct Store {
    conn: Connection,
}

impl Store {
    pub(crate) fn open_in_memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().context("open in-memory DuckDB agent store")?,
        )
    }

    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path).context("open DuckDB agent store")?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        Ok(Self { conn })
    }
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
