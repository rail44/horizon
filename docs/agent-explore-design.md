# Parallel Exploration Sessions — `agent.explore`

Decided 2026-07-25 in an owner consultation, on the measurements in
`docs/research/agent-read-navigation-prior-art-2026-07-25.md` and the two
failed dogfooding runs described below. This document is the scope record
for the implementing worker.

## Problem

A session's history is monotonic: every tool result is retransmitted with
every subsequent provider request, so total input grows as roughly
(rounds × history). Exploration output dominates that history. Two agent
sessions given the same implementation brief (fix the mid-turn
`WaitingForUser` emissions) both died at the model's context ceiling —
final request ~196k input tokens against a 262,144-token window with
65,536 reserved for output — after reading the consumer side of a
3,361-line module. The `fs.grep` locations-only change (`d74a75e`) cut
tool output 28%, but reads compensated (+73%): the ceiling moved from 60
to 82 requests and remained fatal. The edit itself was never the cost:
`fs.edit` accounts for 0–1% of resend weight in every measured session.

The exploration fragments do not need to live in the requesting session's
history at all. What the requester needs is the conclusion ("the emit
sites are X, the consumers are Y"), a few kB. Current-generation harnesses
(OpenCode's task tool is the closest prior art) route open-ended
exploration into a disposable session and return only its final text;
the fragments stay behind and are discarded.

## Decision

1. **New production tool `agent.explore`.** Input: a prompt stating the
   exploration question and the expected deliverable. The tool spawns a
   read-only **parallel session**, waits for its turn to finish, and
   returns the session's final assistant text (plus the spawned session id
   for cost attribution).

2. **A peer, not a child (owner decision).** The exploration session
   references the **same `workspace_root` as the requester** — including
   an isolated requester's worktree — with no worktree isolation of its
   own and **no derivation-tree edge**. The derivation tree remains pure
   code genealogy (`docs/session-relationship-design.md` decision: only
   isolation creates an edge). The exploration session therefore sees the
   requester's uncommitted working-tree state, which is the correct view
   for mid-task exploration; concurrent read-only access is safe.
   Do not use parent/child vocabulary in code or docs for this
   relationship.

3. **First-class session, invisible to the UI, auto-terminated.** It is a
   normal session in the event log and DuckDB projection (its cost is
   measurable exactly like any other session; the requester's
   `ToolCallRequested`/`Finished` events carry the spawned id, which is
   the analytics join key). It is never attached to a pane. On completion
   the tool terminates it.

4. **Read-only toolset, no recursion.** Allowed tools: `fs.read`,
   `fs.grep`, `fs.glob` only. `agent.explore` itself is excluded, so
   recursion is structurally impossible. Every allowed tool is
   `AutoAllowRead`, so approvals are unreachable; if a
   `WaitingForApproval` nevertheless surfaces, the tool call fails
   immediately with an error result — it must never hang waiting for a
   human. The existing built-in role mechanism (`roles::CONFIG_ROLE` is
   precedent) is the likely seam for the restriction; the implementer may
   choose another if it fits better.

5. **The wait is an event subscription.** The requester-side
   implementation subscribes to the exploration session's event stream
   and folds it until `TurnEnded`/`Failed`/`Terminated`, assembling the
   final assistant text from `MessageCommitted`. No bespoke return
   channel. This is deliberate shaping (owner direction): the future
   common abstraction is "subscribe to another agent session's blocking
   events (approvals) and stop events", and this tool must be expressible
   on it without rework. Inter-agent *messaging* stays out of scope under
   its standing agreement (project-level consultation first).

6. **Requester stays responsive; cancellation propagates.** The tool runs
   like `bash` does today (asynchronous completion folded into the turn),
   so `cancel-turn` on the requester terminates the exploration session
   promptly.

7. **Bounded.** Exploration sessions run with a tighter turn cap (25
   rather than the standard cap): they answer one question. On the cap,
   on provider failure, or on the exploration session's own context
   overflow, the tool returns an error result with whatever final text
   exists; the requester recovers by asking a narrower question.

8. **Restart cleanup.** An exploration session is meaningless without its
   waiter. On daemon resume, never-completed exploration sessions are
   terminated rather than re-adopted. If the explore role id alone can
   identify them, no wire change is needed; if a wire field is added it
   must be additive (regenerate the schema artifact —
   `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond
   wire_schema` — the checker gates non-additive changes).

## Catalog description (draft, adjust wording freely)

> Delegate an open-ended, multi-file exploration to a parallel read-only
> session sharing this workspace. Use it when locating or understanding
> code would take several rounds of grep/glob/read whose outputs you do
> not need verbatim afterwards; state the question and the exact
> deliverable (paths, line numbers, facts). For one to three known files,
> read them directly instead. Returns the exploration's final report.

## Tests (hermetic; the deterministic fallback provider is the harness)

- The spawned session advertises exactly the read-only toolset.
- The requester's history after a call contains the call and the final
  text, nothing else from the exploration.
- Cancelling the requester's turn terminates the exploration session.
- A `WaitingForApproval` in the exploration session fails the call.
- Daemon resume terminates a never-completed exploration session.
- Schema artifact stays green (bless if a wire type changed).

## Measurement (after merge; not the worker's job)

Re-run the `WaitingForUser` brief a third time and compare against the
two failures (82 requests / 196k peak / death): requester context peak,
completion, and total requester+exploration consumption. Total tokens are
expected to *rise* (the exploration session pays its own preamble); the
success criteria are requester headroom and completion.

## Out of scope for v1

Parallel fan-out of several explorations, background/asynchronous
explore, recursion, write-capable delegates, a cheaper model override for
exploration sessions, and inter-agent messaging.
