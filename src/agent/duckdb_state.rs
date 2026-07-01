use std::path::Path;

use anyhow::{Context, Result};
use duckdb::Connection;

use crate::workspace::SessionId;

mod append;
mod import;
mod projection;
mod query;
mod records;
mod schema;

use schema::INITIALIZE_SCHEMA_SQL;

pub use records::{
    AgentStoredApproval, AgentStoredConversationMessage, AgentStoredEvent, AgentStoredMessage,
    AgentStoredSession, AgentStoredSessionSnapshot, AgentStoredToolCall, AgentStoredToolResult,
    AppendAgentEvent,
};

pub struct DuckDbAgentStateStore {
    conn: Connection,
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

    fn initialize_schema(&self) -> Result<()> {
        self.conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        Ok(())
    }
}

fn session_id_text(session_id: SessionId) -> Result<String> {
    let value = serde_json::to_value(session_id).context("serialize session id")?;
    Ok(value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        agent_event_kind, AgentApprovalRequest, AgentEvent, AgentMessage, AgentMessageDelta,
        AgentMessageRole, AgentProviderId, AgentSessionState, AgentToolCallId,
        AgentToolCallRequest, AgentToolCallResult,
    };
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    #[test]
    fn stores_events_and_rebuilds_agent_frame() {
        let store = DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = AgentToolCallId("call-1".to_string());

        store
            .append_events(
                session_id,
                Some(AgentProviderId("builtin.agent.rig".to_string())),
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
                provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            Some(AgentProviderId("builtin.agent.rig".to_string()))
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
                    Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            Some(AgentProviderId("builtin.agent.rig".to_string()))
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
                Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            crate::agent::event_log::AgentEventLogRecord {
                schema: crate::agent::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-1".to_string(),
                sequence: 0,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            crate::agent::event_log::AgentEventLogRecord {
                schema: crate::agent::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-2".to_string(),
                sequence: 1,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            crate::agent::event_log::AgentEventLogRecord {
                schema: crate::agent::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::agent::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-3".to_string(),
                sequence: 2,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
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
                Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            rig_message_json_roundtrip(crate::agent::rig::rig_messages_from_horizon_events(
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
                |(index, event)| crate::agent::event_log::AgentEventLogRecord {
                    schema: crate::agent::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                    version: crate::agent::event_log::AGENT_EVENT_LOG_VERSION,
                    event_id: format!("event-{index}"),
                    sequence: index as u64,
                    session_id,
                    turn_id: Some("turn-1".to_string()),
                    provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
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
            rig_message_json_roundtrip(crate::agent::rig::rig_messages_from_horizon_events(
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
