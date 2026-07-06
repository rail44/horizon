use std::path::Path;

use anyhow::{Context, Result};
use duckdb::Connection;

use crate::contract::SessionId;

mod append;
mod import;
mod projection;
mod query;
mod records;
mod schema;

use schema::INITIALIZE_SCHEMA_SQL;

use records::AgentStoredEvent;

#[cfg(test)]
pub use records::{
    AgentStoredApproval, AgentStoredMessage, AgentStoredSessionSnapshot, AgentStoredToolCall,
    AgentStoredToolResult, AppendEvent,
};
pub use records::{AgentStoredSession, RecallEntry, RecallEntryKind, RecallSearchReport};

pub struct Store {
    conn: Connection,
    /// Whether opening this store had to migrate a pre-`event_at`
    /// `agent_events` table (see [`Self::migrate_legacy_agent_events_schema`]).
    /// Not test-only: `horizon-agentd`'s startup rebuild-skip check
    /// (task 2 of the readiness fix) reads this via [`Self::
    /// migrated_legacy_schema`] to know it must not trust the projection's
    /// existing `agent_sessions.last_sequence` high-water mark -- a
    /// migration just dropped and recreated `agent_events` (losing its
    /// rows) without touching `agent_sessions`, so that table's numbers
    /// would otherwise look deceptively "current" against an now-empty
    /// projection.
    migrated_legacy_schema: bool,
}

