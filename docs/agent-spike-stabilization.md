# Agent Spike Stabilization Notes

Date: 2026-06-30

> **Superseded.** This note's recommendations (JSONL as the primary durable
> log, DuckDB as a rebuildable projection, a non-blocking log writer,
> turn-level persistence granularity) were adopted — see
> `docs/agent-duckdb-state-design.md`'s "Runtime Boundary" section for the
> shipped shape. The runtime they describe has since moved out of the
> Horizon process entirely: `horizon-agentd` now owns the event log and the
> DuckDB projection — see `docs/agent-runtime-split-design.md`.

This note records the stabilization pass after the first Agent pane, Rig, and
DuckDB implementation passes. It separates measured facts from design
implications so the next implementation step does not continue from guesswork.

## Current Implementation Shape

The current agent stack has these primary pieces:

- `src/agent/mod.rs` is the Horizon-owned command/event/frame contract.
- `src/agent/rig.rs` is the current Rig provider bridge.
- `src/agent/event_log.rs` is the JSONL durable event log.
- `src/agent/duckdb_state.rs` is the derived DuckDB projection layer.
- `src/agent/view/mod.rs` is the first rich Agent pane renderer.
- The genai spike has been removed; genai remains only as comparative research
  material in historical notes.

The core direction is sound: Horizon owns the normalized session/event/tool/UI
contract, and provider frameworks stay behind provider modules. The main
remaining instability is not the provider boundary. It is the granularity of
runtime events versus durable persistence.

## DuckDB Measurement

An ignored micro benchmark was added:

```bash
HORIZON_AGENT_DUCKDB_BENCH_EVENTS=500 \
  cargo test \
  agent::duckdb_state::tests::bench_append_projection_costs \
  -- --ignored --nocapture
```

Measured on this workstation with 500 events:

| Scenario | Total append | Avg | p50 | p95 | Max | Events query | Messages query | Frame query |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| in-memory deltas | 11300.323ms | 22.599ms | 19.426ms | 36.729ms | 42.926ms | 9.613ms | 7.506ms | 12.645ms |
| in-memory mixed turn | 9868.146ms | 19.734ms | 18.349ms | 27.991ms | 40.061ms | 7.543ms | 3.853ms | 6.882ms |
| file-backed deltas | 9469.079ms | 18.936ms | 17.998ms | 25.072ms | 42.240ms | 6.854ms | 4.379ms | 5.676ms |

Interpretation:

- The current append path is heavy enough to explain UI stalls when streaming
  deltas are persisted synchronously.
- File-backed DuckDB was not obviously worse than in-memory in this run; the
  issue is the per-event synchronous path, not simply disk I/O.
- Query/readback is comparatively small at this scale.
- The benchmark measures `append_event`, which currently includes:
  - `MAX(sequence)` lookup,
  - event JSON serialization,
  - insert into `agent_events`,
  - session upsert,
  - projection insert.

This makes delta-by-delta durable writes the wrong default for a streaming UI.

## Rig Integration Granularity

Rig has two relevant extension areas:

- `ConversationMemory` for conversation history.
- `VectorStoreIndex` / companion vector store crates for retrieval.

`rig-core` defines `ConversationMemory` as:

- `load(conversation_id) -> Vec<Message>`
- `append(conversation_id, Vec<Message>)`
- `clear(conversation_id)`

Rig calls memory append after a successful turn, not for each streaming delta.
Its own docs say append runs inline before the agent returns, so backends should
keep it cheap. The streaming prompt implementation appends
`response.messages.clone().unwrap_or_default()` only after
`AgentRunStep::Done`.

The `rig-memory` companion crate provides policies and adapters over this
conversation-memory boundary: sliding windows, token windows, demotion hooks,
and compaction. It does not change the base persistence granularity into
streaming deltas.

