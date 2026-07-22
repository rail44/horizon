use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use uuid::Uuid;

use crate::contract::{event_kind, ProviderEvent, ProviderId, SessionId};
use crate::roles::RoleId;

use super::{
    PersistedSessionContext, Record, TurnTracker, WriterHandle, AGENT_EVENT_LOG_SCHEMA,
    AGENT_EVENT_LOG_VERSION,
};

pub struct Appender {
    writer: WriterHandle,
    session_id: SessionId,
    provider_id: Option<ProviderId>,
    role_id: Option<RoleId>,
    session_context: Option<PersistedSessionContext>,
    turn_tracker: TurnTracker,
}

impl Appender {
    pub fn new(
        writer: WriterHandle,
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        role_id: Option<RoleId>,
    ) -> Self {
        Self {
            writer,
            session_id,
            provider_id,
            role_id,
            session_context: None,
            turn_tracker: TurnTracker::new(),
        }
    }

    /// Stamps the same host-authored placement metadata onto every later
    /// record. Callers which do not know a context (legacy fixtures and
    /// compatibility tests) keep emitting the pre-field envelope.
    pub fn with_session_context(mut self, session_context: PersistedSessionContext) -> Self {
        self.session_context = Some(session_context);
        self
    }

    pub fn append_provider_events(&mut self, events: Vec<ProviderEvent>) -> Result<()> {
        for event in events {
            let turn_id = self.turn_tracker.turn_id_for_event(&event.event);
            let record = Record {
                schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: AGENT_EVENT_LOG_VERSION,
                event_id: Uuid::new_v4().to_string(),
                sequence: 0,
                session_id: self.session_id,
                turn_id,
                provider_id: self.provider_id.clone(),
                role_id: self.role_id.clone(),
                session_context: self.session_context.clone(),
                event_kind: event_kind(&event.event).to_string(),
                event: event.event,
                provider_payload: event.provider_payload,
                created_at_unix_ms: unix_time_ms(),
            };
            self.writer.append(record)?;
        }
        Ok(())
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
