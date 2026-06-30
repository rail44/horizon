use anyhow::{Context, Result};
use duckdb::params;

use crate::agent::{
    AgentApprovalRequest, AgentEvent, AgentMessage, AgentMessageDelta, AgentMessageRole,
    AgentToolCallRequest, AgentToolCallResult,
};
use crate::workspace::SessionId;

use super::{schema::PROJECTION_TABLES, session_id_text, DuckDbAgentStateStore};

impl DuckDbAgentStateStore {
    pub fn rebuild_projections(&self) -> Result<()> {
        for session in self.sessions()? {
            self.rebuild_projections_for_session(session.session_id)?;
        }
        Ok(())
    }

    pub fn rebuild_projections_for_session(&self, session_id: SessionId) -> Result<()> {
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
        turn_id: Option<&str>,
        provider_id: Option<&str>,
        sequence: i64,
        event_kind: &str,
        event: &AgentEvent,
    ) -> Result<()> {
        match event {
            AgentEvent::MessageCommitted(message) => {
                self.insert_message(event_id, session_id, sequence, message, false)
            }
            AgentEvent::ReasoningDelta(delta) | AgentEvent::AssistantTextDelta(delta) => {
                self.insert_delta(event_id, session_id, sequence, delta)
            }
            AgentEvent::ToolCallRequested(request) => {
                self.insert_tool_call(event_id, session_id, sequence, request)
            }
            AgentEvent::ToolCallFinished(result) => {
                self.insert_tool_result(event_id, session_id, sequence, result)
            }
            AgentEvent::ApprovalRequested(request) => {
                self.insert_approval(event_id, session_id, sequence, request)
            }
            AgentEvent::StateChanged(_)
            | AgentEvent::ToolCallStarted(_)
            | AgentEvent::Error(_)
            | AgentEvent::Exited(_) => Ok(()),
        }?;

        self.project_conversation_message(
            event_id,
            session_id,
            turn_id,
            provider_id,
            sequence,
            event_kind,
            event,
        )
    }

    fn project_conversation_message(
        &self,
        event_id: &str,
        session_id: &str,
        turn_id: Option<&str>,
        provider_id: Option<&str>,
        sequence: i64,
        event_kind: &str,
        event: &AgentEvent,
    ) -> Result<()> {
        for message in crate::agent::rig::rig_messages_from_horizon_events(&[event.clone()]) {
            let rig_message_json =
                serde_json::to_string(&message).context("serialize Rig conversation message")?;
            self.conn.execute(
                "INSERT INTO agent_conversation_messages (
                    event_id,
                    session_id,
                    conversation_id,
                    turn_id,
                    sequence,
                    provider_id,
                    horizon_event_kind,
                    rig_message_json
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    event_id,
                    session_id,
                    session_id,
                    turn_id,
                    sequence,
                    provider_id,
                    event_kind,
                    &rig_message_json,
                ],
            )?;
        }
        Ok(())
    }

    fn insert_message(
        &self,
        event_id: &str,
        session_id: &str,
        sequence: i64,
        message: &AgentMessage,
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
        delta: &AgentMessageDelta,
    ) -> Result<()> {
        self.insert_message(
            event_id,
            session_id,
            sequence,
            &AgentMessage {
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
        request: &AgentToolCallRequest,
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
        result: &AgentToolCallResult,
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
        request: &AgentApprovalRequest,
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

fn role_text(role: AgentMessageRole) -> &'static str {
    match role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    }
}
