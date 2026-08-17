use rig_core::completion::Message;

use crate::contract::{Event, SessionId, ToolCallId};
use crate::persistence::projection::duckdb::DuckdbStoreHandle;

use super::clearing::cleared_call_ids_from_events;
use super::mapping::rig_messages_from_horizon_events;
use crate::tools::{memory_document_from_events, MemoryDocument};

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
    /// The standing-agent memory document replayed from the same events —
    /// `None` when no `MemoryDigest` events were found (a non-standing session,
    /// or a standing session that has never updated its memory).
    pub(super) memory_document: Option<MemoryDocument>,
    /// `true` when the memory document was built from `fallback_events`
    /// (a cross-session seed, board #39/#41) rather than from this
    /// session's own DuckDB events. Only set in the store path, where the
    /// session's own empty event set confirms this is a fresh spawn being
    /// seeded — not a same-session resume.
    pub(super) seed_from_fallback: bool,
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
            memory_document: memory_document_from_events_if_nonempty(fallback_events),
            seed_from_fallback: false,
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
            // A cross-session seed (board #39's wake-action v2 seed-spawn
            // path) supplies the prior keeper's MemoryDigest events as
            // `fallback_events` even though this session's own DuckDB row
            // set is empty. The messages/cleared-call-ids come from this
            // session's own events (none for a fresh spawn); the memory
            // document is the seed's, unless the session already has one.
            let mut memory_document = memory_document_from_events_if_nonempty(&events);
            let memory_from_fallback = memory_document.is_none();
            if memory_document.is_none() {
                memory_document = memory_document_from_events_if_nonempty(fallback_events);
            }
            let seed_from_fallback = memory_from_fallback && memory_document.is_some();
            RigSessionHistory {
                messages: rig_messages_from_horizon_events(&events),
                cleared_call_ids: cleared_call_ids_from_events(&events),
                memory_document,
                seed_from_fallback,
            }
        })
        .unwrap_or_else(|_| {
            // DuckDB query failed: fall back entirely to `fallback_events`,
            // same as the no-store path above.
            if fallback_events.is_empty() {
                return RigSessionHistory::default();
            }
            RigSessionHistory {
                messages: rig_messages_from_horizon_events(fallback_events),
                cleared_call_ids: cleared_call_ids_from_events(fallback_events),
                memory_document: memory_document_from_events_if_nonempty(fallback_events),
                seed_from_fallback: false,
            }
        })
}

/// Replays `MemoryDigest` events into a document, returning `None` when the
/// result is empty (no digest events, or all were no-update declarations) so
/// the session loop can skip the projection for a session that has never
/// updated its memory.
fn memory_document_from_events_if_nonempty(events: &[Event]) -> Option<MemoryDocument> {
    let document = memory_document_from_events(events);
    if document.is_empty() {
        None
    } else {
        Some(document)
    }
}
