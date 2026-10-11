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

pub use records::SessionLiveness;
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

    /// Read only a complete projection. The health check and query share the
    /// writer's lock, so an already-issued handle cannot expose a partial DB.
    pub(crate) fn query<T>(&self, query: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let store = self
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(reason) = &store.failure {
            anyhow::bail!("history search is unavailable until rebuild: {reason}");
        }
        query(&store)
    }

    /// Projects one live append. `cursor`, when known, is the record's own
    /// position in the JSONL file and is committed with it.
    pub(crate) fn project(
        &self,
        record: &crate::persistence::event_log::Record,
        cursor: Option<&crate::persistence::event_log::LogCursor>,
    ) {
        let mut store = self
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if store.failure.is_some() {
            return;
        }
        if let Err(error) = store.append_record_at(record, cursor) {
            let reason = format!("{error:#}");
            eprintln!("horizon-agent: history search disabled until rebuild: {reason}");
            store.failure = Some(reason);
        }
    }

    /// Resume's pre-filter over every session with an event at or below
    /// `through_sequence` -- see [`SessionLiveness`].
    pub fn session_liveness(&self, through_sequence: u64) -> Result<Vec<SessionLiveness>> {
        self.query(|store| store.session_liveness(through_sequence))
    }

    /// One session's records at or below `through_sequence`, rebuilt from the
    /// projection so resume need not read the event log before the cursor.
    pub fn session_records(
        &self,
        session_id: SessionId,
        through_sequence: u64,
    ) -> Result<Vec<crate::persistence::event_log::Record>> {
        self.query(|store| store.session_records(session_id, through_sequence))
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
    failure: Option<String>,
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
        Ok(Self {
            conn,
            failure: None,
        })
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
