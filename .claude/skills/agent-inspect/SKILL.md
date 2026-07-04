---
name: agent-inspect
description: Investigate Horizon agent-session behavior from its persisted JSONL event log and optional DuckDB projection - trace an agent session, replay events, find out why an agent session appeared to freeze or went silent, inspect agent history, attribute a gap to provider latency vs. tool execution vs. approval wait, extract tool call args/results, or locate bash output spill files. Trigger words: trace an agent session, replay events, why did it freeze, inspect agent history, agent latency, provider gap, agent silence.
---

# Inspecting Horizon agent sessions

Horizon persists every `agent::contract::Event` from every agent session to one
append-only JSONL log, and (optionally) rebuilds a DuckDB projection from it at
startup. Both are read-only-safe to poke at from outside the app. This skill is
about *reading* that history, not the app's live runtime.

## Where the data lives

- **JSONL event log** (the durable source of truth): path from the
  `HORIZON_AGENT_EVENT_LOG` env var, then `config.toml`'s `[agent].event_log_path`,
  falling back to `$XDG_DATA_HOME/horizon/agent-events.jsonl` (commonly
  `~/.local/share/horizon/agent-events.jsonl` on Linux if `XDG_DATA_HOME` is
  unset) — see `crates/horizon-agent/src/config.rs`'s `AgentPersistenceConfig`. One file, one
  background writer thread, shared by every agent session in the process —
  sessions interleave in it, distinguished by `session_id`.
- **DuckDB projection** (optional, rebuildable, *not* the source of truth):
  exists only if `HORIZON_AGENT_STATE_DB` (or `config.toml`'s
  `[agent].state_db_path`) was set to a file path (conventionally `*.duckdb`)
  when Horizon last started; unset means no DuckDB file at all — this one has
  no built-in default path.
- **Bash tool output spill files**: every `bash` tool call writes its full,
  uncapped output to `<temp dir>/horizon-bash-<uuid>.log`, referenced by the
  tool result's `output_file` field — always, regardless of whether the
  in-context output was truncated (`crates/horizon-agent/src/tools/bash/output.rs`).

## JSONL record shape

Each line is one `agent::persistence::event_log::Record` (`crates/horizon-agent/src/persistence/event_log/mod.rs`):

| field | meaning |
|---|---|
| `schema` | always `"horizon.agent.event_log"` |
| `version` | always `1` |
| `event_id` | uuid string, unique per record |
| `sequence` | `u64`, **globally monotonic across the whole file** — not reset per session |
| `session_id` | uuid string |
| `turn_id` | string or `null`; opened by a user `message_committed` (which itself carries the new id) and closed by the next `state_changed` to `WaitingForUser`/`WaitingForApproval`/`Cancelled`/`Failed`/`Terminated` (which still carries the id); events outside a turn are `null` |
| `provider_id` | string or `null`, e.g. `"builtin.agent.rig"`, `"builtin.agent.mock"` |
| `event_kind` | snake_case string, see below |
| `event` | the `contract::Event`, serde's default externally-tagged form: `{"MessageCommitted":{"role":"User","text":"..."}}`, `{"ToolCallStarted":"call-1"}` (tuple variant wrapping a plain string), or a bare string like `"ProviderRequestFirstToken"` for a fieldless variant |
| `provider_payload` | arbitrary JSON or `null`; provider-owned, opaque (e.g. rig tool-call metadata) |
| `created_at_unix_ms` | `u64` unix-epoch milliseconds — **the only reliable per-event real-world timestamp in the whole system** (see the DuckDB caveat below) |

`event_kind` values (`agent::contract::event_kind`): `state_changed`,
`reasoning_delta`, `assistant_text_delta`, `message_committed`,
`tool_call_requested`, `tool_call_started`, `tool_call_finished`,
`approval_requested`, `provider_request_sent`, `provider_request_first_token`,
`provider_request_finished`, `error`, `exited`.

The last three are turn-request lifecycle markers bracketing a turn's round
trip to the model provider: `provider_request_sent` (carries `{"model": "..."}`)
when the request leaves Horizon, `provider_request_first_token` when the first
chunk of any kind comes back, `provider_request_finished` when the provider's
response stream ends. They exist specifically so the silence between a user
message and the first delta can be attributed to provider latency instead of
guessed at — see the gap-attribution recipe below.

## Tolerant parsing — the log can contain torn lines

The log is append-only text, and a hard kill or (historically, before Horizon's
single-writer-per-process fix) a race between two writers can leave a torn or
corrupt line. Piping the raw file straight through `jq` dies on the first bad
line and silently drops everything after it — confirmed against a real
`/tmp/horizon-agent-events.jsonl` on a dev machine that had exactly one such
line: plain `jq '.field' file` stopped dead at it, one line in.

