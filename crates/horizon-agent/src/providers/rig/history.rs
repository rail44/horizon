use super::conversation::ConversationHistory;

use crate::contract::{Event, OccurrenceId};

use super::clearing::cleared_occurrence_ids_from_events;

/// Everything a resumed session has to rebuild from its persisted events:
/// the canonical Rig history, plus the Tier 1 cleared set frozen by whatever
/// clearing passes already ran (`super::clearing`'s module doc). The two are
/// deliberately separate -- history is reloaded in full, and the cleared set
/// is re-applied on top of it as a projection, so a resumed session sends
/// the provider exactly what a continuously-running one would.
#[derive(Debug, Default)]
pub(super) struct RigSessionHistory {
    pub(super) messages: ConversationHistory,
    pub(super) cleared_occurrence_ids: Vec<OccurrenceId>,
}

/// Rebuilds a session's prior history from the events the host hands it in
/// `StartSession::history`: the session's own event-log records on a
/// resume, nothing on a fresh spawn (which starts with an empty history).
/// The log is authoritative (`docs/agent-persistence-contract.md`), so the
/// DuckDB projection is not consulted.
pub(super) fn load_rig_session_history(events: &[Event]) -> Result<RigSessionHistory, String> {
    Ok(RigSessionHistory {
        messages: ConversationHistory::from_events(events)?,
        cleared_occurrence_ids: cleared_occurrence_ids_from_events(events),
    })
}
