use anyhow::Result;
use duckdb::params;

#[cfg(test)]
use crate::contract::SessionId;
use crate::contract::{
    ApprovalRequest, Event, Message, MessageDelta, MessageRole, ToolCallRequest, ToolCallResult,
};

use super::Store;
#[cfg(test)]
use super::{schema::PROJECTION_TABLES, session_id_text};

pub(super) struct EventRecordRef<'a> {
    pub(super) event_id: &'a str,
    pub(super) session_id: &'a str,
    pub(super) sequence: i64,
    pub(super) event: &'a Event,
}

impl Store {
    #[cfg(test)]
    pub fn rebuild_projections(&self) -> Result<()> {
        for session in self.sessions()? {
            self.rebuild_projections_for_session(session.session_id)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn rebuild_projections_for_session(&self, session_id: SessionId) -> Result<()> {
        let session_id_text = session_id_text(session_id)?;
        let events = self.events_for_session(session_id)?;
        self.clear_projections_for_session(&session_id_text)?;
        for record in events {
            self.project_event(EventRecordRef {
                event_id: &record.event_id,
                session_id: &session_id_text,
                sequence: record.sequence,
                event: &record.event,
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn clear_projections_for_session(&self, session_id: &str) -> Result<()> {
        for table in PROJECTION_TABLES {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?"),
                params![session_id],
            )?;
        }
        Ok(())
    }

    pub(super) fn project_event(&self, record: EventRecordRef) -> Result<()> {
        let EventRecordRef {
            event_id,
            session_id,
            sequence,
            event,
        } = record;
        match event {
            Event::MessageCommitted(message) => {
                self.insert_message(event_id, session_id, sequence, message, false)
            }
            Event::ReasoningDelta(delta) | Event::AssistantTextDelta(delta) => {
                self.insert_delta(event_id, session_id, sequence, delta)
            }
            Event::ToolCallRequested(request) => {
                self.insert_tool_call(event_id, session_id, sequence, request)
            }
            Event::ToolCallFinished(result) => {
                self.insert_tool_result(event_id, session_id, sequence, result)
            }
            Event::ApprovalRequested(request) => {
                self.insert_approval(event_id, session_id, sequence, request)
            }
            // No projection table wants these yet: they're timing markers
            // for replay/inspection (see their doc comments on `Event`),
            // not transcript/tool/approval state. They still land in
            // `agent_events` via the caller's insert before `project_event`
            // runs, so `agent_events` remains the durable source a future
            // projection could be built from.
            Event::StateChanged(_)
            | Event::ToolCallStarted(_)
            | Event::ProviderRequestSent(_)
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::Error(_)
            | Event::Exited(_)
            | Event::TurnEnded(_) => Ok(()),
        }
    }

    fn insert_message(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        message: &Message,
        is_delta: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_messages (event_id, session_id, sequence, role, text, is_delta)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                role_text(message.role),
                &message.text,
                is_delta,
            ],
        )?;
        Ok(())
    }

    fn insert_delta(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        delta: &MessageDelta,
    ) -> Result<()> {
        self.insert_message(
            event_id,
            session_id,
            sequence,
            &Message {
                role: delta.role,
                text: delta.text.clone(),
            },
            true,
        )
    }

    fn insert_tool_call(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        request: &ToolCallRequest,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_tool_calls (
                event_id,
                session_id,
                sequence,
                call_id,
                tool_id,
                input_json
             ) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                &request.tool_id,
                serde_json::to_string(&request.input)?,
            ],
        )?;
        Ok(())
    }

    fn insert_tool_result(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        result: &ToolCallResult,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_tool_results (
                event_id,
                session_id,
                sequence,
                call_id,
                output_json
             ) VALUES (?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &result.call_id.0,
                serde_json::to_string(&result.output)?,
            ],
        )?;
        Ok(())
    }

    fn insert_approval(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        request: &ApprovalRequest,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_approvals (event_id, session_id, sequence, call_id, reason)
             VALUES (?, ?, ?, ?, ?)",
            params![
                event_id,
                session_id,
                sequence,
                &request.call_id.0,
                &request.reason,
            ],
        )?;
        Ok(())
    }
}

fn role_text(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
