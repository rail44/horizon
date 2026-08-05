# logd — Log Infrastructure Daemon — Design

Status: decisions settled 2026-08-06 (owner consultation in the project
session). Not implemented; no roadmap slot claimed yet. Evidence base:
`docs/research/change-notification.md` and
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

## Open items (v1 slicing not yet decided)

- Migration order: extract the event-log writer from
  `crates/horizon-agent` (agentd becomes a sender) vs. start with the
  board logs (smaller, newer, fewer consumers) — undecided.
- Whether recall's query path moves to logd in v1 or keeps its
  in-agentd store until the projection migrates.
- Subscription protocol details (per-log streams vs one multiplexed
  stream; cursor persistence convention for consumers).
- Crash-window semantics of decision 2 (what agentd's send blocks on,
  and for how long, before surfacing the error).
- The distro libduckdb 1.5.0 pin (`AGENTS.md` Build setup) is
  orthogonal but will constrain any future extension adoption.
