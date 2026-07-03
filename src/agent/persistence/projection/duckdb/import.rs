use anyhow::{Context, Result};
use duckdb::params;

use crate::agent::persistence::event_log::Record;

use super::{
    projection::EventRecordRef, schema::CLEAR_ALL_AGENT_STATE_SQL, session_id_text, Store,
};

impl Store {
    pub(crate) fn replace_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<()> {
        self.clear_all_agent_state()?;
        for record in records {
            self.insert_event_log_record(record)?;
        }
        Ok(())
    }

    fn clear_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(CLEAR_ALL_AGENT_STATE_SQL)?;
        Ok(())
    }

    fn insert_event_log_record(&self, record: Record) -> Result<()> {
        let session_id_text = session_id_text(record.session_id)?;
        let sequence = i64::try_from(record.sequence).context("agent event sequence overflow")?;
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
                &record.event_id,
                &session_id_text,
                record.turn_id.as_deref(),
                sequence,
                &record.event_kind,
                &event_json,
                provider_id_text.as_deref(),
                provider_payload_json.as_deref(),
            ],
        )?;

        self.upsert_session(&session_id_text, provider_id_text.as_deref(), sequence)?;
        self.project_event(EventRecordRef {
            event_id: &record.event_id,
            session_id: &session_id_text,
            sequence,
            event: &record.event,
        })?;
        Ok(())
    }
}
