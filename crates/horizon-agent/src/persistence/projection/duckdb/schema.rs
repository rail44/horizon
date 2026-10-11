/// The shape of the tables below. Bump it whenever a table or column
/// changes: `CREATE TABLE IF NOT EXISTS` never alters an existing file, so
/// a projection stamped with another version (or one whose stamp cannot be
/// read at all) is rebuilt from the event log, and the rebuild drops and
/// recreates every table ([`DROP_ALL_AGENT_STATE_SQL`]).
pub(super) const PROJECTION_SCHEMA_VERSION: u32 = 2;

pub(super) const INITIALIZE_SCHEMA_SQL: &str = "
-- Invalidates this derived cache when event representation or this
-- projection's own table shape changes. Written only after a complete
-- rebuild.
CREATE TABLE IF NOT EXISTS agent_projection_format (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    event_log_version INTEGER NOT NULL,
    projection_schema_version INTEGER NOT NULL
);

-- The last event log record this projection ingested: the byte offset just
-- past that record's JSONL line, and the record's identity. Committed in the
-- same transaction as the records it covers, so it never runs ahead of the
-- projected rows. Startup verifies only that one line and then reads the log
-- from `log_offset` on.
CREATE TABLE IF NOT EXISTS agent_log_cursor (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    log_offset BIGINT NOT NULL,
    sequence BIGINT NOT NULL,
    event_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id TEXT PRIMARY KEY,
    provider_id TEXT,
    -- Last-seen role for the session, mirroring how `provider_id` is
    -- carried (see `Store::upsert_session`'s `COALESCE` on conflict --
    -- role_id follows the same never-clear-to-NULL-on-a-role-less-event
    -- rule). See `docs/agent-feedback-design.md`'s decision 1 (the
    -- `role_id` projection gap).
    role_id TEXT,
    -- The latest non-null `Record::session_context`, as JSON. Same
    -- never-clear-to-NULL rule as `provider_id`, so restoring a session's
    -- placement needs no event log read.
    session_context_json TEXT,
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
-- reader. `CREATE TABLE IF NOT EXISTS` does not alter an existing table,
-- so a shape change bumps `PROJECTION_SCHEMA_VERSION` and the rebuild that
-- follows recreates every table.
CREATE TABLE IF NOT EXISTS agent_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    sequence BIGINT NOT NULL,
    event_kind TEXT NOT NULL,
    horizon_event_json TEXT NOT NULL,
    provider_id TEXT,
    -- The role active for this session when the event was recorded --
    -- fixes the `role_id` projection gap noted in
    -- `docs/agent-feedback-design.md`'s decision 1. Carried straight from
    -- `Record::role_id`, same shape as `provider_id` above.
    role_id TEXT,
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
    occurrence_id TEXT NOT NULL,
    tool_id TEXT NOT NULL,
    input_json TEXT NOT NULL,
    UNIQUE (session_id, occurrence_id)
);

CREATE TABLE IF NOT EXISTS agent_tool_results (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    call_id TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    output_json TEXT NOT NULL,
    -- Copied from ToolCallResult's explicit outcome. Its constructor owns
    -- interpretation of the tool output; denial can be an error even when
    -- the original payload has no error flag. Keep output_json unchanged.
    is_error BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_approvals (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    call_id TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    -- NULL while pending. A start acknowledges approval; a result before
    -- start records its explicit denial, cancellation or replacement outcome.
    outcome TEXT
);

-- Turn-level bookkeeping, not analytics: one row per turn recording how it
-- ended (no derived durations -- see `docs/agent-feedback-design.md`'s
-- decision 2, schema mirrors the existing per-tool-call granularity).
-- `ended_event_id` is the `agent_events` row for the `TurnEnded` event
-- itself; join through it for `event_at` rather than duplicating a
-- timestamp here.
CREATE TABLE IF NOT EXISTS agent_turns (
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    end_reason TEXT NOT NULL,
    ended_event_id TEXT NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);

";

#[cfg(test)]
pub(super) const PROJECTION_TABLES: &[&str] = &[
    "agent_messages",
    "agent_tool_calls",
    "agent_tool_results",
    "agent_approvals",
    "agent_turns",
];

/// Drops every table so a full rebuild recreates them with the current
/// shape ([`INITIALIZE_SCHEMA_SQL`] runs right after). Deleting rows would
/// keep an older file's columns.
pub(super) const DROP_ALL_AGENT_STATE_SQL: &str = "
DROP TABLE IF EXISTS agent_projection_format;
DROP TABLE IF EXISTS agent_log_cursor;
DROP TABLE IF EXISTS agent_messages;
DROP TABLE IF EXISTS agent_tool_calls;
DROP TABLE IF EXISTS agent_tool_results;
DROP TABLE IF EXISTS agent_approvals;
DROP TABLE IF EXISTS agent_turns;
DROP TABLE IF EXISTS agent_events;
DROP TABLE IF EXISTS agent_sessions;
";
