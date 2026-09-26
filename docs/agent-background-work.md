# Background work ownership

The session runtime owns a work group in `horizon-agent::tools::background`.
A worker registers before acquiring resources or entering a queue, and holds
its registration through result delivery and actual resource retirement.
Registration destruction handles early return, future cancellation and unwind.
Environment activation replaces tool state while retaining the same group.

## Lifetimes

| Lifetime | Work | Stops when |
| --- | --- | --- |
| Call | bash, Web, approval judgment | That exact call occurrence is cancelled, replaced, or its session closes |
| Pass | Mixture-of-Agents proposers | The pass completes, is cancelled, unwinds, or its waiting future is dropped |
| Session | `task` children | The parent session closes; cancelling a turn leaves them running |

The kind of work separately controls environment-switch accounting. Only bash
and Web block environment changes, as before. Children and judgments remain
outside that policy. Bash keeps its per-session FIFO and process-tree kill
implementation; Web and judgments use cancellation tokens to drop futures.
Cancelling an HTTP future stops Horizon's request handling; it does not promise
that a remote server has stopped computing or will reverse a charge.

## Stop, finish, and retire

Each registration transitions once from active to finished or cancelled. The
winner invokes the resource-specific stop action once, outside the registry
lock. An action attached after cancellation runs immediately, covering resources
created during teardown. Replacements stop the old registration; its eventual
destruction cannot remove the replacement. Cancellation and Web's accumulated
domain annotations use both call and occurrence IDs. Late provider completions
therefore cannot stop a new occurrence that reused a call ID.

Winning completion authorizes delivery, but the registration remains counted
until delivery and cleanup have ended. Existing daemon occurrence checks still
reject stale results that completed before cancellation reached the worker.
Judge cancellation produces neither a verdict nor a result. A judge transport
panic escalates to human approval. Already-saved decisions remain history.

Session shutdown withdraws the child-launch capability before closing the group.
Acquiring a MoA host also acquires a pass registration under that capability's
lock: teardown between acquisition and launch cannot recreate a live group.
Child watchers own termination separately from retained reports. Completed
reports remain readable through `task_output`; cancelled watchers cannot recreate
reports after parent teardown. A child is terminated once, and its watcher keeps
its registration until the host confirms actual session exit, including child
background work. Hosts with asynchronous termination implement `wait_stopped`.

The daemon requests cancellation, then waits up to five seconds for all registered
work to retire before considering deletion of an isolated worktree. On timeout
it retains the directory and reports that decision. This is a bounded cleanup
wait, not a claim that the remaining worker has stopped; child watchers can
continue waiting after the parent entry has retired. The wait is outside the
daemon lifecycle lock so child teardown can make progress. Close/detach semantics,
wire and event-log formats, and permission policy are unchanged.

## Adding work

Choose its lifetime and kind, register before launch, install any resource-specific
stop action, and keep the registration in the worker. Async workers select on its
cancellation token; synchronous resources supply a stop action. Finish before
publishing a result, and retire only after actual work ends. Stop actions should
request cancellation promptly; potentially long exit waits belong to the worker.
No additional cancellation registry or work counter is needed.

Regression coverage exercises exact-occurrence cancellation, replacement,
stop-before-resource-attachment, completion/cancellation races, panic cleanup,
blocked child exit, runtime-state replacement, and pass-future destruction.
