# Milestone planning and execution

**Owner correction, 2026-09-15:** the owner stated that most of the September 13
implementation did not reflect their intention and requested a redesign from
the desired development flow. This document preserves that implementation's
design and validation record; its earlier descriptions of agreement do not
establish owner authorization. The current design foundation and its explicitly
identified proposals are in [Board redesign](board-redesign-design.md).
In particular, automatic integration and the existing UI are not adopted
requirements of the redesign.

The owner supplies a feature or outcome. Horizon investigates the repository,
decomposes and prioritizes work, presents unresolved decisions concisely, and
runs implementation tasks. Decisions must be understandable before the owner
can decide whether to retain or delegate them. Reading a long comment thread
is not a prerequisite for answering.

**Implementation status (2026-09-13, now in main):** implemented on
`board-milestone-flow`. The daemon fixture verifies parallel work,
decision conversations, integration into main, restart and milestone
achievement. Real-model judgment quality and native GUI interaction still
need dogfooding. This document describes Horizon product behavior, not a
repository development-flow policy.

## September 13 design as implemented

### Milestones and board tasks

A milestone expresses a desired feature or outcome and its acceptance
criteria. Its concrete tasks are ordinary board items, whether created by AI,
created by the owner, or drawn from the existing backlog. They are linked to
the milestone and carry priority, dependencies, required decisions and
implementation results. A separate task list embedded in a plan must not
become a second task-management system.

The primary view presents milestones, current progress and decisions requiring
an answer. Task details, implementation choices and full discussion history
are available when opened. The existence of parent/dependency fields in the
model does not establish that the current board uses them: the board audited
for this discussion was flat.

### Decisions and conversation

AI decides implementation methods within the agreed goal, acceptance criteria
and existing policy. A specification choice that cannot be resolved from
those constraints requires the owner's decision. AI's own decisions remain
briefly recorded and inspectable, with links to changes and verification.

Each human-facing decision presents the current question, a recommendation
and reason, and the affected tasks. The owner can discuss that particular
issue with AI in natural language. A response is not automatically a decision:
asking whether searching every session would be slow continues the discussion;
directing the first version to search only the current session settles scope.
An explicit decision or clear agreement to a concrete proposal is sufficient;
there is no redundant confirmation step.

The concise current summary becomes the decision record when settled. Full
conversation remains accessible without requiring the owner to read it to
understand the current issue. Goal or acceptance-criteria changes also use
this decision path.

An unresolved decision blocks only tasks requiring it and their dependent
successors. Other eligible work in the same milestone or another milestone
continues. Milestone progress must be able to show work running alongside
unresolved decisions.

### Planning and priority

AI decomposes work and updates tasks, dependencies and priorities as results
reveal new work or make planned work unnecessary. It records why the plan
changed. Routine replanning within the agreed goal and acceptance criteria
does not require approval of every revision.

Prioritization includes the order of milestones, not just tasks inside one
milestone. AI uses importance and dependencies, respects priorities and
deadlines explicitly supplied by the owner, and records its reasons. The
owner can inspect and correct the order.

Dispatch selects from tasks whose necessary decisions are settled and whose
prerequisites are complete. If a preferred milestone has no eligible work,
work from another milestone can proceed. A decision affecting only part of
the preferred milestone does not prevent its other tasks from proceeding.

### Parallel implementation

Parallel implementation is part of the initial scope. Eligible tasks are
dispatched in priority order into separate worktrees when their planned source
changes and functional impact have no clear conflict. Planning and decision
conversations also continue while implementation runs.

AI must examine the affected source and functionality, not just whether two
tasks mention different filenames. Known conflicts impose ordering on the
affected tasks. Conflicts discovered during implementation are handled between
those tasks while unrelated work continues. Neither a single active task per
project nor a single shared implementation worktree per milestone satisfies
this design.

### Completion and integration into main

A task completes when its acceptance criteria have been verified. A milestone
is achieved when the criteria for the overall outcome have been verified;
successful task reports alone do not establish this. AI verifies conditions
it can check and presents conditions requiring human evaluation to the owner.
Evidence is recorded against each condition. Unverified conditions lead to
verification work or a focused human decision. Satisfied conditions allow
automatic completion without a blanket final approval.

Finished task branches are integrated into **main** automatically after their
changes have been combined with the latest main and passed acceptance checks
and the repository's required quality gate. Integration respects dependencies
and selects higher-priority changes among those ready to merge. An independent
finished branch can merge while a higher-priority task is still being
implemented. Main can therefore receive part of a milestone before the whole
milestone is achieved; a milestone-wide integration branch is not required.

Merges are finalized in sequence while implementation continues in parallel.
Checks from an older main are not sufficient evidence for a newly combined
result. AI resolves merge conflicts and verification failures within settled
specifications, then verifies again. A resolution requiring a specification
choice goes back to the owner; unrelated tasks and merge candidates continue.
The board retains a concise record of the integrated changes and verification.

### Separate improvement opportunities

These may be separate tasks without postponing the basic flow:

- Richer task granularity, classification and dependency expression. The basic
  board-task identity, milestone association and dependency/decision links
  needed to run this flow are required now.
- Quantitative evaluation of the quality and technical debt resulting from AI
  decisions. Decision, change and verification records are required now so
  later evaluation has evidence.
- More accurate conflict analysis and better concurrency tuning. Basic change
  scope tracking and parallel dispatch are required now.

## Implementation

