# Async `task` — background delegation with push delivery

Status: designed 2026-07-28 (owner-approved shape); not yet implemented.
Prereqs landed: `task` rename + measured routing clauses + cap-summary
(`docs/agent-explore-design.md` 2026-07-27 addenda), argument
double-encoding repair.

## Why

Measured, not speculative (`docs/research/agent-ceiling-death-autopsy-
2026-07-26.md` incl. 追補 4; `agent-delegation-and-batching-probes-
2026-07-27.md`):

- After the adoption fixes, the reference trajectory finally appeared —
  delegate first, batched reads, self-implementation with interleaved
  verification — and the sole remaining death is the raw context
  ceiling. The next lever is putting less work *into* the requester's
  window, not surviving past it.
- Synchronous spawn-and-wait prices every delegation at one fully
  blocked requester turn, which structurally biases toward one big
  monolithic brief per session (observed: exactly one `task` call per
  run, ~27k-char reports). Small-granularity delegation — several
  scoped questions in flight while the requester keeps working — needs
  the requester to *not* wait.
- The models' trained distribution is the deciding constraint (owner
  direction after the join-tool draft was rejected): mainstream
  harnesses run subagents in the background, tell the model "you will
  be notified when it completes; keep working", let children cross turn
  boundaries, and wake the agent when results land. A join-first,
  turn-scoped design is off-distribution and would predictably not be
  driven well.

## Decisions (owner, 2026-07-27/28)

1. **Launch is non-blocking.** `task` returns immediately:
   `{session_id, description, status: "started"}`. The description
   already labels the transcript row; it becomes the notification label
   too.
2. **Delivery is push, not pull.**
   - Requester mid-turn: when a child completes, the completion is
     injected as a notification message into the requester's next
     provider round — report head inline, full text via `task_output`
     when the report exceeds the inline budget. Multiple completions
     landing between rounds coalesce into one notification block.
   - Requester turn already ended: the completion **starts a new turn
     automatically** (system-initiated; kin of `continue-turn`), with
     the notification as that turn's input. A task launched late in a
     turn is therefore never wasted.
   - Notification shape must be template-safe for the production
     models (M3's chat template is the strictest observed — mapping
     arguments, no string branch; the exact message role/format is an
     implementation decision to be verified against it).
3. **`task_output(session_id)` fetch tool** for full reports and
   re-reads. This *is* mainstream (Claude Code `TaskOutput`, Hermes/
   MiniMax `bash_output` lineage) and complements push delivery; it is
   not the primary channel.
4. **Children are session-scoped, not turn-scoped.** They die on
   requester-session termination or an explicit cancel; they survive
   `cancel-turn` (interrupting the requester must not vaporize
   in-flight investigation — mainstream behavior). Restart cleanup
   keeps using the role identity, unchanged.
5. **Concurrency cap: 3** children in flight per requester. Launches
   beyond the cap fail fast with a clear error naming the running
   children (id + description). Per-child iteration cap 25 and
   `summarize_on_cap` behavior unchanged.
6. **The plumbing is the subscription abstraction.** Everything above
   is written against "subscribe to another session's blocking/stop
   events" (owner direction recorded 2026-07-26 in
   `docs/agent-explore-design.md`): completion/stop events are the v1
   consumers (notification injection, auto-turn wake, `task_output`
   readiness). Approval-forwarding for future write-capable children is
   the same subscription with one more event kind — no new seam later.
7. **v1 children stay read-only**, so approvals are structurally
   impossible inside a child (not suppressed — impossible: the role
   allowlist contains no tool that can cross a boundary). Write-capable
   task roles are explicitly out of scope here; their open questions
   (worktree/branch policy for child edits, approval forwarding,
   reconciling half-done work after a cap) are recorded for the next
   design round.

## Turn-semantics notes (implementation constraints)

- The sync waiter/fold machinery does not survive as a blocking join;
  it becomes the completion-event subscription that drives injection
  and wake. There is no blocking path left in the tool.
- `Event::TurnEnded` remains the only turn boundary; a
  notification-started turn is a normal turn that happens to have a
  synthetic input. External monitors keep trusting `turn_ended` only.
- If the requester is `WaitingForApproval` when a child completes, the
  notification waits; approval resolution proceeds first (no new
  interleaving with the approval state machine).
- Requester cancellation mid-injection must not lose the report:
  undelivered completions stay queued and deliver on the next turn,
  whoever starts it.

## Measurement plan (same axes as the series)

Rerun T-callid after implementation: task launches per session,
parallel launches per response (probe-verified capability), requester
read volume and calls/req, whether the requester survives to the gate
phase. Before/after baseline is 追補 4's run (f89f0780).
