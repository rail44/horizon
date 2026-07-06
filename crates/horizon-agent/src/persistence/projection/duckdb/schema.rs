pub(super) const INITIALIZE_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id TEXT PRIMARY KEY,
    provider_id TEXT,
    -- Last-seen role for the session, mirroring how `provider_id` is
    -- carried (see `Store::upsert_session`'s `COALESCE` on conflict --
    -- role_id follows the same never-clear-to-NULL-on-a-role-less-event
    -- rule). See `docs/agent-feedback-design.md`'s decision 1 (the
    -- `role_id` projection gap).
    role_id TEXT,
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
    tool_id TEXT NOT NULL,
    input_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_tool_results (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    call_id TEXT NOT NULL,
    output_json TEXT NOT NULL,
    -- Derived at projection time from `output_json`'s own `is_error` key
    -- (the convention every tool's error output already follows -- see
    -- `docs/agent-feedback-design.md`'s decision 1), not re-parsed on every
    -- read: `Store::insert_tool_result` sets this once, from the same
    -- `serde_json::Value` it serializes into `output_json`.
    is_error BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_approvals (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    call_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    -- NULL while the approval is still pending; then 'approved' or
    -- 'denied', derived from event *order* rather than any string match
    -- (see `docs/agent-feedback-design.md`'s decision 1 and the addendum
    -- at the bottom of that file): a `ToolCallStarted` for this `call_id`
    -- means the human approved it (`Store::project_event`'s
    -- `ToolCallStarted` arm); a `ToolCallFinished` for this `call_id`
    -- arriving while `outcome` is still NULL means it was denied (a deny
    -- short-circuits without ever starting -- `tools::approval::
    -- synchronous_result(ran=false)`).
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

pub(super) const CLEAR_ALL_AGENT_STATE_SQL: &str = "
DELETE FROM agent_messages;
DELETE FROM agent_tool_calls;
DELETE FROM agent_tool_results;
DELETE FROM agent_approvals;
DELETE FROM agent_turns;
DELETE FROM agent_events;
DELETE FROM agent_sessions;
";