`horizon-board::workflow` holds the domain types and validated state transitions.
A persisted `Plan` contains board item ids; `PlanDraft` is only an agent report
awaiting application. Applying a draft creates or adopts ordinary items,
updates their milestone/dependency links, and preserves active or implemented
task definitions. Unnecessary unstarted tasks can be archived. A task's
instructions, criteria, scope, execution state and results have one canonical
board item.

The writer validates each workflow mutation against the full current board
under its file lock and expected item revision. It appends all changed items
as one `workflow-batch` event, including newly allocated ids. The event's id
covers the highest allocated id, preserving the reader's id-allocation rule.
A stale reservation cannot launch a duplicate or conflicting task. Existing
ordinary items remain readable, and legacy claiming excludes managed work.
Workflow events do not post comments or wake the keeper.

Decisions retain their own conversations, affected item ids and resolution.
Submitting text adds an owner message; only the subsequent discussion result
can settle it. The result is bound to the conversation turn, so another owner
message invalidates a provisional resolution. Reopened decisions also block
successors of previously integrated work. Explicit owner ordering is retained
as precedence constraints when AI updates ranks. Explicit goal edits invalidate
pending tasks until the plan is reconciled with the new goal revision.

The agent daemon runs three roles: planner (including decision conversations),
implementer, and verifier. Each task owns an implementation session/worktree;
verification uses a separate session/worktree at the prepared commit. A single
coordinator lease prevents duplicate schedulers while reservations and monitor
state are per item. Slow worktree/provider startup runs concurrently. Planning,
consultation and eligible task execution can proceed alongside each other.
Task results trigger replanning; a generation counter preserves results that
arrive during a planning turn. Source/functional scope controls conflicting
reservations, and actual changed paths are checked against declared source
scope before accepting an implementation result.

A report remains provisional until the assigned session returns to idle.
Task results identify a clean committed branch. Verification reports identify
the exact prepared commit and provide evidence for every criterion. Successful
automatic evidence references a check command; human evidence references a
settled decision. The monitor checks command strings against actual successful
bash executions in that assignment. Cached, denied and failed results do not
count. Bash output reuse is restricted to a single turn because a coordinator
or another actor can change files between assignments.

The coordinator combines each finished task with current main, verifies that
candidate in the verifier's worktree, and durably reserves its integration.
The final main update is a fast-forward after checking the expected base,
candidate head and checkout cleanliness. An advanced main invalidates the
combined verification. An already integrated candidate is recognized after a
restart or a failed board write. Dirty main changes are preserved and the
problem is attached to the candidate task. Merge conflicts return to corrective
implementation and replanning. Only verified milestone-level criteria establish
achievement.

On daemon restart, provisional planning/conversation work is rescheduled and
verification runs again. An interrupted implementation retains its worktree
and attempt context for replanning or explicit retry; its provisional report
does not become a completed task. Tool approvals use the existing session
approval UI. The board exposes the affected session when attention is needed.

## Native board and CLI

Open an ordinary item and select **Plan, run and integrate into main** to
activate its goal. The default milestone view shows goals, task progress and
concise decisions. Select a decision to discuss it; task and milestone links
open ordinary item details. Detailed history and implementation choices remain
available through the history toggle. Pause, resume, replan, decision messages
and navigation go through the command model. The session action can attach a
daemon-created session that the shell has not previously displayed.

The CLI exposes the same durable records and operations:

```sh
horizon board add "Desired outcome" --body "Context and constraints"
horizon board milestone 48
horizon board flow 48
horizon board answer 48 scope "Would that change the response time?"
horizon board answer 48 scope "Use the current session only."
horizon board pause 48
horizon board replan 48
horizon board resume 48
```

Use the id returned by `add`; 48 is illustrative. `flow` and `show` also work
for task items. `--json` exposes criteria, decision histories, scopes, execution,
verification and integration records. A CLI answer is a conversation message,
not an unconditional resolution. Ordinary board moves establish owner priority
constraints that subsequent planning respects.

The log protocol is **4**, in lockstep. Installing requires a workspace build
and restarting Horizon, `horizon-agentd` and `horizon-logd` together. A runtime
reload alone cannot replace the shell's protocol. Agent and terminal protocol
versions are unchanged.

## Verification and remaining limits

Domain and writer tests cover real task identity/adoption, task-scoped and
transitive decision blocking, conversation versus resolution, stale replies,
concurrent reservations, conflicting scopes, active/completed task preservation,
automatic replanning, explicit owner order, criteria evidence, restart races,
legacy items and actual Postbag request/reply round-trips. Git tests cover main
advancement, repeatable integration recovery, preservation of dirty main and
changed candidates, and actual source scope enforcement. Monitor and bash
cache tests distinguish fresh successful verification from old or failed output.

`python3 scripts/check-board-milestone.py` uses built CLI/log/agent binaries,
a temporary Git repository and a deterministic localhost provider. Barriers
require two independent task sessions and two verification sessions to overlap.
The fixture keeps one decision unresolved while independent branches merge,
checks that a follow-up question remains discussion, forces a fresh verification
when main advances, restarts agentd, settles the decision, runs its dependent
task, and verifies milestone achievement. All commits and main merges belong
to the temporary repository. No external API or operating board is used.
The fixture requires local sockets and daemon process spawning outside session
containment.

Execution currently follows the agent daemon's project directory, and automatic
integration requires an available checkout of main. Routing multiple projects
to a global daemon and replacing an explicitly terminated session/worktree
remain limitations. Human evaluation, real-model planning/decision quality and
native GUI usability need operator dogfooding; the deterministic fixture checks
the machinery, not those judgments. Richer task classification/dependency
expression and quantitative quality/debt analysis remain the separate
improvements identified above.
