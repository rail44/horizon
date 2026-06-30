use std::path::Path;

use anyhow::{Context, Result};
use duckdb::{params, Connection, OptionalExt};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    agent::{
        agent_event_kind, agent_frame_from_events, AgentApprovalRequest, AgentEvent, AgentFrame,
        AgentMessage, AgentMessageDelta, AgentMessageRole, AgentProviderId, AgentToolCallId,
        AgentToolCallRequest, AgentToolCallResult,
    },
    workspace::SessionId,
};

pub struct DuckDbAgentStateStore {
    conn: Connection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSession {
    pub session_id: SessionId,
    pub provider_id: Option<AgentProviderId>,
    pub last_sequence: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSessionSnapshot {
    pub session: AgentStoredSession,
    pub frame: AgentFrame,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub approval_count: usize,
}

#[derive(Clone, Debug)]
pub struct AppendAgentEvent {
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<AgentProviderId>,
    pub event: AgentEvent,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub event_kind: String,
    pub event: AgentEvent,
    pub provider_id: Option<AgentProviderId>,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub role: AgentMessageRole,
    pub text: String,
    pub is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolCall {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub tool_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolResult {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredApproval {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredConversationMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub conversation_id: String,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub provider_id: Option<AgentProviderId>,
    pub horizon_event_kind: String,
    pub rig_message_json: String,
}

impl DuckDbAgentStateStore {
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory().context("open in-memory DuckDB agent store")?,
        };
        store.initialize_schema()?;
        Ok(store)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path).context("open DuckDB agent store")?,
        };
        store.initialize_schema()?;
        Ok(store)
    }

    pub fn append_event(&self, record: AppendAgentEvent) -> Result<AgentStoredEvent> {
        let session_id_text = session_id_text(record.session_id)?;
        let sequence = self.next_sequence(&session_id_text)?;
        let event_id = Uuid::new_v4().to_string();
        let event_kind = agent_event_kind(&record.event).to_string();
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
        self.project_event(
            &event_id,
            &session_id_text,
            record.turn_id.as_deref(),
            provider_id_text.as_deref(),
            sequence,
            &event_kind,
            &record.event,
        )?;

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

    pub fn append_events(
        &self,
        session_id: SessionId,
        provider_id: Option<AgentProviderId>,
        events: impl IntoIterator<Item = AgentEvent>,
    ) -> Result<Vec<AgentStoredEvent>> {
        events
            .into_iter()
            .map(|event| {
                self.append_event(AppendAgentEvent {
                    session_id,
                    turn_id: None,
                    provider_id: provider_id.clone(),
                    event,
                    provider_payload: None,
                })
            })
            .collect()
    }

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

    pub fn replace_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = crate::agent_event_log::AgentEventLogRecord>,
    ) -> Result<()> {
        self.clear_all_agent_state()?;
        for record in records {
            self.insert_event_log_record(record)?;
        }
        Ok(())
    }

    fn initialize_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS agent_sessions (
                session_id TEXT PRIMARY KEY,
                provider_id TEXT,
                last_sequence BIGINT NOT NULL DEFAULT -1,
                updated_at TIMESTAMP NOT NULL DEFAULT now()
            );

            CREATE TABLE IF NOT EXISTS agent_events (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                turn_id TEXT,
                sequence BIGINT NOT NULL,
                event_kind TEXT NOT NULL,
                horizon_event_json TEXT NOT NULL,
                provider_id TEXT,
                provider_payload_json TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT now(),
                UNIQUE(session_id, sequence)
            );

            CREATE TABLE IF NOT EXISTS agent_messages (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                sequence BIGINT NOT NULL,
                role TEXT NOT NULL,
                text TEXT NOT NULL,
                is_delta BOOLEAN NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_tool_calls (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                sequence BIGINT NOT NULL,
                call_id TEXT NOT NULL,
                tool_id TEXT NOT NULL,
                input_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_tool_results (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                sequence BIGINT NOT NULL,
                call_id TEXT NOT NULL,
                output_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_approvals (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                sequence BIGINT NOT NULL,
                call_id TEXT NOT NULL,
                reason TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_conversation_messages (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                turn_id TEXT,
                sequence BIGINT NOT NULL,
                provider_id TEXT,
                horizon_event_kind TEXT NOT NULL,
                rig_message_json TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    fn clear_projections_for_session(&self, session_id: &str) -> Result<()> {
        for table in [
            "agent_messages",
            "agent_tool_calls",
            "agent_tool_results",
            "agent_approvals",
            "agent_conversation_messages",
        ] {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?"),
                params![session_id],
            )?;
        }
        Ok(())
    }

    fn clear_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            DELETE FROM agent_messages;
            DELETE FROM agent_tool_calls;
            DELETE FROM agent_tool_results;
            DELETE FROM agent_approvals;
            DELETE FROM agent_conversation_messages;
            DELETE FROM agent_events;
            DELETE FROM agent_sessions;
            ",
        )?;
        Ok(())
    }

    fn insert_event_log_record(
        &self,
        record: crate::agent_event_log::AgentEventLogRecord,
    ) -> Result<()> {
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
        self.project_event(
            &record.event_id,
            &session_id_text,
            record.turn_id.as_deref(),
            provider_id_text.as_deref(),
            sequence,
            &record.event_kind,
            &record.event,
        )?;
        Ok(())
    }

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

    fn upsert_session(
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

    fn project_event(
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
        for message in crate::agent_rig_spike::rig_messages_from_horizon_events(&[event.clone()]) {
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

fn parse_role(value: &str) -> AgentMessageRole {
    match value {
        "user" => AgentMessageRole::User,
        "assistant" => AgentMessageRole::Assistant,
        _ => AgentMessageRole::Assistant,
    }
}

fn session_id_text(session_id: SessionId) -> Result<String> {
    let value = serde_json::to_value(session_id).context("serialize session id")?;
    Ok(value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentSessionState, AgentToolCallRequest};
    use std::time::{Duration, Instant};

    #[test]
    fn stores_events_and_rebuilds_agent_frame() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        store
            .append_events(
                session_id,
                Some(AgentProviderId("spike.agent.rig-core".to_string())),
                [
                    AgentEvent::StateChanged(AgentSessionState::Running),
                    AgentEvent::MessageCommitted(AgentMessage {
                        role: AgentMessageRole::User,
                        text: "snapshot".to_string(),
                    }),
                    AgentEvent::ToolCallRequested(AgentToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({}),
                    }),
                    AgentEvent::ApprovalRequested(AgentApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "needs approval".to_string(),
                    }),
                    AgentEvent::ToolCallFinished(AgentToolCallResult {
                        call_id,
                        output: serde_json::json!({ "tab_count": 1 }),
                    }),
                ],
            )
            .expect("append events");

        let frame = store.frame_for_session(session_id).expect("frame");
        assert_eq!(frame.state, Some(AgentSessionState::Running));
        assert_eq!(frame.pending_approval_call_id(), None);
        assert_eq!(frame.items.len(), 4);
    }

    #[test]
    fn exposes_queryable_message_tool_and_approval_projections() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        store
            .append_events(
                session_id,
                None,
                [
                    AgentEvent::MessageCommitted(AgentMessage {
                        role: AgentMessageRole::Assistant,
                        text: "ready".to_string(),
                    }),
                    AgentEvent::ToolCallRequested(AgentToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({ "include": "tabs" }),
                    }),
                    AgentEvent::ApprovalRequested(AgentApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "approval".to_string(),
                    }),
                    AgentEvent::ToolCallFinished(AgentToolCallResult {
                        call_id,
                        output: serde_json::json!({ "ok": true }),
                    }),
                ],
            )
            .expect("append events");

        let messages = store.messages_for_session(session_id).expect("messages");
        assert_eq!(messages[0].text, "ready");
        assert_eq!(messages[0].role, AgentMessageRole::Assistant);

        let calls = store.tool_calls_for_session(session_id).expect("calls");
        assert_eq!(calls[0].tool_id, "workspace.snapshot");
        assert_eq!(calls[0].input["include"], "tabs");

        let approvals = store.approvals_for_session(session_id).expect("approvals");
        assert_eq!(approvals[0].reason, "approval");

        let results = store.tool_results_for_session(session_id).expect("results");
        assert_eq!(results[0].output["ok"], true);
    }

    #[test]
    fn preserves_optional_provider_payload_on_event_records() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let provider_payload = serde_json::json!({
            "rig": {
                "tool_call": {
                    "id": "rig-call-id",
                    "call_id": "provider-call-id",
                    "signature": "sig",
                }
            }
        });

        store
            .append_event(AppendAgentEvent {
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("spike.agent.rig-core".to_string())),
                event: AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: "with provider payload".to_string(),
                }),
                provider_payload: Some(provider_payload.clone()),
            })
            .expect("append event");

        let events = store.events_for_session(session_id).expect("events");
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            events[0].provider_id,
            Some(AgentProviderId("spike.agent.rig-core".to_string()))
        );
        assert_eq!(events[0].provider_payload, Some(provider_payload));
    }

    #[test]
    fn file_backed_store_reopens_persisted_events_and_projections() {
        let path = std::env::temp_dir().join(format!("horizon-agent-{}.duckdb", Uuid::new_v4()));
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        {
            let store = DuckDbAgentStateStore::open(&path).expect("open file store");
            store
                .append_events(
                    session_id,
                    Some(AgentProviderId("builtin.agent.mock".to_string())),
                    [
                        AgentEvent::MessageCommitted(AgentMessage {
                            role: AgentMessageRole::User,
                            text: "snapshot".to_string(),
                        }),
                        AgentEvent::ToolCallRequested(AgentToolCallRequest {
                            call_id: call_id.clone(),
                            tool_id: "workspace.snapshot".to_string(),
                            input: serde_json::json!({}),
                        }),
                        AgentEvent::ToolCallFinished(AgentToolCallResult {
                            call_id,
                            output: serde_json::json!({ "tab_count": 1 }),
                        }),
                    ],
                )
                .expect("append events");
        }

        let reopened = DuckDbAgentStateStore::open(&path).expect("reopen file store");
        let sessions = reopened.sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        assert_eq!(
            sessions[0].provider_id,
            Some(AgentProviderId("builtin.agent.mock".to_string()))
        );
        assert_eq!(sessions[0].last_sequence, 2);
        assert!(!sessions[0].updated_at.is_empty());

        let events = reopened.events_for_session(session_id).expect("events");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].provider_id,
            Some(AgentProviderId("builtin.agent.mock".to_string()))
        );

        let messages = reopened.messages_for_session(session_id).expect("messages");
        assert_eq!(messages[0].text, "snapshot");

        let results = reopened
            .tool_results_for_session(session_id)
            .expect("results");
        assert_eq!(results[0].output["tab_count"], 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn file_backed_store_reopens_session_snapshots_for_restore_read_model() {
        let path = std::env::temp_dir().join(format!("horizon-agent-{}.duckdb", Uuid::new_v4()));
        let first_session_id = SessionId::new();
        let second_session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        {
            let store = DuckDbAgentStateStore::open(&path).expect("open file store");
            store
                .append_events(
                    first_session_id,
                    Some(AgentProviderId("builtin.agent.mock".to_string())),
                    [
                        AgentEvent::MessageCommitted(AgentMessage {
                            role: AgentMessageRole::User,
                            text: "snapshot".to_string(),
                        }),
                        AgentEvent::ToolCallRequested(AgentToolCallRequest {
                            call_id: call_id.clone(),
                            tool_id: "workspace.snapshot".to_string(),
                            input: serde_json::json!({}),
                        }),
                        AgentEvent::ApprovalRequested(AgentApprovalRequest {
                            call_id: call_id.clone(),
                            reason: "approval".to_string(),
                        }),
                    ],
                )
                .expect("append first session");
            store
                .append_events(
                    second_session_id,
                    Some(AgentProviderId("spike.agent.rig-core".to_string())),
                    [AgentEvent::MessageCommitted(AgentMessage {
                        role: AgentMessageRole::Assistant,
                        text: "ready".to_string(),
                    })],
                )
                .expect("append second session");
        }

        let reopened = DuckDbAgentStateStore::open(&path).expect("reopen file store");
        let snapshots = reopened.session_snapshots().expect("snapshots");
        assert_eq!(snapshots.len(), 2);

        let first = snapshots
            .iter()
            .find(|snapshot| snapshot.session.session_id == first_session_id)
            .expect("first session snapshot");
        assert_eq!(first.message_count, 1);
        assert_eq!(first.tool_call_count, 1);
        assert_eq!(first.approval_count, 1);
        assert_eq!(first.frame.items.len(), 3);

        let second = snapshots
            .iter()
            .find(|snapshot| snapshot.session.session_id == second_session_id)
            .expect("second session snapshot");
        assert_eq!(
            second.session.provider_id,
            Some(AgentProviderId("spike.agent.rig-core".to_string()))
        );
        assert_eq!(second.message_count, 1);
        assert_eq!(second.tool_call_count, 0);
        assert_eq!(second.approval_count, 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rebuilds_query_projections_from_durable_events() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let first_session_id = SessionId::new();
        let second_session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        store
            .append_events(
                first_session_id,
                Some(AgentProviderId("builtin.agent.mock".to_string())),
                [
                    AgentEvent::MessageCommitted(AgentMessage {
                        role: AgentMessageRole::User,
                        text: "snapshot".to_string(),
                    }),
                    AgentEvent::ToolCallRequested(AgentToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({}),
                    }),
                    AgentEvent::ApprovalRequested(AgentApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "approval".to_string(),
                    }),
                    AgentEvent::ToolCallFinished(AgentToolCallResult {
                        call_id,
                        output: serde_json::json!({ "tab_count": 1 }),
                    }),
                ],
            )
            .expect("append first session");
        store
            .append_events(
                second_session_id,
                Some(AgentProviderId("spike.agent.rig-core".to_string())),
                [AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: "ready".to_string(),
                })],
            )
            .expect("append second session");

        store
            .conn
            .execute_batch(
                "
                DELETE FROM agent_messages;
                DELETE FROM agent_tool_calls;
                DELETE FROM agent_tool_results;
                DELETE FROM agent_approvals;
                ",
            )
            .expect("clear projections");
        assert!(store
            .session_snapshots()
            .expect("empty snapshots")
            .iter()
            .all(|snapshot| {
                snapshot.message_count == 0
                    && snapshot.tool_call_count == 0
                    && snapshot.approval_count == 0
            }));

        store.rebuild_projections().expect("rebuild projections");
        let snapshots = store.session_snapshots().expect("snapshots");
        let first = snapshots
            .iter()
            .find(|snapshot| snapshot.session.session_id == first_session_id)
            .expect("first snapshot");
        assert_eq!(first.message_count, 1);
        assert_eq!(first.tool_call_count, 1);
        assert_eq!(first.approval_count, 1);
        assert_eq!(first.frame.items.len(), 4);

        let second = snapshots
            .iter()
            .find(|snapshot| snapshot.session.session_id == second_session_id)
            .expect("second snapshot");
        assert_eq!(second.message_count, 1);
        assert_eq!(second.tool_call_count, 0);
        assert_eq!(second.approval_count, 0);

        store
            .rebuild_projections_for_session(first_session_id)
            .expect("rebuild first session");
        let first_after_second_rebuild = store
            .session_snapshots()
            .expect("snapshots")
            .into_iter()
            .find(|snapshot| snapshot.session.session_id == first_session_id)
            .expect("first snapshot");
        assert_eq!(first_after_second_rebuild.message_count, 1);
        assert_eq!(first_after_second_rebuild.tool_call_count, 1);
        assert_eq!(first_after_second_rebuild.approval_count, 1);
    }

    #[test]
    fn rebuilds_store_from_agent_event_log_records() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());
        let records = vec![
            crate::agent_event_log::AgentEventLogRecord {
                schema: crate::agent_event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent_event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-1".to_string(),
                sequence: 0,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("spike.agent.rig-core".to_string())),
                event_kind: agent_event_kind(&AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "snapshot".to_string(),
                }))
                .to_string(),
                event: AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "snapshot".to_string(),
                }),
                provider_payload: Some(serde_json::json!({ "source": "jsonl" })),
                created_at_unix_ms: 1,
            },
            crate::agent_event_log::AgentEventLogRecord {
                schema: crate::agent_event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent_event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-2".to_string(),
                sequence: 1,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("spike.agent.rig-core".to_string())),
                event_kind: agent_event_kind(&AgentEvent::ToolCallRequested(
                    AgentToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({}),
                    },
                ))
                .to_string(),
                event: AgentEvent::ToolCallRequested(AgentToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "workspace.snapshot".to_string(),
                    input: serde_json::json!({}),
                }),
                provider_payload: None,
                created_at_unix_ms: 2,
            },
            crate::agent_event_log::AgentEventLogRecord {
                schema: crate::agent_event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent_event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-3".to_string(),
                sequence: 2,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("spike.agent.rig-core".to_string())),
                event_kind: agent_event_kind(&AgentEvent::ToolCallFinished(AgentToolCallResult {
                    call_id: call_id.clone(),
                    output: serde_json::json!({ "ok": true }),
                }))
                .to_string(),
                event: AgentEvent::ToolCallFinished(AgentToolCallResult {
                    call_id,
                    output: serde_json::json!({ "ok": true }),
                }),
                provider_payload: None,
                created_at_unix_ms: 3,
            },
        ];

        store
            .append_events(
                SessionId::new(),
                None,
                [AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: "old".to_string(),
                })],
            )
            .expect("append old data");
        store
            .replace_from_event_log_records(records)
            .expect("replace from records");

        let sessions = store.sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        assert_eq!(sessions[0].last_sequence, 2);

        let events = store.events_for_session(session_id).expect("events");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            events[0].provider_payload,
            Some(serde_json::json!({ "source": "jsonl" }))
        );

        let messages = store.messages_for_session(session_id).expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "snapshot");
        assert_eq!(
            store
                .tool_calls_for_session(session_id)
                .expect("tool calls")[0]
                .tool_id,
            "workspace.snapshot"
        );
        assert_eq!(
            store
                .tool_results_for_session(session_id)
                .expect("tool results")[0]
                .output["ok"],
            true
        );
    }

    #[test]
    fn projects_rig_conversation_messages_from_appended_events() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());
        let events = vec![
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "snapshot please".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "checking workspace".to_string(),
            }),
            AgentEvent::ToolCallRequested(AgentToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({}),
            }),
            AgentEvent::ToolCallFinished(AgentToolCallResult {
                call_id,
                output: serde_json::json!({ "tab_count": 1 }),
            }),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "There is one tab.".to_string(),
            }),
        ];

        store
            .append_events(
                session_id,
                Some(AgentProviderId("spike.agent.rig-core".to_string())),
                events.clone(),
            )
            .expect("append events");

        let projected = store
            .conversation_messages_for_session(session_id)
            .expect("conversation messages");
        assert_eq!(projected.len(), 4);
        assert!(projected
            .iter()
            .all(|record| record.conversation_id == session_id_text(session_id).unwrap()));
        assert_eq!(projected[0].horizon_event_kind, "message_committed");
        assert_eq!(projected[1].horizon_event_kind, "tool_call_requested");
        assert_eq!(projected[2].horizon_event_kind, "tool_call_finished");
        assert_eq!(projected[3].horizon_event_kind, "message_committed");

        let rig_messages = store
            .rig_messages_for_session(session_id)
            .expect("rig messages");
        assert_eq!(
            rig_messages,
            rig_message_json_roundtrip(crate::agent_rig_spike::rig_messages_from_horizon_events(
                &events,
            ))
        );
    }

    #[test]
    fn rebuilds_rig_conversation_projection_from_event_log_records() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let events = [
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "hello".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "ignored streaming delta".to_string(),
            }),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "hi".to_string(),
            }),
        ];
        let records = events
            .iter()
            .enumerate()
            .map(
                |(index, event)| crate::agent_event_log::AgentEventLogRecord {
                    schema: crate::agent_event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                    version: crate::agent_event_log::AGENT_EVENT_LOG_VERSION,
                    event_id: format!("event-{index}"),
                    sequence: index as u64,
                    session_id,
                    turn_id: Some("turn-1".to_string()),
                    provider_id: Some(AgentProviderId("spike.agent.rig-core".to_string())),
                    event_kind: agent_event_kind(event).to_string(),
                    event: event.clone(),
                    provider_payload: None,
                    created_at_unix_ms: index as u64,
                },
            )
            .collect::<Vec<_>>();

        store
            .replace_from_event_log_records(records)
            .expect("replace from event log");

        let rig_messages = store
            .rig_messages_for_session(session_id)
            .expect("rig messages");
        assert_eq!(rig_messages.len(), 2);
        assert_eq!(
            rig_messages,
            rig_message_json_roundtrip(crate::agent_rig_spike::rig_messages_from_horizon_events(
                &events,
            ))
        );
    }

    #[test]
    #[ignore = "micro benchmark; run with --ignored --nocapture"]
    fn bench_append_projection_costs() {
        let event_count = std::env::var("HORIZON_AGENT_DUCKDB_BENCH_EVENTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000);

        run_append_projection_bench(
            "in-memory deltas",
            DuckDbAgentStateStore::open_in_memory().expect("open in-memory store"),
            event_count,
            bench_delta_event,
            None,
        );

        run_append_projection_bench(
            "in-memory mixed turn",
            DuckDbAgentStateStore::open_in_memory().expect("open in-memory store"),
            event_count,
            bench_mixed_turn_event,
            None,
        );

        let path = std::env::temp_dir().join(format!(
            "horizon-agent-duckdb-bench-{}.duckdb",
            Uuid::new_v4()
        ));
        run_append_projection_bench(
            "file-backed deltas",
            DuckDbAgentStateStore::open(&path).expect("open file-backed store"),
            event_count,
            bench_delta_event,
            Some(path),
        );
    }

    fn run_append_projection_bench(
        label: &str,
        store: DuckDbAgentStateStore,
        event_count: usize,
        event_at: impl Fn(usize) -> AgentEvent,
        cleanup_path: Option<std::path::PathBuf>,
    ) {
        let session_id = SessionId::new();
        let provider_id = Some(AgentProviderId("bench.agent".to_string()));
        let mut append_durations = Vec::with_capacity(event_count);

        let total_start = Instant::now();
        for index in 0..event_count {
            let start = Instant::now();
            store
                .append_event(AppendAgentEvent {
                    session_id,
                    turn_id: Some(format!("turn-{}", index / 100)),
                    provider_id: provider_id.clone(),
                    event: event_at(index),
                    provider_payload: None,
                })
                .expect("append bench event");
            append_durations.push(start.elapsed());
        }
        let total_append = total_start.elapsed();

        let events_query = elapsed(|| store.events_for_session(session_id).expect("events"));
        let messages_query = elapsed(|| store.messages_for_session(session_id).expect("messages"));
        let frame_query = elapsed(|| store.frame_for_session(session_id).expect("frame"));

        let stats = DurationStats::from_samples(&append_durations);
        eprintln!(
            "agent_duckdb bench: {label}; events={event_count}; append_total={}; append_avg={}; append_p50={}; append_p95={}; append_max={}; events_query={}; messages_query={}; frame_query={}",
            format_duration(total_append),
            format_duration(stats.avg),
            format_duration(stats.p50),
            format_duration(stats.p95),
            format_duration(stats.max),
            format_duration(events_query.0),
            format_duration(messages_query.0),
            format_duration(frame_query.0),
        );

        if let Some(path) = cleanup_path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn bench_delta_event(index: usize) -> AgentEvent {
        if index % 2 == 0 {
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: format!("reasoning delta {index}\n"),
            })
        } else {
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: format!("assistant delta {index}\n"),
            })
        }
    }

    fn bench_mixed_turn_event(index: usize) -> AgentEvent {
        match index % 10 {
            0 => AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: format!("user message {index}"),
            }),
            1 | 2 => AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: format!("thinking chunk {index}\n"),
            }),
            3 | 4 | 5 => AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: format!("assistant chunk {index}\n"),
            }),
            6 => AgentEvent::ToolCallRequested(AgentToolCallRequest {
                call_id: AgentToolCallId(format!("call-{index}")),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({ "index": index }),
            }),
            7 => AgentEvent::ApprovalRequested(AgentApprovalRequest {
                call_id: AgentToolCallId(format!("call-{}", index - 1)),
                reason: "benchmark approval".to_string(),
            }),
            8 => AgentEvent::ToolCallFinished(AgentToolCallResult {
                call_id: AgentToolCallId(format!("call-{}", index - 2)),
                output: serde_json::json!({ "ok": true, "index": index }),
            }),
            _ => AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: format!("assistant final {index}"),
            }),
        }
    }

    fn elapsed<T>(f: impl FnOnce() -> T) -> (Duration, T) {
        let start = Instant::now();
        let value = f();
        (start.elapsed(), value)
    }

    struct DurationStats {
        avg: Duration,
        p50: Duration,
        p95: Duration,
        max: Duration,
    }

    impl DurationStats {
        fn from_samples(samples: &[Duration]) -> Self {
            let mut sorted = samples.to_vec();
            sorted.sort();
            let total = sorted.iter().copied().sum::<Duration>();
            Self {
                avg: total / sorted.len() as u32,
                p50: percentile(&sorted, 50),
                p95: percentile(&sorted, 95),
                max: *sorted.last().expect("samples"),
            }
        }
    }

    fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
        let index = ((sorted.len().saturating_sub(1)) * percentile) / 100;
        sorted[index]
    }

    fn format_duration(duration: Duration) -> String {
        format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
    }

    fn rig_message_json_roundtrip(
        messages: Vec<rig_core::completion::Message>,
    ) -> Vec<rig_core::completion::Message> {
        messages
            .into_iter()
            .map(|message| {
                let json = serde_json::to_string(&message).expect("serialize Rig message");
                serde_json::from_str(&json).expect("deserialize Rig message")
            })
            .collect()
    }
}
