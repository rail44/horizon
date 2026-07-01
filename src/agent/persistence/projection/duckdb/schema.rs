pub(super) const INITIALIZE_SCHEMA_SQL: &str = "
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

";

pub(super) const PROJECTION_TABLES: &[&str] = &[
    "agent_messages",
    "agent_tool_calls",
    "agent_tool_results",
    "agent_approvals",
];

pub(super) const CLEAR_ALL_AGENT_STATE_SQL: &str = "
DELETE FROM agent_messages;
DELETE FROM agent_tool_calls;
DELETE FROM agent_tool_results;
DELETE FROM agent_approvals;
DELETE FROM agent_events;
DELETE FROM agent_sessions;
";
