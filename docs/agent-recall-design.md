# Agent Recall Design — Live Projection and the Recall Tools

Written 2026-07-06 (roadmap item "Recall tool"; follows the Letta survey's
"retrieval over summarization" conclusion, `docs/research/letta.md`). Records
two decisions made with the owner: the DuckDB projection becomes **live**
(agentd appends events as they happen, decision "A" below), and the agent
gets two grep-shaped recall tools over it.

## Problem

The token-window history policy drops old turns from what the provider
sees; nothing lets the agent reach back for them. The DuckDB projection has
the full history — but it was rebuilt from JSONL **only at agentd startup**
(the runtime append path existed only under `cfg(test)`), and `horizon-agentd`
is long-lived across UI restarts. A recall tool over the startup-frozen
projection would miss both the current session's evicted turns and every
session created since agentd started — the primary use cases.

## Premises (measured 2026-07-06, DuckDB 1.5 / duckdb-rs "1" bundled)

- **Write volume is a non-issue**: live agent traffic peaks at ~1-2 events/s
  (streaming deltas included; ~35% of events are deltas).
- **Cross-process locking is all-or-nothing**: one process holding the file
  read-write blocks every other open, *including read-only*; N read-only
  opens coexist; a held read-only open conversely blocks a read-write open.
- **Same-process opens share the instance**: two `Store::open`s of the same
  path inside one process both succeed (libduckdb's database-instance
  cache), so an in-process long-lived writer and per-query readers never
  contend on the file lock.

## Decision A: agentd holds the projection live

`horizon-agentd` opens the Store once at startup, performs the existing
full rebuild from JSONL (unchanged — it remains the reconciliation point
that makes the projection rebuildable-by-construction), and then **keeps
the Store**, projecting each event as it is appended to the JSONL log.
JSONL remains the source of truth: a DuckDB append failure is warned and
tolerated (never blocks the JSONL write or the session), and the next
startup rebuild repairs any divergence.

Ordering and confinement: sequences are assigned by the event-log writer's
background thread, so the projection append rides the same thread with the
same materialized `Record` — one owner thread, no shared-connection
locking, and DuckDB rows always match their JSONL lines.

**Accepted consequence — external readers change.** While agentd runs, the
`.duckdb` file cannot be opened by another process at all (measured above).
`agent-inspect`'s direct `duckdb -readonly` recipes therefore only work
when agentd is stopped; the skill is updated to say so and to lead with the
JSONL (which external tooling can always read). This was weighed against
the alternative (transient open + watermark refresh per recall query, which
preserves external access but leaves the file cold between calls) and
decided deliberately: the projection is becoming a live read model — the
direction the knowledge-base ambitions on the roadmap point — and the
inspection path should move to the log or to an agentd-served query surface
when the need returns.

## The recall tools

Two auto-allowed (read-only) tools, shaped like the Letta filesystem
finding — grep-familiar primitives, no vector store, no SQL exposure (both
remain non-goals per `docs/agent-duckdb-state-design.md`):

- **`recall.search`** — case-insensitive substring search over committed
  message text (`agent_messages`, `NOT is_delta`), tool-call arguments
  (`agent_tool_calls.input_json`), and tool results
  (`agent_tool_results.output_json`). Input: `query`, optional
  `scope: "session" | "all"` (default `"session"` — the calling session),
  optional `limit` (default 20). Output rows carry session id, sequence,
  kind, role/tool id, a bounded snippet around the first match, and the
  event's wall-clock time; the total match count is always reported
  (`fs.grep`'s cap-and-report discipline). Matching is a parameterized SQL
  `LIKE` with escaped input — the model never writes SQL.
- **`recall.read`** — the "open" to search's "grep": reads committed
  messages and tool calls/results for one session in sequence order,
  `from_sequence`/`limit`-windowed and character-capped, so the agent can
  pull the full context around a hit instead of relying on snippets.

Sessions gain the context this needs: `ToolSessionState` now carries the
session's own id and the projection path (threaded at agentd session
spawn), which is also the seat any future history-aware tool would use.
Reads open their own `Store` per call — measured safe alongside the live
writer in the same process — and drop it immediately.

Streaming deltas are deliberately not searchable: committed rows carry the
same final text without the near-duplicate noise. Reasoning text is only
persisted as deltas today, so recall covers what was *said and done* (user/
assistant messages, tool traffic), not chain-of-thought — a fine default
for "what did we discuss/do", revisitable if a committed-reasoning
projection ever exists.

Role interaction: recall joins the general catalog like every read tool;
the `config` role's narrow allowlist is unchanged (its envelope
deliberately excludes history access it doesn't need).

## Out of scope (deliberate)

- Vector/semantic search and any archival/"processed knowledge" layer —
  the knowledge-base roadmap item, which this tool's usage evidence should
  inform first (Letta: tool familiarity beats tool sophistication).
- Cross-session recall UI; this is agent-facing only.
- An agentd-served query surface for external inspection (revisit when
  agent-inspect's SQL recipes are genuinely missed).