The root Rig facade lists many DB/vector companion crates, including LanceDB,
MongoDB, PostgreSQL, SQLite, SurrealDB, Qdrant, Milvus, ScyllaDB, HelixDB, and
others. These are primarily vector store or backend integrations behind Rig's
retrieval abstractions, not a reason for Horizon to persist UI streaming deltas
as database rows.

References:

- Local `rig-core-0.39.0/src/memory.rs`
- Local `rig-core-0.39.0/src/agent/prompt_request/streaming.rs`
- <https://docs.rs/rig-memory/latest/rig_memory/>
- <https://github.com/0xPlaygrounds/rig>

## Design Implications

Horizon should split live UI events from durable history:

- Live UI may keep `ReasoningDelta` and `AssistantTextDelta` for responsiveness.
- Durable history should prefer turn/message-level commits.
- If intermediate deltas are retained, they should be sampled, batched, or
  explicitly treated as diagnostic trace data.
- DuckDB should not be called synchronously on every streamed delta from the UI
  thread's event path.

The next persistence design should align closer to Rig's memory boundary:

```text
turn start
  user message
  streaming reasoning/text/tool events for UI
turn final
  durable turn record
  durable committed messages/tool calls/tool results/approvals
  optional provider payloads for replay/debugging
```

This does not mean Horizon must adopt Rig memory as the source of truth.
Horizon can still keep DuckDB as the durable local state layer. The important
adjustment is to persist at a turn/coalesced-message boundary, while retaining
the normalized Horizon event stream for live rendering and policy execution.

## Recommended Next Step

The event persistence boundary is now stable enough to build on.

The preferred design is now:

```text
provider event
  -> Horizon policy/tool processing
  -> live AgentRuntimeState / AgentFrame for UI
  -> JSONL durable event log writer
  -> DuckDB projection rebuild or tailing ingest
```

This keeps DuckDB in the architecture, but moves it from the live streaming
append path to rebuildable read-model/projection work.

## Persistence Boundary

The correct durable raw-log boundary is after
`process_agent_provider_event(...)`, not before it.

Rationale:

- The provider-origin event is still preserved because the first processed
  `AgentProviderEvent` keeps the original `provider_payload`.
- Horizon policy events, approval requests, auto tool execution state, and tool
  results are also part of the user-visible agent history.
- The UI already renders from these processed Horizon events.
- Replaying from this boundary can reconstruct the same provider-neutral
  `AgentFrame` without re-executing tools.

The current synchronous path is:

```text
create_signal_from_channel(handle.events())
  -> process_agent_provider_event(workspace, provider_event)
  -> runtime_state.extend_provider_events(processing.horizon_events)
       -> optional DuckDB append_event per event
       -> in-memory AgentFrame update
  -> workspace.update_agent_frame(...)
```

The next path should split this:

```text
processed AgentProviderEvent batch
  -> in-memory AgentFrame update
  -> non-blocking log writer enqueue
```

DuckDB projection should consume the log later, or rebuild from it on demand.

## JSONL Event Log Contract

The JSONL log should be the durable source for raw processed Horizon events.
DuckDB tables are derived and rebuildable.

Suggested line schema:

```json
{
  "schema": "horizon.agent.event_log",
  "version": 1,
  "event_id": "uuid",
  "sequence": 123,
  "session_id": "uuid",
  "turn_id": "turn-uuid-or-null",
  "provider_id": "builtin.agent.rig",
  "event_kind": "assistant_text_delta",
  "event": {},
  "provider_payload": null,
  "created_at_unix_ms": 1782782450000
}
```

Rules:

- `sequence` is monotonically increasing per log file. It does not need to be
  gap-free after crashes.
- `event_id` is stable and should be used for idempotent projection ingest.
- The writer should be single-owner inside the Horizon process.
- Writes should be sent to a background thread/task over a channel.
- Flush can be buffered; strict fsync-per-event is not required for the current
  UX goal.
- Shutdown should request a final flush, but loss of the last few events on
  crash is acceptable unless a future setting opts into stronger durability.
