# Agent DuckDB State Design

This document records the first DuckDB-backed state decision for the Horizon
Agent pane.

Design date: 2026-06-29

> **Superseded in part.** The "Runtime Boundary" section describes the JSONL
> writer and DuckDB rebuild running inside the Horizon process. That
> ownership has since moved: `horizon-agentd`, not Horizon, now owns the
> event log and the DuckDB projection — see `docs/agent-runtime-split-design.md`
> (steps 3-4 and the readiness addendum). The store shape and JSONL-as-source-
> of-truth decisions below are otherwise unchanged.

## Decision Summary

Horizon should use JSONL as the agent event log and DuckDB as a projection
substrate.

Decisions:

- Persist provider-neutral `agent::contract::Event` records to JSONL as the primary durable
  audit layer.
- Preserve optional `provider_payload_json` for framework-specific replay and
  migration safety.
- Build queryable projections for transcript messages, tool calls, tool
  results, and approval requests.
- Keep the live runtime and pane rendering provider-neutral.
- Keep DuckDB out of default builds until the runtime integration is ready.
  (Superseded: the runtime integration is done and the projection now runs
  by default -- see the "Runtime Boundary" section below.)

This keeps Horizon's session and pane model independent from `rig-core`,
`genai`, or a wasm plugin implementation, while still leaving room to preserve
provider-native metadata that matters for later replay.

## Why DuckDB

DuckDB is a good fit for Horizon's first agent state layer because the agent
history is append-heavy and analysis-friendly:

- event replay needs ordered records by session,
- transcript rendering needs compact projections,
- tool and approval history benefits from structured queries,
- future inspection/debugging can use SQL directly,
- local-first state can stay in-process without a server database.

The existing project context also supports the choice:

- `quacker` uses an append-only source with DuckDB projections and SQL-oriented
  read models.
- `pokelab` uses DuckDB as a local analytical query layer for structured data.

Those precedents do not make DuckDB mandatory, but they reduce adoption risk
for a local state/query substrate in this codebase family.

## Store Shape

The state layer uses JSONL as the durable source of truth and
`agent::persistence::projection::duckdb::Store` as a derived
projection/read-model backend.

Primary event table:

```text
agent_events
  event_id
  session_id
  turn_id
  sequence
  event_kind
  horizon_event_json
  provider_id
  provider_payload_json nullable
  created_at
```

Session metadata:

```text
agent_sessions
  session_id
  provider_id
  last_sequence
  updated_at
```

Initial projections:

```text
agent_messages
agent_tool_calls
agent_tool_results
agent_approvals
```

The projections are intentionally derived from `agent::contract::Event`. If a
projection needs to change, it can be rebuilt from `agent_events` without
changing the provider contract.

Not every event needs a projection to be useful. `ProviderRequestSent`/
`ProviderRequestFirstToken`/`ProviderRequestFinished` (see
`docs/agent-provider-contract.md`) are pure timing markers bracketing a
turn's round trip to the provider; they land in `agent_events` like any other
event (with `created_at_unix_ms` in the JSONL record and `created_at` in the
DuckDB row) but `project_event`'s exhaustive match treats them as a no-op —
no dedicated table claims them. Querying `agent_events` directly by
`event_kind` is the intended way to reconstruct provider-latency gaps; see
the `agent-inspect` skill for the jq/SQL recipes.

## Provider Payload Boundary

`horizon_event_json` is the durable Horizon contract. It is used for normal
pane rendering, approval state, tool state, and transcript reconstruction.

`provider_payload_json` is optional and should be treated as provider-owned.
For a Rig-backed provider, it can retain details that Horizon does not model
directly yet:

- Rig tool call `id` and provider `call_id`,
- tool call `signature`,
- `additional_params`,
- reasoning blocks,
- provider-native token or response metadata.

Horizon core should not require this payload for basic UI behavior. Its purpose
is compatibility with provider replay, advanced memory, future migrations, and
debugging.

Rig provider payloads are versioned opaque JSON values. The current schema is
`horizon.rig.provider_payload` version `1`. The DuckDB store only preserves the
JSON value alongside the provider-neutral event.

DuckDB does not interpret provider payloads. Provider-specific replay, history
reconstruction, and migration logic belongs to the provider implementation. For
the builtin Rig provider, `agent::providers::rig` loads ordered Horizon events
from the DuckDB projection store and converts them into Rig messages in provider
code.

## Runtime Boundary

Runtime persistence writes normalized Horizon events to the JSONL event log.
DuckDB is rebuilt from JSONL and is not the primary append path.

