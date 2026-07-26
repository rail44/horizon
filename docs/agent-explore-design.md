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

---

## Addendum (2026-07-26): follow-up turns, fork seeding, and the measurement plan

The first measured runs (four runs of the `WaitingForUser` brief; see the
roadmap's context-consumption entry) established: routing fixed adoption
at the task's entry point only; the one delegation's report was accurate
for the question asked but the question omitted the requester's own
measured evidence, so the requester — correctly — distrusted the
conclusion and re-explored by hand. Prompt guidance ("include your
observations") is a silent-failure control: non-compliance is invisible
until a report collides with data downstream. Two structural mechanisms
are added instead, to be **measured against each other**, not decided by
argument. Their shared premise: what a delegate inherits occupies its
context window; what it does not inherit must be told.

### B. Follow-up turns

`agent.explore` gains an optional `session_id` input. When present, the
prompt is sent as a further user message to that still-alive exploration
session and the call waits for that session's next turn to end; when
absent, behavior is unchanged (fresh spawn). Rationale, from run 4: the
requester demonstrably *noticed* the report/evidence contradiction in its
reasoning; what it lacked was a cheap repair action, so it fell back to
re-reading everything itself. A follow-up lands the contradiction — which
itself carries exactly the missing evidence — on a session that already
holds the relevant files in its history.

- **Lifetime**: an exploration session now survives its first completed
  turn and is terminated when the requester's own turn ends (completed,
  failed, cancelled, or the requester session going away). One scope, no
  idle orphans. Daemon-restart cleanup is unchanged: every exploration
  session is terminated on resume.
- Each follow-up call installs a fresh event tap; the fold logic is the
  turn-scoped fold already in place.
- Failure of a follow-up (unknown/terminated session id) resolves as an
  ordinary error tool result naming the fresh-spawn alternative.

### C. Fork seeding (owner direction, simplified from history-rollback)

An exploration session may be seeded with **a copy of the requester's
history so far**, so the delegate sees everything the requester sees —
the structural answer to the evidence gap. No in-session rollback, no
replay marks: the fork child is an ordinary exploration session whose
initial `rig_history` is reconstructed from the requester's persisted
events (the same event-to-history mapping session resume already uses).
Discard-on-completion is plain termination; the requester is untouched.

- **Tail sanitization is mandatory**: at delegation time the requester's
  persisted event stream ends mid-turn, with at least one tool call (the
  `agent.explore` call itself) lacking a result. Seeding must close or
  drop unpaired calls, or the provider rejects the history.
- **Ceiling inheritance is the known cost**: the child's exploration room
  is (model window − requester's current size). Fresh spawns always have
  ~190k; a fork forked at 150k has ~46k. This is the tradeoff being
  measured, not a defect.
- **Cache expectation — grounded, and deliberately pessimistic.** Request
  #0 `cached_input_tokens` across eight same-brief sessions in the
  existing log: three hits of exactly 2,432 tokens, five zeros. So
  synthetic.new does share prefix cache across sessions, but unreliably
  (routing-dependent), and only up to the first byte of divergence.
  A same-preamble fork variant (advertise the parent's full toolset,
  enforce read-only at the execution layer, steer via a trailing message)
  would make the shared prefix span the whole seeded history — but given
  the observed unreliability it cannot be justified on cache grounds
  alone and is **deferred**; v1 implements the restricted-advertisement
  child and pays one uncached ingest of the seed.
- Mode is selected by the harness, not the model: an environment-only
  switch (`HORIZON_EXPLORE_SEED=fresh|fork`, default `fresh`), following
  the `HORIZON_AGENT_EVENT_LOG` convention. Model-visible parameters
  would confound the adoption measurement.

### Independent observation worth its own follow-up

The 2,432-token sharing ceiling exists because the system prompt renders
the per-session working directory *before* the large stable sections
(repository instructions, 16.8KB, identical for every session). Ordering
the preamble stable-first / variable-last would let all sessions share
most of the preamble opportunistically. Separate change, separately
measurable; not part of this slice.

### Measurement plan

Same brief (backlog 48's duplicate-`fs.edit` investigation, with the
measured evidence embedded in the brief per the run-4 lesson), one run
per arm to start, judged qualitatively like the four `WaitingForUser`
runs:

| arm | seeding | follow-up available |
|---|---|---|
| fresh | `fresh` | yes |
| fork  | `fork`  | yes |

Metrics per arm: adoption (initial and follow-up), requester context
peak, completion, requester+delegate total and uncached tokens, and
whether the delegate's report reconciles the embedded evidence without a
manual redo.