impl Store {
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().context("open in-memory DuckDB agent store")?,
        )
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path).context("open DuckDB agent store")?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        let migrated_legacy_schema = Self::migrate_legacy_agent_events_schema(&conn)?;
        conn.execute_batch(INITIALIZE_SCHEMA_SQL)?;
        Ok(Self {
            conn,
            migrated_legacy_schema,
        })
    }

    /// See the field's doc comment on [`Self::migrated_legacy_schema`].
    pub fn migrated_legacy_schema(&self) -> bool {
        self.migrated_legacy_schema
    }

    /// Migrates a pre-`event_at` `agent_events` table (created by a Horizon
    /// build before this column existed) so the `CREATE TABLE IF NOT
    /// EXISTS` above can lay down the current schema. `CREATE TABLE IF NOT
    /// EXISTS` is additive-only and never alters an existing table, and
    /// DuckDB (confirmed against the bundled 1.10504.0) rejects `ALTER
    /// TABLE ... ADD COLUMN` with an inline `NOT NULL` constraint
    /// ("Adding columns with constraints not yet supported"), so a plain
    /// `ADD COLUMN IF NOT EXISTS` can't get us to `event_at TIMESTAMP NOT
    /// NULL` either. Dropping the stale table and letting the `CREATE
    /// TABLE IF NOT EXISTS` recreate it is cheap and correct specifically
    /// *because* the DuckDB projection is always fully rebuilt from the
    /// JSONL log right after this runs (`replace_from_event_log_records`
    /// clears every table and reinserts all of it) -- the rebuild
    /// repopulates every row's `event_at` from the JSONL record's
    /// `created_at_unix_ms`, so nothing is lost by dropping first.
    ///
    /// Returns whether a migration actually ran -- `true` both for a
    /// genuine legacy file and for a brand-new one (where `agent_events`
    /// doesn't exist yet either), which is harmless: [`Self::
    /// migrated_legacy_schema`]'s one caller only uses `true` to skip an
    /// optimization (trusting a freshness check), never to skip correctness
    /// work.
    fn migrate_legacy_agent_events_schema(conn: &Connection) -> Result<bool> {
        let has_event_at: i64 = conn.query_row(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_name = 'agent_events' AND column_name = 'event_at'",
            [],
            |row| row.get(0),
        )?;
        if has_event_at == 0 {
            conn.execute_batch("DROP TABLE IF EXISTS agent_events;")?;
            return Ok(true);
        }
        Ok(false)
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
    use crate::contract::{
        event_kind, ApprovalRequest, Event, Message, MessageDelta, MessageRole, ProviderId,
        ProviderRequestSent, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
    };
    use duckdb::params;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    #[test]
    fn stores_events_and_rebuilds_agent_frame() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());

        store
            .append_events(
                session_id,
                Some(ProviderId("builtin.agent.rig".to_string())),
                [
                    Event::StateChanged(SessionState::Running),
                    Event::MessageCommitted(Message {
                        role: MessageRole::User,
                        text: "snapshot".to_string(),
                    }),
                    Event::ToolCallRequested(ToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({}),
                    }),
                    Event::ApprovalRequested(ApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "needs approval".to_string(),
                    }),
                    Event::ToolCallFinished(ToolCallResult {
                        call_id,
                        output: serde_json::json!({ "tab_count": 1 }),
                    }),
                ],
            )
            .expect("append events");

        let frame = store.frame_for_session(session_id).expect("frame");
        assert_eq!(frame.state, Some(SessionState::Running));
        assert_eq!(frame.pending_approval_call_id(), None);
        assert_eq!(frame.items.len(), 4);
    }

    #[test]
    fn exposes_queryable_message_tool_and_approval_projections() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());

        store
            .append_events(
                session_id,
                None,
                [
                    Event::MessageCommitted(Message {
                        role: MessageRole::Assistant,
                        text: "ready".to_string(),
                    }),
                    Event::ToolCallRequested(ToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({ "include": "tabs" }),
                    }),
                    Event::ApprovalRequested(ApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "approval".to_string(),
                    }),
                    Event::ToolCallFinished(ToolCallResult {
                        call_id,
                        output: serde_json::json!({ "ok": true }),
                    }),
                ],
            )
            .expect("append events");

        let messages = store.messages_for_session(session_id).expect("messages");
        assert_eq!(messages[0].text, "ready");
        assert_eq!(messages[0].role, MessageRole::Assistant);

        let calls = store.tool_calls_for_session(session_id).expect("calls");
        assert_eq!(calls[0].tool_id, "workspace.snapshot");
        assert_eq!(calls[0].input["include"], "tabs");

        let approvals = store.approvals_for_session(session_id).expect("approvals");
        assert_eq!(approvals[0].reason, "approval");

        let results = store.tool_results_for_session(session_id).expect("results");
        assert_eq!(results[0].output["ok"], true);
    }

    /// The provider-request lifecycle markers have no dedicated projection
    /// table (`projection::project_event`'s exhaustive match treats them as
    /// a documented no-op passthrough) but must still round-trip through
    /// `agent_events` — that table, not a projection, is what the
    /// `agent-inspect` skill's replay/gap-attribution recipes read.
    #[test]
    fn persists_provider_request_lifecycle_events_without_a_dedicated_projection() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();

        store
            .append_events(
                session_id,
                Some(ProviderId("builtin.agent.rig".to_string())),
                [
                    Event::ProviderRequestSent(ProviderRequestSent {
                        model: "gpt-4o-mini".to_string(),
                    }),
                    Event::ProviderRequestFirstToken,
                    Event::ProviderRequestFinished,
                ],
            )
            .expect("append events");

        let events = store.events_for_session(session_id).expect("events");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_kind, "provider_request_sent");
        assert_eq!(
            events[0].event,
            Event::ProviderRequestSent(ProviderRequestSent {
                model: "gpt-4o-mini".to_string(),
            })
        );
        assert_eq!(events[1].event_kind, "provider_request_first_token");
        assert_eq!(events[1].event, Event::ProviderRequestFirstToken);
        assert_eq!(events[2].event_kind, "provider_request_finished");
        assert_eq!(events[2].event, Event::ProviderRequestFinished);

        assert!(store
            .messages_for_session(session_id)
            .expect("messages")
            .is_empty());
        assert!(store
            .tool_calls_for_session(session_id)
            .expect("tool calls")
            .is_empty());
        assert!(store
            .tool_results_for_session(session_id)
            .expect("tool results")
            .is_empty());
        assert!(store
            .approvals_for_session(session_id)
            .expect("approvals")
            .is_empty());
    }

    #[test]
    fn preserves_optional_provider_payload_on_event_records() {
        let store = Store::open_in_memory().expect("store");
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
            .append_event(AppendEvent {
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
                event: Event::MessageCommitted(Message {
                    role: MessageRole::Assistant,
                    text: "with provider payload".to_string(),
                }),
                provider_payload: Some(provider_payload.clone()),
            })
            .expect("append event");

        let events = store.events_for_session(session_id).expect("events");
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            events[0].provider_id,
            Some(ProviderId("builtin.agent.rig".to_string()))
        );
        assert_eq!(events[0].provider_payload, Some(provider_payload));
    }

    #[test]
    fn file_backed_store_reopens_persisted_events_and_projections() {
        let path = std::env::temp_dir().join(format!("horizon-agent-{}.duckdb", Uuid::new_v4()));
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());

        {
            let store = Store::open(&path).expect("open file store");
            store
                .append_events(
                    session_id,
                    Some(ProviderId("builtin.agent.mock".to_string())),
                    [
                        Event::MessageCommitted(Message {
                            role: MessageRole::User,
                            text: "snapshot".to_string(),
                        }),
                        Event::ToolCallRequested(ToolCallRequest {
                            call_id: call_id.clone(),
                            tool_id: "workspace.snapshot".to_string(),
                            input: serde_json::json!({}),
                        }),
                        Event::ToolCallFinished(ToolCallResult {
                            call_id,
                            output: serde_json::json!({ "tab_count": 1 }),
                        }),
                    ],
                )
                .expect("append events");
        }

        let reopened = Store::open(&path).expect("reopen file store");
        let sessions = reopened.sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        assert_eq!(
            sessions[0].provider_id,
            Some(ProviderId("builtin.agent.mock".to_string()))
        );
        assert_eq!(sessions[0].last_sequence, 2);
        assert!(!sessions[0].updated_at.is_empty());

        let events = reopened.events_for_session(session_id).expect("events");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].provider_id,
            Some(ProviderId("builtin.agent.mock".to_string()))
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
        let call_id = ToolCallId("call-1".to_string());

        {
            let store = Store::open(&path).expect("open file store");
            store
                .append_events(
                    first_session_id,
                    Some(ProviderId("builtin.agent.mock".to_string())),
                    [
                        Event::MessageCommitted(Message {
                            role: MessageRole::User,
                            text: "snapshot".to_string(),
                        }),
                        Event::ToolCallRequested(ToolCallRequest {
                            call_id: call_id.clone(),
                            tool_id: "workspace.snapshot".to_string(),
                            input: serde_json::json!({}),
                        }),
                        Event::ApprovalRequested(ApprovalRequest {
                            call_id: call_id.clone(),
                            reason: "approval".to_string(),
                        }),
                    ],
                )
                .expect("append first session");
            store
                .append_events(
                    second_session_id,
                    Some(ProviderId("builtin.agent.rig".to_string())),
                    [Event::MessageCommitted(Message {
                        role: MessageRole::Assistant,
                        text: "ready".to_string(),
                    })],
                )
                .expect("append second session");
        }

        let reopened = Store::open(&path).expect("reopen file store");
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
            Some(ProviderId("builtin.agent.rig".to_string()))
        );
        assert_eq!(second.message_count, 1);
        assert_eq!(second.tool_call_count, 0);
        assert_eq!(second.approval_count, 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rebuilds_query_projections_from_durable_events() {
        let store = Store::open_in_memory().expect("store");
        let first_session_id = SessionId::new();
        let second_session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());

        store
            .append_events(
                first_session_id,
                Some(ProviderId("builtin.agent.mock".to_string())),
                [
                    Event::MessageCommitted(Message {
                        role: MessageRole::User,
                        text: "snapshot".to_string(),
                    }),
                    Event::ToolCallRequested(ToolCallRequest {
                        call_id: call_id.clone(),
                        tool_id: "workspace.snapshot".to_string(),
                        input: serde_json::json!({}),
                    }),
                    Event::ApprovalRequested(ApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "approval".to_string(),
                    }),
                    Event::ToolCallFinished(ToolCallResult {
                        call_id,
                        output: serde_json::json!({ "tab_count": 1 }),
                    }),
                ],
            )
            .expect("append first session");
        store
            .append_events(
                second_session_id,
                Some(ProviderId("builtin.agent.rig".to_string())),
                [Event::MessageCommitted(Message {
                    role: MessageRole::Assistant,
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
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let call_id = ToolCallId("call-1".to_string());
        let records = vec![
            crate::persistence::event_log::Record {
                schema: crate::persistence::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-1".to_string(),
                sequence: 0,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
                role_id: None,
                event_kind: event_kind(&Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "snapshot".to_string(),
                }))
                .to_string(),
                event: Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "snapshot".to_string(),
                }),
                provider_payload: Some(serde_json::json!({ "source": "jsonl" })),
                created_at_unix_ms: 1,
            },
            crate::persistence::event_log::Record {
                schema: crate::persistence::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-2".to_string(),
                sequence: 1,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
                role_id: None,
                event_kind: event_kind(&Event::ToolCallRequested(ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "workspace.snapshot".to_string(),
                    input: serde_json::json!({}),
                }))
                .to_string(),
                event: Event::ToolCallRequested(ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "workspace.snapshot".to_string(),
                    input: serde_json::json!({}),
                }),
                provider_payload: None,
                created_at_unix_ms: 2,
            },
            crate::persistence::event_log::Record {
                schema: crate::persistence::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-3".to_string(),
                sequence: 2,
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
                role_id: None,
                event_kind: event_kind(&Event::ToolCallFinished(ToolCallResult {
                    call_id: call_id.clone(),
                    output: serde_json::json!({ "ok": true }),
                }))
                .to_string(),
                event: Event::ToolCallFinished(ToolCallResult {
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
                [Event::MessageCommitted(Message {
                    role: MessageRole::Assistant,
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

    /// The bug this column fixes: a full rebuild used to stamp every row
    /// with `DEFAULT now()` at (re)insert time, clustering a session's
    /// entire history within about a second regardless of how far apart
    /// the real events were. Spreads the fixture's timestamps across
    /// several real days -- not milliseconds -- so that bug would be
    /// obvious, not just a rounding error, and reads `event_at` back out
    /// via `epoch_ms(event_at)` (DuckDB's own reverse of the `epoch_ms(?)`
    /// conversion `import::insert_event_log_record` writes with) to prove
    /// an exact round trip of `Record::created_at_unix_ms`.
    #[test]
    fn rebuild_projects_real_event_timestamps_into_event_at_column() {
        let store = Store::open_in_memory().expect("store");
        let session_id = SessionId::new();
        let day_ms: u64 = 24 * 60 * 60 * 1000;
        let timestamps: Vec<u64> = vec![
            1_700_000_000_000,
            1_700_000_000_000 + day_ms,
            1_700_000_000_000 + 3 * day_ms,
        ];
        let records = timestamps
            .iter()
            .enumerate()
            .map(
                |(index, &created_at_unix_ms)| crate::persistence::event_log::Record {
                    schema: crate::persistence::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                    version: crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                    event_id: format!("event-{index}"),
                    sequence: index as u64,
                    session_id,
                    turn_id: None,
                    provider_id: None,
                    role_id: None,
                    event_kind: "state_changed".to_string(),
                    event: Event::StateChanged(SessionState::Running),
                    provider_payload: None,
                    created_at_unix_ms,
                },
            )
            .collect::<Vec<_>>();

        store
            .replace_from_event_log_records(records)
            .expect("replace from records");

        let session_id_text = session_id_text(session_id).expect("session id text");
        let mut stmt = store
            .conn
            .prepare(
                "SELECT event_id, epoch_ms(event_at) FROM agent_events
                 WHERE session_id = ? ORDER BY sequence",
            )
            .expect("prepare");
        let rows = stmt
            .query_map(params![&session_id_text], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query_map")
            .map(|row| row.expect("row"))
            .collect::<Vec<_>>();

        let expected = timestamps
            .iter()
            .enumerate()
            .map(|(index, &ts)| (format!("event-{index}"), ts as i64))
            .collect::<Vec<_>>();
        assert_eq!(
            rows, expected,
            "event_at must round-trip each record's real created_at_unix_ms exactly"
        );
    }

    /// A `.duckdb` file from before `event_at` existed has `agent_events`
    /// in its old shape (`created_at TIMESTAMP NOT NULL DEFAULT now()`, no
    /// `event_at`). `CREATE TABLE IF NOT EXISTS` alone would leave it
    /// exactly as-is; `Store::open` must detect the stale shape and
    /// migrate it before the store is usable.
    #[test]
    fn migrates_pre_event_at_agent_events_table_on_open() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-legacy-schema-{}.duckdb",
            Uuid::new_v4()
        ));
        let session_id = SessionId::new();
        let session_id_text = session_id_text(session_id).expect("session id text");

        {
            // Hand-build the pre-`event_at` schema (see `schema.rs`'s
            // comment for the shape it replaced) and seed it with a stale
            // row, modeling a real leftover file from an older Horizon
            // build.
            let legacy = Connection::open(&path).expect("open legacy connection");
            legacy
                .execute_batch(
                    "CREATE TABLE agent_events (
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
                    );",
                )
                .expect("create legacy table");
            legacy
                .execute(
                    "INSERT INTO agent_events (
                        event_id, session_id, turn_id, sequence, event_kind, horizon_event_json
                    ) VALUES ('stale-event', ?, NULL, 0, 'state_changed', '\"stale\"')",
                    params![&session_id_text],
                )
                .expect("seed legacy row");
        }

        // `Store::open` runs the migration before `INITIALIZE_SCHEMA_SQL`.
        let store = Store::open(&path).expect("open store over legacy file");

        let has_event_at: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM information_schema.columns
                 WHERE table_name = 'agent_events' AND column_name = 'event_at'",
                [],
                |row| row.get(0),
            )
            .expect("check migrated column");
        assert_eq!(has_event_at, 1, "migration must add the event_at column");

        let stale_row_count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0))
            .expect("count rows after migration");
        assert_eq!(
            stale_row_count, 0,
            "migration drops and recreates the stale table; the old row does not survive \
             (the rebuild that always follows in production repopulates it from JSONL)"
        );

        // The store keeps working normally post-migration: a real rebuild
        // still lands the JSONL record's real timestamp in `event_at`.
        store
            .replace_from_event_log_records([crate::persistence::event_log::Record {
                schema: crate::persistence::event_log::AGENT_EVENT_LOG_SCHEMA.to_string(),
                version: crate::persistence::event_log::AGENT_EVENT_LOG_VERSION,
                event_id: "event-after-migration".to_string(),
                sequence: 0,
                session_id,
                turn_id: None,
                provider_id: None,
                role_id: None,
                event_kind: "state_changed".to_string(),
                event: Event::StateChanged(SessionState::Running),
                provider_payload: None,
                created_at_unix_ms: 1_700_000_000_000,
            }])
            .expect("replace from records after migration");

        let event_at_ms: i64 = store
            .conn
            .query_row(
                "SELECT epoch_ms(event_at) FROM agent_events WHERE event_id = 'event-after-migration'",
                [],
                |row| row.get(0),
            )
            .expect("query event_at after migration");
        assert_eq!(event_at_ms, 1_700_000_000_000);

        let _ = std::fs::remove_file(path);
    }

    /// End-to-end for the *live* projection (task 1 of the recall work),
    /// not just the rebuild-at-startup path the tests above cover: drives
    /// real appends through `event_log::WriterHandle::open_silently(path,
    /// Some(duckdb_path))` -- the exact seam `horizon-agentd`'s
    /// `open_persistence` uses -- then opens a *fresh*, independent
    /// `Store::open` in this same process (proving same-process concurrent
    /// opens don't contend, per this feature's measured premises) and
    /// checks the rows are already there, with each record's own
    /// `event_at`, not a rebuild discovering them at the next restart and
    /// not a `now()` stamp from the writer thread's own append time.
    #[test]
    fn live_projection_reflects_writer_thread_appends_with_correct_event_at() {
        use crate::persistence::event_log::{
            Record, WriterHandle, WriterInit, AGENT_EVENT_LOG_SCHEMA, AGENT_EVENT_LOG_VERSION,
        };

        let event_log_path = std::env::temp_dir().join(format!(
            "horizon-agent-live-duckdb-events-{}.jsonl",
            Uuid::new_v4()
        ));
        let duckdb_path = std::env::temp_dir().join(format!(
            "horizon-agent-live-duckdb-state-{}.duckdb",
            Uuid::new_v4()
        ));
        let session_id = SessionId::new();

        let (writer, init_rx) =
            WriterHandle::open_silently(&event_log_path, Some(duckdb_path.clone()));
        match init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(_) => {}
            WriterInit::Failed(error) => panic!("unexpected startup failure: {error}"),
        }

        let record_at = |created_at_unix_ms: u64| Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: Uuid::new_v4().to_string(),
            sequence: 0, // placeholder -- the writer thread assigns the real one
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            event_kind: "state_changed".to_string(),
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            created_at_unix_ms,
        };

        writer
            .append(record_at(1_700_000_000_000))
            .expect("append 0");
        writer
            .append(record_at(1_700_000_050_000))
            .expect("append 1");
        writer.flush().expect("flush");

        let reopened = Store::open(&duckdb_path).expect("reopen duckdb store");
        let session_id_text = session_id_text(session_id).expect("session id text");
        let mut stmt = reopened
            .conn
            .prepare(
                "SELECT sequence, epoch_ms(event_at) FROM agent_events
                 WHERE session_id = ? ORDER BY sequence",
            )
            .expect("prepare");
        let rows = stmt
            .query_map(params![&session_id_text], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query_map")
            .map(|row| row.expect("row"))
            .collect::<Vec<_>>();

        assert_eq!(
            rows,
            vec![(0, 1_700_000_000_000), (1, 1_700_000_050_000)],
            "the writer thread's live per-append projection must assign real sequences and \
             carry each record's own event_at, not a rebuild-time now() stamp"
        );

        let _ = std::fs::remove_file(&event_log_path);
        let _ = std::fs::remove_file(&duckdb_path);
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
            Store::open_in_memory().expect("open in-memory store"),
            event_count,
            bench_delta_event,
            None,
        );

        run_append_projection_bench(
            "in-memory mixed turn",
            Store::open_in_memory().expect("open in-memory store"),
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
            Store::open(&path).expect("open file-backed store"),
            event_count,
            bench_delta_event,
            Some(path),
        );
    }

    fn run_append_projection_bench(
        label: &str,
        store: Store,
        event_count: usize,
        event_at: impl Fn(usize) -> Event,
        cleanup_path: Option<std::path::PathBuf>,
    ) {
        let session_id = SessionId::new();
        let provider_id = Some(ProviderId("bench.agent".to_string()));
        let mut append_durations = Vec::with_capacity(event_count);

        let total_start = Instant::now();
        for index in 0..event_count {
            let start = Instant::now();
            store
                .append_event(AppendEvent {
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

    fn bench_delta_event(index: usize) -> Event {
        if index.is_multiple_of(2) {
            Event::ReasoningDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: format!("reasoning delta {index}\n"),
            })
        } else {
            Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: format!("assistant delta {index}\n"),
            })
        }
    }

    fn bench_mixed_turn_event(index: usize) -> Event {
        match index % 10 {
            0 => Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: format!("user message {index}"),
            }),
            1 | 2 => Event::ReasoningDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: format!("thinking chunk {index}\n"),
            }),
            3..=5 => Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: format!("assistant chunk {index}\n"),
            }),
            6 => Event::ToolCallRequested(ToolCallRequest {
                call_id: ToolCallId(format!("call-{index}")),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({ "index": index }),
            }),
            7 => Event::ApprovalRequested(ApprovalRequest {
                call_id: ToolCallId(format!("call-{}", index - 1)),
                reason: "benchmark approval".to_string(),
            }),
            8 => Event::ToolCallFinished(ToolCallResult {
                call_id: ToolCallId(format!("call-{}", index - 2)),
                output: serde_json::json!({ "ok": true, "index": index }),
            }),
            _ => Event::MessageCommitted(Message {
                role: MessageRole::Assistant,
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
}
