use rig_core::completion::Message;

use crate::agent::persistence::projection::duckdb;
use crate::session::SessionId;

use super::rig_messages_from_horizon_events;

pub(super) fn load_rig_history(
    path: Option<&std::path::Path>,
    session_id: SessionId,
) -> Vec<Message> {
    let Some(path) = path else {
        return Vec::new();
    };

    duckdb::Store::open(path)
        .and_then(|store| {
            let events = store
                .events_for_session(session_id)?
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>();
            Ok(rig_messages_from_horizon_events(&events))
        })
        .unwrap_or_default()
}
