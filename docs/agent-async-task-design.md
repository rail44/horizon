# Async `task` — background delegation with push delivery

Status: designed 2026-07-28 (owner-approved shape); **implemented
2026-07-28**. Prereqs landed: `task` rename + measured routing clauses +
cap-summary (`docs/agent-explore-design.md` 2026-07-27 addenda), argument
double-encoding repair.

Implementation record (what shipped, where):

- `crates/horizon-agent/src/tools/explore.rs` (+ `explore/children.rs`,
  `explore/notify.rs`) — non-blocking `start`, `task_output`, the
  per-requester child registry with the cap, the completion queue, and the
  wake channel.
- `crates/horizon-agentd/src/session/subscription.rs` — the named
  "subscribe to another session's stop/blocking events" seam (decision 6),
  generalizing the old ad-hoc event tap.
- `crates/horizon-agent/src/providers/rig/session.rs` — the drain before
  each provider round (`inject_task_notification`) and the auto-turn wake
  arm (`Next::TaskWake`).
- `contract::MessageRole::TaskNotification` marks a delivered notification
  in the event log so it never masquerades as a human user message; it is
  the change that required `SESSION_PROTOCOL_VERSION` 13 → 14 (a variant
  placed before the `#[serde(other)] Unknown` catch-all reorders indices,
  which the schema checker classifies as a reshape).

The **inline report budget** is
`tools::explore::notify::INLINE_REPORT_CAP_CHARS` = 4,000 *characters* (not
bytes — a report is arbitrary UTF-8). Over it, the head is inlined and the
cut names the exact fetch: `call task_output with session_id "<uuid>" for
the full report`.

Deviations from the shape below: none. The **notification wire shape** was
left as an implementation decision by decision 2 and resolved as a
**user-role message carrying plain text**, emitted as one coalesced block
per drain. The argument is template safety: MiniMax-M3's chat template
iterates a tool call's `arguments` as a mapping with no string branch at
all (the failure that killed session `12fd8d14` and produced
`replay_safe_tool_arguments`), so anything tool-call-shaped carries a
standing risk that plain user text does not. Persistence and the transcript
distinguish it by role; the provider never sees the distinction, and a
resumed session replays it as the same user message that was originally
sent.

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
   - *Added 2026-07-28.* An empty report is never delivered as a
     report. When a child dies or is capped and its "partial report"
     body is empty once stray reasoning-close artifacts are discounted
     (`</think>`/`</mm:think>`/`</thinking>` — the serving-layer leak
     measured that day), the notification says it produced no usable
     report, names the cause, and tells the requester to relaunch a
     narrower task instead of re-reading this one. The stored report
     keeps the body verbatim for `task_output` and forensics: this is
     an emptiness *test*, not a rewrite — defensively stripping the
     artifact from live output remains an open decision (candidate 6).
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

   *Amended 2026-07-28.* The budget is **concurrent provider streams,
   counting the requester's own turn**, not children: 3 children plus a
   requester mid-turn is four streams against one endpoint, and that is
   what produced the observed `429 Too many concurrent requests` which
   killed a child 397s in
   (`docs/research/agent-harness-findings-97-2026-07-28.md`, candidate
   5). So `MAX_CONCURRENT_PROVIDER_STREAMS = 3` and the launch ceiling
   is that minus one, i.e. 2 children. The refusal now also says the
   limit is provider concurrency rather than a policy quota, keeping the
   running-children list. Held static: a provider-side signal
   (`Retry-After`, or a 429 body naming the real limit) would be the
   input for tuning it dynamically, and that is not built.
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

**First validation run (session 5dd49a85, 2026-07-28, M3).** The async
loop worked end-to-end in production: launch receipt → requester turn
ended → completion notification (4,405 chars: 4k head plus a
`task_output` pointer) started an auto-turn → `task_output` fetched the
full report → batched reads followed. The run nonetheless launched
exactly ONE task (plus one `task_output`; no follow-up launches during
implementation), so consumption matched the synchronous era's shape: 110
requests, death at input 229,212 (the max_tokens-adjusted ceiling),
calls/req 1.37, first edit at request 18, 28 files +348/−31 with the
final `cargo check` clean. Conclusion: the mechanism is proven and the
granularity is unchanged — hence the 2026-07-28 wording amendment to
`prompt::DELEGATION_ROUTING_SECTION` and the matching `task` description
(decompose into parallel launches; keep delegating follow-up questions
while implementing), to be measured on the next dogfood run. The next
structural lever is compaction (design in progress).
