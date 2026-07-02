use anyhow::Result;
use duckdb::params;

use crate::agent::contract::{
    ApprovalRequest, Event, Message, MessageDelta, MessageRole, ToolCallRequest, ToolCallResult,
};
#[cfg(test)]
use crate::session::SessionId;

use super::Store;
#[cfg(test)]
use super::{schema::PROJECTION_TABLES, session_id_text};

impl Store {
    #[cfg(test)]
    pub(crate) fn rebuild_projections(&self) -> Result<()> {
        for session in self.sessions()? {
            self.rebuild_projections_for_session(session.session_id)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rebuild_projections_for_session(&self, session_id: SessionId) -> Result<()> {
        let session_id_text = session_id_text(session_id)?;
        let events = self.events_for_session(session_id)?;
        self.clear_projections_for_session(&session_id_text)?;
        for record in events {
            self.project_event(
                &record.event_id,
                &session_id_text,
                record.turn_id.as_deref(),
                record.provider_id.as_ref().map(|id| id.0.as_str()),
                record.sequence,
                &record.event_kind,
                &record.event,
            )?;
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

    pub(super) fn project_event(
        &self,
        event_id: &str,
        session_id: &str,
        _turn_id: Option<&str>,
        _provider_id: Option<&str>,
        sequence: i64,
        _event_kind: &str,
        event: &Event,
    ) -> Result<()> {
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
            Event::StateChanged(_)
            | Event::ToolCallStarted(_)
            | Event::Error(_)
            | Event::Exited(_) => Ok(()),
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