- Readers should ignore a trailing partial line.
- Corrupt complete lines should be reported and skipped or quarantined; they
  must not prevent reading later valid lines.

This directly matches the current consistency needs: UI correctness comes from
the live runtime state, and durable state is eventually consistent.

## Turn Boundary

There is no explicit turn model yet. Existing code uses implicit boundaries:

- `MessageCommitted(User)` naturally starts a user turn.
- `ReasoningDelta` and `AssistantTextDelta` belong to the active turn.
- `MessageCommitted(Assistant)` finalizes assistant text for the active turn.
- Tool call/request/result/approval events currently act as frame boundaries in
  the UI coalescing helper.

For persistence, the least risky next step is to add turn ids in the log writer,
not by changing every provider command yet.

Recommended v1 turn rules:

- Start a new turn when logging `MessageCommitted(User)`.
- Keep subsequent reasoning/text/tool/approval events in that turn.
- Keep auto tool execution results in the same turn as the tool request.
- If a tool result is sent back to the provider and causes a continuation, keep
  that continuation in the same turn until an assistant message is committed or
  the session enters `WaitingForUser`.
- Close the turn when `StateChanged(WaitingForUser)`,
  `StateChanged(WaitingForApproval)`, `StateChanged(Failed)`, or
  `StateChanged(Terminated)` is logged.
- System/lifecycle initialization messages may use `turn_id = null` unless a
  later UI needs them grouped.

The exact rules can be implemented in an `AgentTurnTracker` owned by the log
writer/projector. Providers do not need to emit turn ids in v1.

## Rig Memory Projection

Rig-compatible memory requires the same effective granularity as
`ConversationMemory`:

```text
conversation_id -> ordered Vec<rig_core::completion::Message>
```

JSONL + DuckDB can provide this by projecting processed events into a
`conversation_messages` read model:

```text
agent_conversation_messages
  event_id
  session_id
  conversation_id
  turn_id
  sequence
  provider_id
  rig_message_json
  horizon_event_kind
```

Initial mapping can reuse `rig_messages_from_horizon_events`:

- `MessageCommitted(User)` -> `Message::user(...)`
- `MessageCommitted(Assistant)` -> `Message::assistant(...)`
- `ToolCallRequested` -> Rig tool call message
- `ToolCallFinished` -> Rig tool result message
- `Error` -> assistant error message
- streaming deltas are ignored for Rig memory unless no final committed
  assistant message exists.

Important limitation: current Horizon-to-Rig reconstruction loses some
provider-native tool metadata unless `provider_payload` is consulted. The log
schema preserves `provider_payload`, so the projection can later prefer exact
Rig payloads for tool calls when available.

`conversation_id` can initially be the Horizon `session_id`. A later provider
setting can override it if Horizon supports sharing one conversation across
sessions.

## DuckDB Ingest Strategy

Use a staged implementation:

1. JSONL append writer only.
2. Rebuild DuckDB projections from JSONL on startup or explicit command.
3. Add tailing/incremental projector only if startup rebuild becomes too slow or
   live query UX needs it.

For v1, DuckDB should use `event_id` uniqueness to make rebuild/ingest
idempotent. Projection tables should remain derived.

This keeps failure recovery simple:

- JSONL is source of truth.
- DuckDB can be deleted and rebuilt.
- Partial last line is ignored.
- Duplicate JSONL lines are harmless if `event_id` is stable.
- Sequence gaps are diagnostics, not fatal errors.

## Implementation Order

1. Add JSONL event log types and writer as the standard persistence path.
2. Replace synchronous DuckDB calls in `AgentRuntimeStateStore` with
   non-blocking log enqueue.
3. Add JSONL read/replay tests, including partial/corrupt line cases.
4. Add DuckDB rebuild-from-JSONL path for existing event/message/tool/approval
   projections.
5. Add `agent_conversation_messages` projection for Rig-compatible memory.
6. Continue factoring `agent/rig.rs` by provider runtime, mapping, payload, and
   memory responsibilities as those areas grow.
