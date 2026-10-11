use std::sync::{Arc, Mutex};

use super::DuckdbStoreHandle;

/// One shared slot holding the live DuckDB projection `Store` that this
/// process's event-log writer thread opens (see `event_log::writer`'s doc
/// comments) -- shared between `horizon-agentd`'s `AgentdState` (the recall
/// tools' context, via `tools::ToolSessionState`/`RecallContext`) and the
/// rig provider (`providers::rig`'s `load_rig_history`).
///
/// Readers never block on it: each use reads the current decision with
/// [`Self::current`] -- still [`DuckdbStoreState::Pending`] while the
/// writer's first full rebuild runs, then the same `Ready`/`Unavailable`
/// answer for every reader from every thread once [`Self::set`] has run
/// (owner decision, 2026-10-11: session start and conversation proceed
/// during a rebuild, and recall says it is still being rebuilt).
///
/// **Why not just a `crossbeam_channel::Receiver`.** A channel's message is
/// delivered to whichever single `recv()` call happens to win the race;
/// every *other* caller (a different session's rig-provider thread, a
/// later resumed session, etc.) would get nothing. This slot keeps the
/// decided value so any number of readers see the same answer.
///
/// **Why a real database instance can't just be shared by opening it
/// twice.** DuckDB's own connection-level instance cache (keyed by
/// resolved file path) is a common source of confusion here: `duckdb-rs`'s
/// `Connection::open` calls `duckdb_open_ext` directly on every call and
/// does not consult or populate any such cache (the cache-shaped type in
/// its own source, `StatementCache`, is a prepared-*statement* cache
/// per-connection, unrelated). Two independent `Store::open` calls against
/// the same path in the same process are two independent, uncoordinated
/// database instances sharing one file unsafely -- confirmed by an actual
/// production symptom: with DuckDB's relaxed durability, a writer
/// instance's committed appends can sit in *that instance's own* in-memory
/// WAL with nothing yet reflected in the on-disk file (no `.duckdb.wal`
/// sibling appears either), so a second instance opened against the same
/// path reads however-stale the file happens to be -- observed as low as
/// zero rows for a session with a real, substantial history. One shared
/// `Store` behind this slot (and one `Mutex` serializing access to it,
/// since write volume is low -- roughly 1-2 events/s -- contention is a
/// non-issue) is the only sound way to give more than one part of the
/// process a live view of the projection.
#[derive(Clone)]
pub struct SharedDuckdbStore {
    inner: Arc<Mutex<DuckdbStoreState>>,
}

/// The slot's current decision (see [`SharedDuckdbStore::current`]).
#[derive(Clone)]
pub enum DuckdbStoreState {
    /// The writer has not decided yet: a full rebuild is still running.
    Pending,
    Ready(DuckdbStoreHandle),
    /// No projection this run: none configured, or it could not be opened,
    /// rebuilt, or caught up completely.
    Unavailable,
}

impl SharedDuckdbStore {
    /// An undecided slot -- for the one real production instance shared
    /// between `horizon-agentd`'s `AgentdState` and the rig provider,
    /// populated later via [`Self::set`] once the event-log writer thread's
    /// decision lands.
    pub fn pending() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DuckdbStoreState::Pending)),
        }
    }

    /// An already-decided slot with no store, for any construction path
    /// with no real event-log writer behind it (this crate's own tests,
    /// `ProviderRegistry::builtin`'s test helper, `RecallContext::default`).
    pub fn unavailable() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DuckdbStoreState::Unavailable)),
        }
    }

    /// Records the decision -- called exactly once in production, right
    /// after the event-log writer thread's decision is known (see
    /// `event_log::writer`'s doc comments).
    pub fn set(&self, store: Option<DuckdbStoreHandle>) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = store.map_or(DuckdbStoreState::Unavailable, DuckdbStoreState::Ready);
    }

    /// The current decision, without waiting for one. Read it per use:
    /// a reader that saw `Pending` sees the final answer on its next read.
    pub fn current(&self) -> DuckdbStoreState {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_reports_pending_until_set_then_the_decision() {
        let slot = SharedDuckdbStore::pending();
        let reader = slot.clone();
        assert!(matches!(reader.current(), DuckdbStoreState::Pending));

        let store = DuckdbStoreHandle::new(super::super::Store::open_in_memory().unwrap());
        slot.set(Some(store));
        assert!(matches!(reader.current(), DuckdbStoreState::Ready(_)));

        let unavailable = SharedDuckdbStore::pending();
        unavailable.set(None);
        assert!(matches!(
            unavailable.current(),
            DuckdbStoreState::Unavailable
        ));
        assert!(matches!(
            SharedDuckdbStore::unavailable().current(),
            DuckdbStoreState::Unavailable
        ));
    }
}
