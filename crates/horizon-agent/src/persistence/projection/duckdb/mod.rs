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

    /// Migrates a table shape from an older Horizon build so the `CREATE
    /// TABLE IF NOT EXISTS` in [`INITIALIZE_SCHEMA_SQL`] can lay down the
    /// current schema. `CREATE TABLE IF NOT EXISTS` is additive-only and
    /// never alters an existing table, and DuckDB (confirmed against the
    /// bundled 1.10504.0) rejects `ALTER TABLE ... ADD COLUMN` with an
    /// inline `NOT NULL` constraint ("Adding columns with constraints not
    /// yet supported"), so a plain `ADD COLUMN IF NOT EXISTS` can't get us
    /// to e.g. `agent_tool_results.is_error BOOLEAN NOT NULL` either.
    /// Dropping a stale table and letting `CREATE TABLE IF NOT EXISTS`
    /// recreate it is cheap and correct specifically *because* the whole
    /// projection is rebuildable-by-construction from the JSONL log: every
    /// caller of this method immediately runs `INITIALIZE_SCHEMA_SQL` and
    /// then, if this returned `true`, a full `replace_from_event_log_records`
    /// (see [`Self::migrated_legacy_schema`]'s callers) that repopulates
    /// every dropped table's rows from the source of truth. Extend this
    /// function -- one check + one drop per outdated shape -- whenever a
    /// future column/table is added, rather than writing an in-place
    /// `ALTER TABLE` migration.
    ///
    /// Returns whether *any* migration ran -- `true` both for a genuine
    /// legacy file and for a brand-new one (where these tables don't exist
    /// yet either), which is harmless: [`Self::migrated_legacy_schema`]'s
    /// one caller only uses `true` to skip an optimization (trusting a
    /// freshness check), never to skip correctness work.
    fn migrate_legacy_agent_events_schema(conn: &Connection) -> Result<bool> {
        let mut migrated = false;

        if !column_exists(conn, "agent_events", "event_at")?
            || !column_exists(conn, "agent_events", "role_id")?
        {
            conn.execute_batch("DROP TABLE IF EXISTS agent_events;")?;
            migrated = true;
        }
        if !column_exists(conn, "agent_sessions", "role_id")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_sessions;")?;
            migrated = true;
        }
        if !column_exists(conn, "agent_tool_results", "is_error")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_tool_results;")?;
            migrated = true;
        }
        if !column_exists(conn, "agent_approvals", "outcome")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_approvals;")?;
            migrated = true;
        }
        // `occurrence_id` was added in the same leg as
        // `SESSION_PROTOCOL_VERSION = 15` (still v15 -- the wire change
        // is additive, see `backlog 42 / 55`). A pre-existing DB won't
        // have the column; same drop-and-rebuild pattern as the rows
        // above, so the next `CREATE TABLE IF NOT EXISTS` lays down the
        // new shape and `replace_from_event_log_records` repopulates it
        // from the JSONL log.
        if !column_exists(conn, "agent_tool_calls", "occurrence_id")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_tool_calls;")?;
            migrated = true;
        }
        if !column_exists(conn, "agent_tool_results", "occurrence_id")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_tool_results;")?;
            migrated = true;
        }
        if !column_exists(conn, "agent_approvals", "occurrence_id")? {
            conn.execute_batch("DROP TABLE IF EXISTS agent_approvals;")?;
            migrated = true;
        }
        if !table_exists(conn, "agent_turns")? {
            // Nothing to drop -- a missing table is simply laid down fresh
            // by `INITIALIZE_SCHEMA_SQL` -- but a brand-new `agent_turns`
            // still needs the forced full rebuild `migrated = true` triggers
            // to backfill it from the existing JSONL log.
            migrated = true;
        }

        Ok(migrated)
    }
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_name = ? AND column_name = ?",
        duckdb::params![table, column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = ?",
        duckdb::params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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
