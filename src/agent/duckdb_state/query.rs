use anyhow::{Context, Result};
use duckdb::params;
use serde_json::Value;

use crate::agent::{
    agent_frame_from_events, AgentFrame, AgentMessageRole, AgentProviderId, AgentToolCallId,
};
use crate::workspace::SessionId;

use super::{
    session_id_text, AgentStoredApproval, AgentStoredConversationMessage, AgentStoredEvent,
    AgentStoredMessage, AgentStoredSession, AgentStoredSessionSnapshot, AgentStoredToolCall,
    AgentStoredToolResult, DuckDbAgentStateStore,
};

impl DuckDbAgentStateStore {
    pub fn sessions(&self) -> Result<Vec<AgentStoredSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, provider_id, last_sequence, updated_at::TEXT
             FROM agent_sessions
             ORDER BY updated_at DESC, session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentStoredSession {
                session_id: parse_session_id_column(0, &row.get::<_, String>(0)?)?,
                provider_id: row.get::<_, Option<String>>(1)?.map(AgentProviderId),
                last_sequence: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent sessions")
    }

    pub fn session_snapshots(&self) -> Result<Vec<AgentStoredSessionSnapshot>> {
        self.sessions()?
            .into_iter()
            .map(|session| {
                let session_id = session.session_id;
                Ok(AgentStoredSessionSnapshot {
                    session,
                    frame: self.frame_for_session(session_id)?,
                    message_count: self.messages_for_session(session_id)?.len(),
                    tool_call_count: self.tool_calls_for_session(session_id)?.len(),
                    approval_count: self.approvals_for_session(session_id)?.len(),
                })
            })
            .collect()
    }

    pub fn events_for_session(&self, session_id: SessionId) -> Result<Vec<AgentStoredEvent>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT
                event_id,
                turn_id,
                sequence,
                event_kind,
                horizon_event_json,
                provider_id,
                provider_payload_json
             FROM agent_events
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let event_json: String = row.get(4)?;
            let provider_payload_json: Option<String> = row.get(6)?;
            Ok(AgentStoredEvent {
                event_id: row.get(0)?,
                session_id,
                turn_id: row.get(1)?,
                sequence: row.get(2)?,
                event_kind: row.get(3)?,
                event: serde_json::from_str(&event_json).map_err(|err| {
                    duckdb::Error::FromSqlConversionFailure(
                        4,
                        duckdb::types::Type::Text,
                        Box::new(err),
                    )
                })?,
                provider_id: row.get::<_, Option<String>>(5)?.map(AgentProviderId),
                provider_payload: provider_payload_json
                    .map(|json| {
                        serde_json::from_str(&json).map_err(|err| {
                            duckdb::Error::FromSqlConversionFailure(
                                6,
                                duckdb::types::Type::Text,
                                Box::new(err),
                            )
                        })
                    })
                    .transpose()?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent events")
    }

    pub fn frame_for_session(&self, session_id: SessionId) -> Result<AgentFrame> {
        let events = self
            .events_for_session(session_id)?
            .into_iter()
            .map(|record| record.event)
            .collect::<Vec<_>>();
        Ok(agent_frame_from_events(&events))
    }

    pub fn messages_for_session(&self, session_id: SessionId) -> Result<Vec<AgentStoredMessage>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, role, text, is_delta
             FROM agent_messages
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredMessage {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                role: parse_role(row.get::<_, String>(2)?.as_str()),
                text: row.get(3)?,
                is_delta: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent messages")
    }

    pub fn tool_calls_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredToolCall>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, tool_id, input_json
             FROM agent_tool_calls
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let input_json: String = row.get(4)?;
            Ok(AgentStoredToolCall {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: AgentToolCallId(row.get(2)?),
                tool_id: row.get(3)?,
                input: parse_json_column(4, &input_json)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent tool calls")
    }

    pub fn tool_results_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredToolResult>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, output_json
             FROM agent_tool_results
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            let output_json: String = row.get(3)?;
            Ok(AgentStoredToolResult {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: AgentToolCallId(row.get(2)?),
                output: parse_json_column(3, &output_json)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent tool results")
    }

    pub fn approvals_for_session(&self, session_id: SessionId) -> Result<Vec<AgentStoredApproval>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT event_id, sequence, call_id, reason
             FROM agent_approvals
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredApproval {
                event_id: row.get(0)?,
                session_id,
                sequence: row.get(1)?,
                call_id: AgentToolCallId(row.get(2)?),
                reason: row.get(3)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent approvals")
    }

    pub fn conversation_messages_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AgentStoredConversationMessage>> {
        let session_id_text = session_id_text(session_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT
                event_id,
                conversation_id,
                turn_id,
                sequence,
                provider_id,
                horizon_event_kind,
                rig_message_json
             FROM agent_conversation_messages
             WHERE session_id = ?
             ORDER BY sequence",
        )?;
        let rows = stmt.query_map(params![&session_id_text], |row| {
            Ok(AgentStoredConversationMessage {
                event_id: row.get(0)?,
                session_id,
                conversation_id: row.get(1)?,
                turn_id: row.get(2)?,
                sequence: row.get(3)?,
                provider_id: row.get::<_, Option<String>>(4)?.map(AgentProviderId),
                horizon_event_kind: row.get(5)?,
                rig_message_json: row.get(6)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("query agent conversation messages")
    }

    pub fn rig_messages_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<rig_core::completion::Message>> {
        self.conversation_messages_for_session(session_id)?
            .into_iter()
            .map(|record| {
                serde_json::from_str(&record.rig_message_json)
                    .context("deserialize Rig conversation message")
            })
            .collect()
    }
}

fn parse_role(value: &str) -> AgentMessageRole {
    match value {
        "user" => AgentMessageRole::User,
        "assistant" => AgentMessageRole::Assistant,
        _ => AgentMessageRole::Assistant,
    }
}

fn parse_session_id_column(column: usize, value: &str) -> duckdb::Result<SessionId> {
    let json = serde_json::Value::String(value.to_string());
    serde_json::from_value(json).map_err(|err| {
        duckdb::Error::FromSqlConversionFailure(column, duckdb::types::Type::Text, Box::new(err))
    })
}

fn parse_json_column(column: usize, json: &str) -> duckdb::Result<Value> {
    serde_json::from_str(json).map_err(|err| {
        duckdb::Error::FromSqlConversionFailure(column, duckdb::types::Type::Text, Box::new(err))
    })
}
