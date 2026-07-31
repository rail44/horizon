use rig_core::completion::Message;

use crate::contract::{Event, SessionId, ToolCallId};
use crate::persistence::projection::duckdb::DuckdbStoreHandle;

use super::clearing::cleared_call_ids_from_events;
use super::mapping::rig_messages_from_horizon_events;

/// Everything a resumed session has to rebuild from its persisted events:
/// the canonical Rig history, plus the Tier 1 cleared set frozen by whatever
/// clearing passes already ran (`super::clearing`'s module doc). The two are
/// deliberately separate -- history is reloaded in full, and the cleared set
/// is re-applied on top of it as a projection, so a resumed session sends
/// the provider exactly what a continuously-running one would.
#[derive(Debug, Default)]
pub(super) struct RigSessionHistory {
    pub(super) messages: Vec<Message>,
    pub(super) cleared_call_ids: Vec<ToolCallId>,
}

/// Loads this session's prior history (if any) as Rig messages, through the
/// *shared* DuckDB store handle -- never a fresh `Store::open` of the same
/// path. A second, independent open of the same file is unsound here: with
/// DuckDB's relaxed durability, the writer thread's own committed appends
/// can sit in *that instance's* in-memory WAL well before landing in the
/// on-disk file (`duckdb-rs`'s `Connection::open` has no cross-instance
/// cache -- see `persistence::projection::duckdb::SharedDuckdbStore`'s doc
/// comment), so a second instance opened here can read a stale, possibly
/// zero-row view -- confirmed in practice for a resumed session with real
/// history.
///
/// `store` is `None` when the DuckDB projection failed to open or rebuild
/// (issue 012: lock contention, corrupt file, etc.). Before the fix this
/// returned an empty history silently, so a resumed session answered as if
/// the conversation never happened while the UI transcript stayed complete.
/// Now `fallback_events` -- the JSONL event log's events the resume path
/// already holds and threads through `StartSession::history` -- are used to
/// rebuild the same history the store would have provided, using the same
/// `rig_messages_from_horizon_events` / `cleared_call_ids_from_events`
/// reconstruction. The store path (when `Some`) is unchanged: a live
/// session's normal load still goes through DuckDB. `fallback_events` being
/// empty (a fresh `Control::SessionNew`, or persistence genuinely
/// unavailable in a test) keeps the original empty-history return.
pub(super) fn load_rig_session_history(
    store: Option<&DuckdbStoreHandle>,
    session_id: SessionId,
    fallback_events: &[Event],
) -> RigSessionHistory {
    let Some(store) = store else {
        if fallback_events.is_empty() {
            return RigSessionHistory::default();
        }
        return RigSessionHistory {
            messages: rig_messages_from_horizon_events(fallback_events),
            cleared_call_ids: cleared_call_ids_from_events(fallback_events),
        };
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
            RigSessionHistory {
                messages: rig_messages_from_horizon_events(&events),
                cleared_call_ids: cleared_call_ids_from_events(&events),
            }
        })
        .unwrap_or_default()
}
