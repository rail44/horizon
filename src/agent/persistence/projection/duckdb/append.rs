#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use duckdb::params;
#[cfg(test)]
use duckdb::OptionalExt;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::agent::contract::{event_kind, Event, ProviderId};
#[cfg(test)]
use crate::session::SessionId;

use super::Store;
#[cfg(test)]
use super::{projection::EventRecordRef, session_id_text, AgentStoredEvent, AppendEvent};

impl Store {
    #[cfg(test)]
    pub(crate) fn append_event(&self, record: AppendEvent) -> Result<AgentStoredEvent> {
        let session_id_text = session_id_text(record.session_id)?;
        let sequence = self.next_sequence(&session_id_text)?;
        let event_id = Uuid::new_v4().to_string();
        let event_kind = event_kind(&record.event).to_string();
        let event_json = serde_json::to_string(&record.event).context("serialize agent event")?;
        let provider_id_text = record.provider_id.as_ref().map(|id| id.0.clone());
        let provider_payload_json = record
            .provider_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize provider payload")?;

        self.conn.execute(
            "INSERT INTO agent_events (
                event_id,
                session_id,
                turn_id,
                sequence,
                event_kind,
                horizon_event_json,
                provider_id,
                provider_payload_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &event_id,
                &session_id_text,
                record.turn_id.as_deref(),
                sequence,
                &event_kind,
                &event_json,
                provider_id_text.as_deref(),
                provider_payload_json.as_deref(),
            ],
        )?;

        self.upsert_session(&session_id_text, provider_id_text.as_deref(), sequence)?;
        self.project_event(EventRecordRef {
            event_id: &event_id,
            session_id: &session_id_text,
            sequence,
            event: &record.event,
        })?;

        Ok(AgentStoredEvent {
            event_id,
            session_id: record.session_id,
            turn_id: record.turn_id,
            sequence,
            event_kind,
            event: record.event,
            provider_id: record.provider_id,
            provider_payload: record.provider_payload,
        })
    }

    #[cfg(test)]
    pub(crate) fn append_events(
        &self,
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        events: impl IntoIterator<Item = Event>,
    ) -> Result<Vec<AgentStoredEvent>> {
        events
            .into_iter()
            .map(|event| {
                self.append_event(AppendEvent {
                    session_id,
                    turn_id: None,
                    provider_id: provider_id.clone(),
                    event,
                    provider_payload: None,
                })
            })
            .collect()
    }

    #[cfg(test)]
    fn next_sequence(&self, session_id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0)
                 FROM agent_events
                 WHERE session_id = ?",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .context("query next agent event sequence")
    }

    pub(super) fn upsert_session(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        last_sequence: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_sessions (session_id, provider_id, last_sequence, updated_at)
             VALUES (?, ?, ?, now())
             ON CONFLICT (session_id) DO UPDATE SET
                provider_id = COALESCE(excluded.provider_id, agent_sessions.provider_id),
                last_sequence = excluded.last_sequence,
                updated_at = now()",
            params![session_id, provider_id, last_sequence],
        )?;
        Ok(())
    }
}
