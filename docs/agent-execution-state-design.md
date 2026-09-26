# Agent execution state

The session coordinator owns command ordering and provider futures. Retained
work and input admission have separate owners; configuration, environment,
compaction, memory, and MoA proposals remain independent state (2026-09-25).
Board behavior and input reply destinations are unchanged.

## Input admission and retained work

`providers/rig/session/input.rs::Inputs` owns deduplication, the queue, admission
pause, and the active input batch. An active batch always has a first request:
its destination and receipt ID cannot be changed by passive additions.
`start_next` enforces admission itself. Cancel pauses admission even with an
empty queue, recording `InputQueuePaused` whenever admission changes. Otherwise
an input arriving after that cancellation could execute after a restart.

`session/progress.rs::Execution` owns mutually exclusive retained work:

| State | Retained data | Transition |
| --- | --- | --- |
| Idle | None | A provider round installs its requested tool batch. |
| Tools | Outstanding call descriptors | Each accepted result removes one call; the last returns to Idle. Cancellation drains the batch and awaits host settlement. |
| Halted | An executed result and its tool name | Continue consumes it as the next prompt; fresh input consumes it into history. |

A provider future is owned by the coordinator's call stack, so another provider
round cannot run concurrently. Cancellation of that future uses the same input
admission operation as cancellation between rounds. Completed, failed,
cancelled, and guard-halted turns share `end_interaction`: release per-turn MoA
proposals, settle the input receipt, then publish the turn boundary and state.

Attempt identity is checked by the daemon. The provider's batch remains keyed
by provider call ID: declining a retry legitimately returns the prior attempt's
result. Superseded results never advance a batch.

A stopped response retains its issued calls, including after a stream error.
Before cancellation, failure, guard halt, or truncation recovery can close the
turn or send another request, the provider asks the host to settle the batch.
The host returns recorded results unchanged and cancels only unfinished
occurrences, following approved retries and retaining a denial result held in
an unapproved retry. The provider consumes this receipt into history exactly
once. See [response lifecycle](agent-response-lifecycle.md).

## Tool decisions and completion

`ToolCompletion::live_request` accepts the exact current, unfinished occurrence
before dispatch. Judge completions also require an unresolved decision and no
already-visible human prompt. Human and automatic execution share the unresolved
approval predicate. Retry handlers receive this validated request, rather than
looking up a bare call ID again.

`tools/transition.rs::ToolUpdate` applies start/finish/retry events through
`LiveState`. Asynchronous starts are applied before enqueueing the worker;
synchronous approval execution applies its start/result together. Finished
updates preserve a sibling's approval wait, or report Running while the provider
owes its next round. Only the provider ends the turn. `publish_tool_update`
forwards the applied events before sending a terminal result to the provider.
Approval helpers no longer return a redundant whole-frame snapshot.

`LiveState` now acknowledges persistent batches before folding them.
`ToolUpdate` constructors return `Result`: a successful update may be published
and its result may release the provider; a failed start must not launch a worker.
This is a flush acknowledgement, not an atomic batch or an `fsync` guarantee.
Human/judge authority and each sandbox grant's scope remain separate from
lifecycle handling. See [the persistence contract](agent-persistence-contract.md).

## Recovery contract

The daemon durably settles inputs that actually started before a crash and
cancels unfinished tool attempts before spawning the replacement provider.
Repeated recovery must not produce another receipt for the same active input.
The provider reconstructs accepted IDs, unresolved queued inputs, and admission
pause in `Inputs::restore`. Completed inputs never return to the queue. Queued
inputs retain the previous admission policy: ready queues may proceed; paused
queues wait for explicit owner input that resumes work.

A halted continuation is in-memory execution state; replay already reconstructs
its result in provider history. A new `Execution` therefore starts Idle. A stale
Continue with no retained continuation must leave admission unchanged, including
when a paused queue was restored. Environment handoff, task notifications, and
reply routing keep their existing semantics.

The response-lifecycle follow-up uses agent wire v26; event-log v3 is unchanged. The earlier
v24/v3 cutover still follows [history activation](agent-history-format.md).
