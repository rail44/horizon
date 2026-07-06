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

## Premises (measured 2026-07-06/07, DuckDB 1.5 / duckdb-rs "1" bundled)

- **Write volume is a non-issue**: live agent traffic peaks at ~1-2 events/s
  (streaming deltas included; ~35% of events are deltas).
- **Same-process opens are NOT shared — and that's the load-bearing fact.**
  A first measurement ("two `Store::open`s of one path both succeed, so
  they must share libduckdb's instance cache") was misread: duckdb-rs
  calls `duckdb_open_ext` directly and uses no instance cache; the double
  open only "succeeds" because POSIX file locks are per-process. The
  result is two *independent* database instances unsafely sharing one
  file. Combined with DuckDB's relaxed durability (committed appends sit
  in the instance's in-memory WAL until a checkpoint threshold — nothing
  on disk), a second instance reads the stale on-disk state: observed
  live as the writer's own connection counting 18 events while a fresh
  same-path open in the same process counted 0. **Every same-process
  consumer must therefore go through the one shared `Store` instance**
  (`Arc<Mutex<..>>`); per-call opens are forbidden, including the
  session-resume history replay.
- **External read-only access is diagnostic at best.** Measured both ways
  across holder kinds: a `duckdb` CLI holding the file read-write blocks
  other processes outright, while a crate-held instance let an external
  `duckdb -readonly` open succeed — but that reader sees only the last
  checkpointed state, silently missing everything in the live writer's
  memory. Either way the safe assumption is the same: external reads may
  fail or silently lag; only the JSONL (or a stopped agentd) is
  authoritative from outside.

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

**Accepted consequence — external readers change.** While agentd runs,
external access to the `.duckdb` file is no longer authoritative: an
external open may be refused outright or may succeed and silently show
only the last checkpointed state (see the premises above — both were
observed). `agent-inspect`'s direct `duckdb -readonly` recipes therefore
only give authoritative answers when agentd is stopped; the skill is
updated to say so and to lead with the JSONL (which external tooling can
always read). This was weighed against the alternative (transient open +
watermark refresh per recall query, which preserves external access but
leaves the file cold between calls) and decided deliberately: the
projection is becoming a live read model — the direction the
knowledge-base ambitions on the roadmap point — and the inspection path
should move to the log or to an agentd-served query surface when the need
returns.

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
session's own id and a handle to the process's one shared `Store`
(threaded at agentd session spawn), which is also the seat any future
history-aware tool would use. All reads — the recall tools and the
session-resume history replay alike — go through that shared instance;
per-call `Store::open`s are forbidden (see the premises: an independent
instance silently reads a stale file).

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
