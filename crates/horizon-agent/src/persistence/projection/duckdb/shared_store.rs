use std::sync::{Arc, Condvar, Mutex};

use super::DuckdbStoreHandle;

/// A one-time-settable, multi-reader-blocking handle onto the live DuckDB
/// projection `Store` that this process's event-log writer thread opens
/// (see `event_log::writer`'s doc comments) -- shared between
/// `horizon-sessiond`'s `SessiondState` (the recall tools' context, via
/// `tools::ToolSessionState`/`RecallContext`) and the rig provider
/// (`providers::rig`'s `load_rig_history`), both of which need "block until
/// the writer thread's own rebuild-or-open decision lands, then read the
/// result any number of times from any number of threads".
///
/// **Why not just a `crossbeam_channel::Receiver`.** A channel's message is
/// delivered to whichever single `recv()` call happens to win the race;
/// every *other* caller (a different session's rig-provider thread, a
/// later resumed session, etc.) would get nothing. This cell instead holds
/// the decided value behind a `Mutex`/`Condvar` pair so `wait()` can be
/// called as many times, from as many threads, as needed, and each call
/// sees the same answer once it exists.
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
/// `Store` behind this cell (and one `Mutex` serializing access to it,
/// since write volume is low -- roughly 1-2 events/s -- contention is a
/// non-issue) is the only sound way to give more than one part of the
/// process a live view of the projection.
#[derive(Clone)]
pub struct SharedDuckdbStore {
    // The outer `Option` is "has `set` run yet"; the inner `Option` is the
    // decided value itself (`None` if there's nothing to share). Deliberately
    // structured this way rather than factored further -- it's a private
    // field with exactly one reader/writer pair (`wait`/`set` below).
    #[allow(clippy::type_complexity)]
    inner: Arc<(Mutex<Option<Option<DuckdbStoreHandle>>>, Condvar)>,
}

impl Default for SharedDuckdbStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedDuckdbStore {
    /// An empty, not-yet-decided cell -- for the one real production
    /// instance shared between `horizon-sessiond`'s `SessiondState` and the rig
    /// provider, populated later via [`Self::set`] once the event-log
    /// writer thread's own rebuild-or-open decision lands.
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(None), Condvar::new())),
        }
    }

    /// An already-resolved cell with no store, for any construction path
    /// with no real event-log writer behind it (this crate's own tests,
    /// `ProviderRegistry::builtin`'s test helper) -- [`Self::wait`] on this
    /// returns `None` immediately and never blocks, exactly like the
    /// pre-recall behavior of a provider constructed with no DuckDB path
    /// at all.
    pub fn unavailable() -> Self {
        let cell = Self::new();
        cell.set(None);
        cell
    }

    /// Populates the cell -- called exactly once in production, right
    /// after the event-log writer thread's own rebuild-or-open decision is
    /// known (see `event_log::writer`'s doc comments) -- and wakes every
    /// thread blocked in [`Self::wait`].
    pub fn set(&self, store: Option<DuckdbStoreHandle>) {
        let (lock, condvar) = &*self.inner;
        let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(store);
        condvar.notify_all();
    }

    /// Blocks the calling thread until [`Self::set`] has run (a no-op wait
    /// if it already has), then returns the decided value. Safe to call
    /// from any number of concurrent threads.
    pub fn wait(&self) -> Option<DuckdbStoreHandle> {
        let (lock, condvar) = &*self.inner;
        let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while guard.is_none() {
            guard = condvar
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        guard.clone().flatten()
    }
}
