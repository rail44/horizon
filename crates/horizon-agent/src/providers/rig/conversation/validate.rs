//! The check a log must pass before it is used as history: envelopes,
//! identities, order, and each rig session's conversation records.
use super::*;
use serde_json::Value;
use std::collections::HashSet;

/// Validate the exact decoder, event envelopes, conversation codec and
/// identities, and that each session's records after its first name the
/// record they follow (the judge's verdicts are written beside the chain).
pub fn validate_conversation_records(rows: &[Value]) -> Result<(), String> {
    let mut sessions: HashMap<crate::contract::SessionId, (bool, Vec<Event>)> = HashMap::new();
    let mut ids = HashSet::new();
    let mut previous = None;
    let mut chained = HashSet::new();
    for row in rows {
        let record: crate::persistence::event_log::Record =
            serde_json::from_value(row.clone()).map_err(|error| error.to_string())?;
        if record.schema != "horizon.agent.event_log"
            || record.version != crate::persistence::event_log::AGENT_EVENT_LOG_VERSION
            || record.event_id.is_empty()
            || !ids.insert(record.event_id.clone())
            || previous.is_some_and(|prior| record.sequence <= prior)
        {
            return Err("Invalid event envelope, duplicate identity or unordered sequence".into());
        }
        previous = Some(record.sequence);
        if record
            .parent_id
            .as_ref()
            .is_some_and(|parent| !ids.contains(parent))
        {
            return Err(format!(
                "Record {} follows no earlier record",
                record.event_id
            ));
        }
        if record.event_kind != crate::judge::JUDGE_VERDICT_EVENT_KIND
            && !chained.insert(record.session_id)
            && record.parent_id.is_none()
        {
            return Err(format!(
                "Record {} of session {:?} names no record it follows",
                record.event_id, record.session_id
            ));
        }
        let session = sessions.entry(record.session_id).or_default();
        session.0 |= record
            .provider_id
            .as_ref()
            .is_some_and(|id| id.0 == "builtin.agent.rig")
            || matches!(record.event, Event::ConversationRecorded(_));
        session.1.push(record.event);
    }
    for (session, (rig, events)) in sessions {
        if rig {
            ConversationHistory::from_events(&events)
                .map_err(|error| format!("Session {session:?}: {error}"))?;
        }
    }
    Ok(())
}