Always parse tolerantly first, then slurp for anything that needs
cross-record work:

```sh
LOG=~/.local/share/horizon/agent-events.jsonl   # or $HORIZON_AGENT_EVENT_LOG

# Sanity check: how many lines actually parse?
jq -R 'fromjson? // empty' "$LOG" | jq -s 'length'
```

`-R` reads each line as a raw string; `fromjson?` tries to parse it and turns
a failure into an error `jq` can catch; `// empty` drops the ones that fail.
Every recipe below starts with this same first stage.

### List sessions with time ranges

```sh
jq -R 'fromjson? // empty' "$LOG" | jq -s '
  group_by(.session_id) | map({
    session_id: .[0].session_id,
    provider_id: ([.[].provider_id] | map(select(. != null)) | first),
    events: length,
    started_at: (map(.created_at_unix_ms) | min),
    ended_at: (map(.created_at_unix_ms) | max)
  }) | sort_by(.started_at)[]
'
```

### Replay one session's timeline with per-event gaps

```sh
SID=<session-id>
jq -R 'fromjson? // empty' "$LOG" | jq -cs --arg sid "$SID" '
  map(select(.session_id == $sid)) | sort_by(.sequence)
  | .[0].created_at_unix_ms as $t0
  | foreach .[] as $e
      ({prev: null};
       {prev: $e.created_at_unix_ms,
        out: {sequence: $e.sequence, event_kind: $e.event_kind, turn_id: $e.turn_id,
              t_ms: ($e.created_at_unix_ms - $t0),
              gap_ms: (if .prev == null then 0 else ($e.created_at_unix_ms - .prev) end)}};
       .out)
'
```

`foreach`'s state (`.prev`) and its emitted value (`.out`) are computed
together from the *old* state each step, so `gap_ms` correctly measures the
step just taken rather than always coming out `0`.

### Find big gaps across every session

```sh
jq -R 'fromjson? // empty' "$LOG" | jq -cs '
  group_by(.session_id) | map(
    (sort_by(.sequence)) as $evts
    | [foreach $evts[] as $e
        ({prev:null};
         {prev: $e.created_at_unix_ms,
          out: {session_id: $e.session_id, sequence: $e.sequence, event_kind: $e.event_kind,
                gap_ms: (if .prev == null then 0 else ($e.created_at_unix_ms - .prev) end)}};
         .out)]
  ) | add | map(select(.gap_ms > 2000)) | sort_by(-.gap_ms)[]
'
```

(2000ms threshold is arbitrary — adjust to taste.) Wrap the per-group
`foreach` in `[...]` so `map` yields an array of arrays, then use `add` to
concatenate them into one flat array — not `flatten`, which recurses into
nested arrays and will also shred any event whose JSON happens to contain an
array-valued field (e.g. `fs.glob` results).

### Attribute a gap using the provider-request lifecycle events

Every gap is "waiting on" whatever event immediately preceded it:

```sh
jq -R 'fromjson? // empty' "$LOG" | jq -cs --arg sid "$SID" '
  map(select(.session_id == $sid)) | sort_by(.sequence)
  | foreach .[] as $e
      ({prev:null, prev_kind:null};
       {prev: $e.created_at_unix_ms, prev_kind: $e.event_kind,
        out: {sequence: $e.sequence, event_kind: $e.event_kind,
              gap_ms: (if .prev == null then 0 else ($e.created_at_unix_ms - .prev) end),
              waited_on: (
                if .prev_kind == "provider_request_sent" then "provider latency (time to first token)"
                elif .prev_kind == "tool_call_started" then "tool execution"
                elif .prev_kind == "approval_requested" then "waiting on user/policy approval"
                else "local processing" end)}};
       .out)
'
```

A big `gap_ms` whose `waited_on` is `"provider latency..."` means the model
was slow to respond — nothing Horizon-side to chase. Any other `waited_on`
value with a big gap points at tool execution, an approval nobody answered,
or local processing worth profiling separately. Verified against a small
hand-built fixture with all three lifecycle events during development (real
capture files from before this feature won't have `provider_request_*` kinds
yet — those only start appearing in logs written after this change ships).

### Extract tool calls with args and results

