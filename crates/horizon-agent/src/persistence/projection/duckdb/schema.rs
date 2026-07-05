pub(super) const INITIALIZE_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id TEXT PRIMARY KEY,
    provider_id TEXT,
    last_sequence BIGINT NOT NULL DEFAULT -1,
    updated_at TIMESTAMP NOT NULL DEFAULT now()
);

-- `event_at` is the event's real wall-clock time, copied from the JSONL
-- `Record::created_at_unix_ms` on insert/rebuild (via `epoch_ms(?)`) --
-- *not* a `DEFAULT now()` insert-time stamp. A prior version of this table
-- had exactly that (`created_at TIMESTAMP NOT NULL DEFAULT now()`), which
-- made SQL timing analysis worthless: a full rebuild reinserts a session's
-- entire history in ~1s, so every row got clustered within that second
-- regardless of how many real days the events spanned (see the
-- `agent-inspect` skill's DuckDB section). `created_at` was dropped rather
-- than kept alongside `event_at`: it was never read through this crate's
-- own API (only raw SQL saw it), and keeping a proven-misleading column
-- next to the correct one just reintroduces the mistake for the next
-- reader. See `Store::migrate_legacy_agent_events_schema` (`mod.rs`) for
-- how a pre-`event_at` file gets migrated -- `CREATE TABLE IF NOT EXISTS`
-- below is additive-only and does not by itself alter an existing table.
CREATE TABLE IF NOT EXISTS agent_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    sequence BIGINT NOT NULL,
    event_kind TEXT NOT NULL,
    horizon_event_json TEXT NOT NULL,
    provider_id TEXT,
    provider_payload_json TEXT,
    event_at TIMESTAMP NOT NULL,
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

#[cfg(test)]
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
