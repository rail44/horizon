//! The check a log must pass before it is used as history: envelopes,
//! identities, order, and each rig session's conversation records.
use super::*;
use serde_json::Value;
use std::collections::HashSet;

/// Validate the exact decoder, event envelopes, conversation codec and identities.
pub fn validate_conversation_records(rows: &[Value]) -> Result<(), String> {
    let mut sessions: HashMap<crate::contract::SessionId, (bool, Vec<Event>)> = HashMap::new();
    let mut ids = HashSet::new();
    let mut previous = None;
    for row in rows {
        let record: crate::persistence::event_log::Record =
            serde_json::from_value(row.clone()).map_err(|error| error.to_string())?;
        if record.schema != "horizon.agent.event_log"
            || record.version != 4
            || record.event_id.is_empty()
            || !ids.insert(record.event_id)
            || previous.is_some_and(|prior| record.sequence <= prior)
        {
            return Err("Invalid event envelope, duplicate identity or unordered sequence".into());
        }
        previous = Some(record.sequence);
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
