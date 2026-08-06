# logd — Log Infrastructure Daemon — Design

Status: decisions settled 2026-08-06 (owner consultation in the project
session). **Stage A (ingest) and stage B (subscribe) implemented.**
Evidence base: `docs/research/change-notification.md` and
`docs/research/duckdb-ecosystem.md` (both surveys predate and informed
these decisions; several of their framing assumptions were dissolved
DURING the consultation and the surveys record that honestly).

## Problem

Three roles are scattered today and one is missing entirely:

- **Log collection**: `agent-events.jsonl` is written by agentd itself
  (the event-log writer lives inside `crates/horizon-agent`); the board
  logs are written by whichever short-lived CLI or GUI process holds a
  flock. There is no single collector.
- **Projection ownership**: the DuckDB projection is maintained inside
  agentd, which couples log infrastructure to the agent implementation
  and (via DuckDB's file lock) makes the projection unreadable by any
  other process while agentd runs.
- **Notification**: nothing. A session waiting on approval in an
  unfocused tab is invisible (the 2026-08-04 incident); the integrator
  hand-rolls `tail -F` monitors that have silently died before; board
  comments are discovered only by re-reading. The wake problem blocks
  the agent-constellation work (a shaper agent has no way to learn a
  comment arrived).

## Shape

```
agentd ──(send events)──┐
board CLI / GUI ────────┼──▶ logd: sole appender + projection owner
future domains ─────────┘         + query servant + poke source
                                   │
                                   ├─ *.jsonl        (authoritative logs)
                                   ├─ DuckDB         (internal projection)
                                   └─ logd socket    (ingest / query / subscribe)
```

A third daemon beside agentd and terminald, spawn-on-demand by ANY
client (GUI or CLI — the same discipline the shell already uses for the
other two), with its own socket and its own lockstep protocol version
pair per the terminald-split precedent.

## Decisions

1. **The collector is the notifier, and it is a process.** The owner's
   framing: log infrastructure unrelated to the UI, and peeled off
   agentd. Writes flow only through logd; notification is the writer's
   role, so subscription streams from logd are the primary poke channel.
   The first survey's central axis — writer-independent notification
   (inotify) — was solving a problem this decision removes; inotify
   survives only as the low-level fallback and as `tail -n +N -F` for
   external consumers until they speak the socket.
2. **agentd stays ignorant** (owner decision): it sends events and
   carries no ack tracking, no resend, no delivery logic. A failed
   socket send surfaces exactly like a failed disk append does today —
   the same error class, no special handling. Delivery guarantees are a
   LIFECYCLE property (logd spawned before agentd needs it, supervised,
   respawned; blocking unix-socket sends with OS buffering), not a
   protocol property.
3. **Pokes are lossy; correctness lives in cursors over the log.**
   Subscription streams carry sequence numbers only — never payloads —
   so a consumer that misses the stream entirely and catches up from
   its durable cursor produces byte-identical results. (The 2026-08-04
   incident where a dead monitor silently lost an approval is the
   motivating failure: the notification must never be the authority.)
4. **The projection belongs to logd, and file sharing is a non-goal.**
   The "multiple local processes open the DuckDB file" requirement was
   examined and found to not exist: today's only projection reader is
   agentd's own recall tool. Consumers query logd over its socket with
   NAMED queries (not arbitrary SQL — agents are among the consumers,
   and the query surface is the security surface). DuckLake and Quack,
   both real answers to the sharing problem, are recorded as
   non-adopted in `docs/research/duckdb-ecosystem.md` with a revisit
   trigger (Quack 2.0 stable + a real authorization callback, if
   ad-hoc external SQL is ever wanted).
5. **DuckDB stays, re-justified on its original grounds.** The adoption
   rationale was single-machine writes plus flexible, fast log search —
   an OLAP-shaped engine. Re-evaluated 2026-08-06 at the owner's
   prompting: the workload really is analytical (recall's substring
   scans, usage aggregations over sessions/turns/tool-calls; a
   rebuildable ~350MB columnar projection), and the one practical
   pressure away from DuckDB — its cross-process file lock — dissolves
   under decision 4. The honest alternative (SQLite WAL: multi-process
   readers plus `PRAGMA data_version`, at the cost of row-store size
   and 10-100x slower analytical scans) buys properties this design no
   longer needs. Exit clause: the projection is rebuildable from JSONL
   by construction, so the engine remains swappable at any time — that
   cheapness is itself a design asset to preserve.
6. **In-process read concurrency is logd's private concern.** The
   current `SharedDuckdbStore` serializes behind one Mutex-held
   connection; v1 keeps that. If query contention shows, widen to
   multiple read connections on the same instance (`try_clone`) without
   touching the socket contract.
7. **The human channel is separate machinery.** Desktop notifications
   go through `org.freedesktop.Notifications` (`notify-rust`), using
   `replaces_id` to update in place and `ActionInvoked` to focus the
   relevant pane. Nothing machine-side may depend on a human
   notification having been delivered — it is lossy in a way that does
   not compose with cursors.
8. **Wake policy is not logd's job.** logd notifies subscribers; which
   agent to spawn or resume on which event (the shaper problem) is a
   subscriber's policy, kept out of the log infrastructure. "判断は
   エージェント、起床は機構" — logd is the 機構's lower half only.

## v1 slicing (owner decision 2026-08-06)

Start with the **board logs**: smaller, newer, fewer consumers, and it
defers the agentd extraction surgery. v1 scope:

- logd owns board WRITES (the horizon-board library's write path
  becomes a socket client with connect-or-spawn; the direct flock
  append moves inside logd — replaced, not kept as a fallback).
- Board READS stay file-folds in the library: JSONL stays
  world-readable, a single writer plus atomic appends make direct
  reads safe, and boards have no projection. The named-query surface
  becomes relevant only when agent-events/projection migrate.
- Subscriptions (sequence-number pokes) ship as v1's second half,
  after the daemon skeleton lands.
- agent-events migration and recall's query path are explicitly
  later phases.

## Subscription shape (owner-settled 2026-08-06)

- **One multiplexed NDJSON stream**, not per-log streams: a consumer
  holds one connection and reads `{"log":"board","seq":1234}` lines.
  Per-log streams would scale connections with the number of boards.
- **NDJSON rather than an agent-protocol standard.** A2A's events are
  task-scoped with terminal states and its push mode makes every
  consumer host an authenticated webhook; ACP's updates are
  session/turn-scoped with no topic or cursor; MCP's resource
  subscriptions are the closest fit in *shape* (identifier-only poke,
  client re-reads) but require a JSON-RPC client, carry no sequence
  field, and churn (`resources/subscribe` existed in 2025-11-25 and is
  gone in 2026-07-28). NDJSON over the socket is the only form a shell
  script can consume with no SDK, and MCP's own current spec says a
  custom Unix-socket transport SHOULD reuse newline-delimited framing —
  so an MCP facade later is a message-layer addition, not a transport
  migration. See `docs/research/change-notification.md`.
- **Cursor on connect, no server-side cursor state.** A subscriber may
  send its last-seen seq when it connects; logd replies with the
  current seq before streaming. Borrowed from SSE's `Last-Event-ID`
  without adopting HTTP. logd never persists consumer positions.
- **`horizon board watch`** exposes the stream as lines for shells and
  external processes; the world-readable JSONL plus `tail -n +N -F`
  stays supported indefinitely and must not be degraded by the socket
  path existing.

### Transport multiplexing (implemented stage B)

The subscribe NDJSON stream and the remoc chmux ingest path coexist on one
socket via **first-byte sniffing**: logd's accept loop (logd-local, not the
shared `daemon::run` — which is serial and would block all ingests on a
long-lived subscriber) reads one byte from each accepted connection with
`BufReader::fill_buf` (peek without consuming). A `{` byte (0x7B, the first
character of a JSON subscribe request) routes to the raw NDJSON subscribe
handler; anything else (chmux's binary first byte) routes to the remoc
handshake. Both paths share the same `UnixStream` halves — the peeked byte
stays in the `BufReader`'s buffer for the handler that follows.

The **seq** is the 1-based line number in the JSONL file (counted by the
tolerant reader's `line_count`), assigned by the writer under the exclusive
flock. A consumer that misses pokes catches up via `tail -n +<seq+1> -F`,
which sees byte-identical results (decision 3). logd does not persist
consumer positions; the registry is process-wide and in-memory.

## Open items

- Whether recall's query path moves to logd when the projection
  migrates, or earlier.
- Crash-window semantics of decision 2 (what agentd's send blocks on,
  and for how long, before surfacing the error).
- The distro libduckdb 1.5.0 pin (`AGENTS.md` Build setup) is
  orthogonal but will constrain any future extension adoption.