```sh
jq -R 'fromjson? // empty' "$LOG" | jq -cs --arg sid "$SID" '
  map(select(.session_id == $sid and (.event_kind=="tool_call_requested" or .event_kind=="tool_call_finished")))
  | sort_by(.sequence)
  | map(
      if .event_kind == "tool_call_requested" then
        {call_id: .event.ToolCallRequested.call_id, tool_id: .event.ToolCallRequested.tool_id,
         requested_at: .created_at_unix_ms, input: .event.ToolCallRequested.input}
      else
        {call_id: .event.ToolCallFinished.call_id, finished_at: .created_at_unix_ms,
         output: .event.ToolCallFinished.output}
      end)
  | group_by(.call_id) | map(add)[]
'
```

### Locate bash spill files

```sh
jq -R 'fromjson? // empty' "$LOG" | jq -cs '
  map(select(.event_kind=="tool_call_finished" and (.event.ToolCallFinished.output.output_file? != null)))
  | map({call_id: .event.ToolCallFinished.call_id, output_file: .event.ToolCallFinished.output.output_file})[]
'
```

Or skip the JSONL entirely and just glob the temp dir (loses the call/session
attribution the query above gives you): `ls -la "$(dirname "$LOG")"/horizon-bash-*.log`.

## DuckDB projection

Requires the separate `duckdb` CLI (`command -v duckdb`) — it is not bundled
with Horizon and must be installed independently. Only useful if
`HORIZON_AGENT_STATE_DB` was set the last time Horizon started; there is
nothing to inspect if the file doesn't exist.

Schema (`crates/horizon-agent/src/persistence/projection/duckdb/schema.rs`):

```
agent_sessions(session_id PK, provider_id, last_sequence, updated_at)
agent_events(event_id PK, session_id, turn_id, sequence, event_kind,
             horizon_event_json, provider_id, provider_payload_json, created_at)
agent_messages(event_id PK, session_id, sequence, role, text, is_delta)
agent_tool_calls(event_id PK, session_id, sequence, call_id, tool_id, input_json)
agent_tool_results(event_id PK, session_id, sequence, call_id, output_json)
agent_approvals(event_id PK, session_id, sequence, call_id, reason)
```

`provider_request_sent`/`provider_request_first_token`/`provider_request_finished`
land in `agent_events` (with their `event_kind` and full `horizon_event_json`)
but have no dedicated projection table — the exhaustive match in
`projection::project_event` treats them as a documented no-op. Query
`agent_events` directly for them, or use the JSONL recipes above.

**Critical caveat, confirmed against a real rebuilt file:** `agent_events.created_at`
is when the row was (re)inserted into DuckDB, *not* the event's real time.
Horizon fully rebuilds the DuckDB file from the JSONL log once at startup
(`replace_from_event_log_records` clears every table and reinserts all of
it), and `created_at` gets DuckDB's own `DEFAULT now()` at that moment — the
JSONL record's `created_at_unix_ms` is never copied into any DuckDB column.
On a real capture, every row from one rebuild had `created_at` clustered
within about a second of each other, even though the underlying events
actually spanned several days. **Do gap/timing analysis against the JSONL
log, not DuckDB.** Also note the file only reflects the JSONL as of the last
app start; a session still running right now may not be in it yet.

The rebuild's `Store`/`Connection` is closed again immediately after startup
finishes, so it's safe to open the file while Horizon is running. Open
read-only anyway, out of habit:

```sh
DB=/tmp/horizon-agent-state.duckdb   # wherever HORIZON_AGENT_STATE_DB points

duckdb -readonly "$DB" -c "
  SELECT session_id, provider_id, last_sequence, updated_at
  FROM agent_sessions ORDER BY updated_at DESC;"

duckdb -readonly "$DB" -c "
  SELECT session_id, event_kind, COUNT(*) AS n
  FROM agent_events GROUP BY 1,2 ORDER BY session_id, n DESC;"

duckdb -readonly "$DB" -c "
  SELECT c.sequence, c.tool_id, c.call_id, c.input_json, r.output_json
  FROM agent_tool_calls c LEFT JOIN agent_tool_results r USING (call_id)
  ORDER BY c.sequence;"

duckdb -readonly "$DB" -c "
  SELECT sequence, role, text FROM agent_messages
  WHERE session_id = '<session-id>' AND NOT is_delta ORDER BY sequence;"
```

A pre-existing local `.duckdb` file may carry extra legacy tables/columns
from an older Horizon version (confirmed on a real dev machine: an
`agent_conversation_messages` table not in the current schema at all) —
schema application is `CREATE TABLE IF NOT EXISTS`, additive only, and never
migrates or drops stale tables. Treat `schema.rs` as authoritative for what's
current; `.tables`/`DESCRIBE` may show more than that.
