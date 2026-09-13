# Milestone planning and execution

The owner supplies a feature or outcome. Horizon investigates the repository,
decomposes and prioritizes work, presents unresolved decisions concisely, and
runs implementation tasks. Decisions must be understandable before the owner
can decide whether to retain or delegate them. Reading a long comment thread
is not a prerequisite for answering.

The first implementation supports serial execution in the agent daemon's
current project. It uses native board views, durable board operations, two agent
roles and a deterministic coordinator. This is product behavior, not a
repository development-flow policy.

## Using the flow

Run Horizon from the project whose board is being operated. Add a board item
describing the desired outcome, open it, and select **Plan and run as milestone**.
This authorizes planning and eligible implementation in an isolated worktree.
Legacy items are not automatically converted or executed.

The planner reads the goal, comments, repository and prior decisions. It saves
acceptance criteria, ordered tasks with dependencies, and unresolved decisions.
The board defaults to milestones when any exist; **Show all items** retains
access to the ordinary backlog. Details show the current plan, the next
unresolved decision, task progress and reported verification. The discussion
stays collapsed until **Show discussion** is selected. Workflow progress does
not post comments or wake the keeper.

Each decision has a question, factual context, recommendation and consequence.
The owner answers in free text. After the outstanding decisions are answered,
the planner incorporates the answers into a revised plan. A plan without
outstanding decisions proceeds automatically. Routine implementation choices
belong to the planner; there is no blanket plan-approval question.

Tasks run one at a time, in priority order among those whose dependencies have
finished. Their implementation session and isolated worktree persist across
tasks, including uncommitted changes. The implementation reports a summary and
the verification it ran. The coordinator incorporates this result after the
turn ends. When every task has reported success, the item enters `review`,
not `done`.

**Open session** attaches the active daemon-created session even if the shell
did not previously know its id. Tool approval stays in the agent approval UI;
the board displays that the session needs attention. **Pause** cancels the
current turn and prevents another launch. **Retry / resume** continues from
the preserved worktree. **Revise plan** re-investigates the goal, comments,
owner answers, completed work and last failure. Completed task definitions
must be retained; corrective work gets new keys.

The CLI exposes the same state and owner operations:

```sh
horizon board add "Desired outcome" --body "Context and constraints"
horizon board milestone 48
horizon board flow 48
horizon board answer 48 scope "The first version only needs local use."
horizon board pause 48
horizon board replan 48
horizon board resume 48
```

Use the id returned by `add`; 48 is illustrative. `--json` returns structured
state. CLI writes can queue work while the agent daemon is down; execution
needs Horizon/agentd running from that project. Changing a milestone's title
or body atomically invalidates its plan revision and requests replanning.
An active attempt must stop before its goal can change.

## Persistence and ownership

`horizon-board::workflow` owns the plan, decisions, answers, task results,
attempt reservation, last outcome and workspace reference. Task keys are local
to a milestone. These are structured plan tasks, not reparented legacy items;
no existing board hierarchy is assumed.

`Store::workflow` sends a typed mutation and expected revision to logd. Under
the writer's existing lock, logd validates both and appends one
`workflow-changed` snapshot event. Concurrent reservations and stale answers
cannot both succeed. The fold derives progress status; owner-applied closed
statuses remain closed. Legacy `claim` excludes milestones.

The planner can read and call `board.report`, but cannot run shell commands or
edit files. The implementer uses existing contained filesystem/shell tools
and `board.report`. The keeper's comment-only authority is unchanged. The tool
executor binds the reporting session identity; the writer checks it against
the active attempt token and report type. A report remains provisional until
the coordinator observes the assignment's user message followed by the session
returning to idle. Initial idle and a tool result alone cannot complete work.

The daemon is the composition root. `horizon-agent` does not depend on
`horizon-board`; their boundary remains the `BoardHost` JSON seam. Typed wire
enums use external serde tags for Postbag; model reports use readable `kind`
tags decoded at the daemon boundary. The log protocol is **3**, in lockstep.
Rebuild the workspace and restart Horizon, `horizon-agentd` and `horizon-logd`
when installing. The terminal daemon's protocol is unchanged.

## Execution and interruption

The coordinator holds a per-board file lease and a single active monitor. It
reserves an attempt durably before starting a provider or creating a worktree.
Board rank orders milestones; the plan orders their tasks. Polling durable
state every 500 ms avoids depending on every subscription notification. The
native view continues to reload through logd's subscription stream.

Worktree creation completes before implementation starts. Failure never falls
back to modifying the source checkout. Existing sandbox, repository trust and
approval machinery apply. Ownership is checked before reusing the worker.

A session that stops without a report, exits or reports a blocker leaves a
visible problem and last-attempt context. Failed work is not retried in a loop.
On daemon restart the coordinator waits for session recovery, then marks
unfinished attempts interrupted. Explicit retry can inspect partial work;
restart never silently replays an assignment. A saved decision without an
active attempt survives restart and can be answered normally.

## Validation and remaining scope

State-machine and writer tests cover decisions and replanning, dependency
ordering/cycles, immutable completed tasks, stale/foreign reports, pause and
interruption, concurrent reservations, goal edits, legacy reads/claims, and
actual Postbag request/reply round-trips. The monitor test distinguishes initial
idle, approval wait and actual assignment completion.

`python3 scripts/check-board-milestone.py` runs the built agent and log daemons
against a temporary Git repository and a deterministic localhost provider.
It exercises goal registration, a decision, daemon restart, a free-text answer,
a revised plan, two real shell tasks in dependency order, a shared isolated
worktree and the review projection. The source checkout must stay untouched.
It requires local sockets and process spawning, so it runs outside session
containment. It never calls an external API.

Real-model planning quality and visual interaction with the native GUI still
require dogfooding. Reported checks are session evidence, not independent
verification or owner acceptance. The coordinator does not merge, push,
publish, deploy or mark the milestone accepted.

Remaining work includes routing boards from arbitrary projects to a global
daemon, concurrent execution, shared tasks between milestones, and replacing
an explicitly terminated implementation session/worktree. Normal daemon
restart preserves and resumes existing sessions. Plan revision currently
follows owner answers or an explicit replan request; task completion updates
results and eligibility without starting another planning turn.