> **Addendum (2026-07-07, recall work).** The statement above ("DuckDB is
> rebuilt from JSONL and is not the primary append path") still describes
> JSONL's role as the source of truth, but the projection is no longer
> *only* rebuilt at startup: `horizon-agentd`'s event-log writer thread
> (`persistence::event_log::writer`) now opens the DuckDB store once, right
> after its startup rebuild (or skip, if already current -- the same
> freshness check as before), and *keeps* that `Store` open for the rest of
> the process's life, projecting every subsequent event live, right after
> its own JSONL line is durably written. JSONL is still authoritative and
> the startup rebuild is still the reconciliation point (a live-projection
> append failure only warns and moves on -- see `run_writer`'s doc comment);
> what changed is that a session's history is now queryable in DuckDB
> within the same run it happened in, not only after the next restart. This
> is also what powers the `recall.search`/`recall.read` agent tools (see
> `tools::recall`). One consequence: because the store is now held
> read-write for `horizon-agentd`'s entire lifetime, an external process can
> no longer open the file at all while agentd is running (see the
> `agent-inspect` skill's updated DuckDB section) -- previously the file was
> only briefly locked during a rebuild.

The projection runs by default now, at `$XDG_DATA_HOME/horizon/agent-state.duckdb`
(falling back to `~/.local/share/horizon/agent-state.duckdb`) -- there is no
"unset = disabled" state any more. `HORIZON_AGENT_STATE_DB` (or the config
file's `[agent].state_db_path`), if set, relocates the file instead of
turning the projection on; the runtime rebuilds whatever path resolves. The
file should conventionally use a `.duckdb` extension. It is a DuckDB-native
binary database file containing the Horizon agent tables, not JSONL,
Parquet, or SQLite.

If the configured file cannot be opened for rebuild or memory loading, Horizon
continues with an empty in-memory provider history so the pane can still run.
This fallback is intentionally lossy and is surfaced through the status bar and
`HORIZON_STATUS_DUMP` when it affects projection rebuild.

The JSONL read that seeds sequence numbers and the DuckDB rebuild are both
one-time, process-global costs that grow with total accumulated history, so
neither runs on the UI thread: `event_log::WriterHandle::open` hands back a
usable handle immediately and performs the read on a background thread (see
its "Ordering guarantee" doc comment for how appends made before that read
finishes still get correct sequence numbers), and `app::runtime::agent`
chains the DuckDB rebuild onto that same background initialization. The
status bar shows a transient "catching up" message while this runs, which
clears on success or is replaced by a skipped-lines/failure message —
persistence problems are never silent, but a clean rebuild leaves no
permanent status line behind.

It provides:

- append APIs for `agent::contract::Event`,
- session metadata listing through `agent_sessions`,
- ordered event replay per session,
- `AgentFrame` reconstruction from stored events,
- direct query APIs for messages, tool calls, tool results, and approvals.

The session listing API is the restore/read path before any UI restore behavior:
it can enumerate stored agent sessions with `session_id`, `provider_id`,
`last_sequence`, and `updated_at`, then use the existing per-session replay and
projection queries to inspect a selected session.

`session_snapshots()` builds the first UI-oriented read model on top of those
queries. Each snapshot contains the stored session metadata, reconstructed
`AgentFrame`, and message/tool/approval counts. It is intended for future
overview or archived-session UI without committing to startup restore,
attachment restore, or session title generation yet.

Projection tables are rebuildable. `rebuild_projections()` and
`rebuild_projections_for_session()` clear derived transcript/tool/approval rows
and regenerate them from `agent_events`. This keeps `agent_events` as the
primary durable source while allowing read-model tables to evolve.

It deliberately does not yet move these concerns into DuckDB:

- live provider channels,
- provider-specific conversation replay,
- pane focus and tab/split attachment state,
- pending in-memory command delivery,
- provider process lifecycle,
- vector memory or RAG storage.

Those can be layered later without making DuckDB the only runtime source of
truth.

The hook is intentionally at the runtime event boundary, after provider events
have been normalized through Horizon policy and tool processing. This keeps the
persistence path shared by builtin providers, future Rig/GenAI providers, and
eventually plugin-provided agents.

## Integration Plan

Recommended next steps:

1. Keep JSONL as the primary durable log and DuckDB as a rebuildable projection.
2. Continue writing provider payloads for loss-prone Rig fields.
3. Keep Rig history reconstruction in `agent::providers::rig`, fed by neutral
   ordered event reads from DuckDB.

## Non-goals

These are intentionally not decided by the DuckDB MVP:

- choosing Rig versus GenAI for the builtin real provider,
- adopting Rig memory as Horizon's primary persistent store,
- adding a vector store,
- exposing SQL directly in the UI,
- making DuckDB mandatory for plugin-provided agents.
