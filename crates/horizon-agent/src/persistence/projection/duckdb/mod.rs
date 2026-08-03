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
    /// Whether opening this store had to migrate a pre-`event_at`
    /// `agent_events` table (see [`Self::migrate_legacy_agent_events_schema`]).
    /// Not test-only: `horizon-agentd`'s startup rebuild-skip check
    /// (task 2 of the readiness fix) reads this via [`Self::
    /// migrated_legacy_schema`] to know it must not trust the projection's
    /// existing `agent_sessions.last_sequence` high-water mark -- a
    /// migration just dropped and recreated `agent_events` (losing its
    /// rows) without touching `agent_sessions`, so that table's numbers
    /// would otherwise look deceptively "current" against an now-empty
    /// projection.
    migrated_legacy_schema: bool,
}

impl Store {
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().context("open in-memory DuckDB agent store")?,
        )
    }

    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path).context("open DuckDB agent store")?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        let migrated_legacy_schema = Self::migrate_legacy_agent_events_schema(&conn)?;
        conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        Ok(Self {
            conn,
            migrated_legacy_schema,
        })
    }

    /// See the field's doc comment on [`Self::migrated_legacy_schema`].
    pub(crate) fn migrated_legacy_schema(&self) -> bool {
        self.migrated_legacy_schema
    }

    /// Extension point for a future schema change that `CREATE TABLE IF
    /// NOT EXISTS` in [`INITIALIZE_SCHEMA_SQL`] cannot express on its own:
    /// that statement is additive-only and never alters an existing table,
    /// and DuckDB (confirmed against the bundled 1.10504.0) rejects `ALTER
    /// TABLE ... ADD COLUMN` with an inline `NOT NULL` constraint ("Adding
    /// columns with constraints not yet supported"), so a plain `ADD
    /// COLUMN IF NOT EXISTS` cannot get us to e.g. a new `NOT NULL` column
    /// either. Dropping a stale table and letting `CREATE TABLE IF NOT
    /// EXISTS` recreate it is cheap and correct specifically *because* the
    /// whole projection is rebuildable-by-construction from the JSONL log:
    /// every caller of this method immediately runs `INITIALIZE_SCHEMA_SQL`
    /// and then, if this returns `true`, a full
    /// `replace_from_event_log_records` (see [`Self::migrated_legacy_schema`]'s
    /// callers) that repopulates every dropped table's rows from the source
    /// of truth. Extend this function -- a shape check (e.g. querying
    /// `information_schema.columns`/`.tables`) plus one `DROP TABLE IF
    /// EXISTS` per outdated shape -- whenever a future column/table needs
    /// the same treatment, rather than writing an in-place `ALTER TABLE`
    /// migration.
    ///
    /// This project carries no on-disk schema compatibility by default
    /// (owner decision 2026-08-03): the shape checks this function used to
    /// run for the pre-`event_at`/pre-label/pre-`occurrence_id` DuckDB
    /// projections were retired with the rest of the compat sweep, since a
    /// stale `.duckdb` file is expected to be rotated rather than migrated
    /// forward. The function stays as the seam the *next* genuine schema
    /// change reaches for.
    fn migrate_legacy_agent_events_schema(_conn: &Connection) -> Result<bool> {
        Ok(false)
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
