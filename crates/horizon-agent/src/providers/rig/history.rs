use rig_core::completion::Message;

use crate::contract::SessionId;
use crate::persistence::projection::duckdb::DuckdbStoreHandle;

use super::mapping::rig_messages_from_horizon_events;

/// Loads this session's prior history (if any) as Rig messages, through the
/// *shared* DuckDB store handle -- never a fresh `Store::open` of the same
/// path. A second, independent open of the same file is unsound here: with
/// DuckDB's relaxed durability, the writer thread's own committed appends
/// can sit in *that instance's* in-memory WAL well before landing in the
/// on-disk file (`duckdb-rs`'s `Connection::open` has no cross-instance
/// cache -- see `persistence::projection::duckdb::SharedDuckdbStore`'s doc
/// comment), so a second instance opened here can read a stale, possibly
/// zero-row view -- confirmed in practice for a resumed session with real
/// history. `store` is `None` when no DuckDB projection is configured for
/// this process (or it failed to open/rebuild): callers get an empty
/// history exactly as before, never a panic or a stale read.
pub(super) fn load_rig_history(
    store: Option<&DuckdbStoreHandle>,
    session_id: SessionId,
) -> Vec<Message> {
    let Some(store) = store else {
        return Vec::new();
    };

    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .events_for_session(session_id)
        .map(|records| {
            let events = records
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>();
            rig_messages_from_horizon_events(&events)
        })
        .unwrap_or_default()
}
