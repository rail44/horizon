# Milestones, decisions, and execution

Status: product direction agreed with the owner on 2026-09-13; implementation
proposal. The browser prototype is a simulation, not a functioning Horizon
workflow. No live board data is changed by opening it.

## 1. Outcome and evidence

The owner supplies features or outcomes as milestones. Horizon investigates,
decomposes work, prioritizes executable tasks, runs implementations, and revises
the plan from their results. The owner can understand and redirect this work.

The first proof is one real milestone with at least one real implementation
attempt, through planning, any necessary consultation, execution, verification,
and incorporation of the result. Multiple milestones, shared work, and parallel
execution remain part of the destination; this first proof does not satisfy them.

The owner's immediate obstacle is the inability to see what decisions exist:
too much noise, with judgments not expressed concisely. Asking the owner to
allocate decision authority before presenting those decisions puts the work on
the wrong side of the interface. Discovering and explaining decisions is part
of Horizon's responsibility.

The 2026-09-13 read-only inspection of main `79d2c1b` found 47 board items,
all without parent, dependency, or structured link values. There is tree-rendering
code, but no tree in the actual board. Existing items cannot be reparented through
the public operations. Dependencies and links have model fields but no public
write operations. The implementation must be assessed through real operations
and visible behavior, not the existence of a field or renderer.

This is a product design, not a repository development-flow specification. It
does not reinstate the development-flow document retired by the owner.

## 2. The human-facing projection

The main view answers four questions without requiring a transcript read:

1. What outcome are we trying to achieve, and how will we recognize it?
2. What is happening now, and what will happen next?
3. What remains undecided, why does it matter, and what work does it affect?
4. What evidence supports the reported progress?

Milestone overview: intent, acceptance criteria, current phase, a short current
finding, unresolved decisions, and executable/waiting work with reasons. Task
counts alone are not a measure of milestone completion. Source material, full
attempt logs, and superseded discussions remain available through detail views.

The current finding is maintained from the current plan and observed execution
state. It does not acquire a new permanent comment every time the worker reports
progress. An append-only audit history can coexist with this current projection.

A decision contains a concise question, why it matters now, materially different
options, the recommendation and its reasoning when there is enough evidence,
affected criteria/tasks, and source references. Its disposition distinguishes
missing evidence, open consultation, answered, and superseded/reopened. Neither
every unknown nor every option is automatically a question for the owner.

Known repository decisions and explicit instructions remain inputs. Researchable
facts produce investigation tasks, not owner questionnaires. Equivalent questions
across tasks share a decision record. A resolved decision leaves the current
attention list. New evidence can reopen it with the changed premise identified.
No decision is invented just to satisfy a template or demonstration.

The view accepts natural-language correction as well as selection of a proposed
course. Receiving a message is not the same as understanding and resolving a
decision; the actual plan change must remain inspectable. Previously granted
authorization persists; normal progress does not require repeated approval.

## 3. Product objects and operations

These are conceptual responsibilities, not a finalized Rust schema.

| Object | Required information and behavior |
|---|---|
| Milestone | Owner intent; proposed/accepted criteria and their origin; priority relative to other goals; scope revisions; related tasks and decisions; evidence for achieved criteria |
| Task | Concrete outcome; completion checks; dependencies; related milestone(s); unresolved blockers; references; plan revision; current execution attempt |
| Decision | Question, rationale, alternatives, affected work, evidence, disposition, resolution, author/origin, and the premise revision to which it applies |
| Attempt | Task and plan revision; durable attempt identity; session/worktree/branch references; current state; verification results; integration state; artifact references |
| Plan revision | Task/dependency/priority changes with reasons, relevant decisions, and a base revision for rejecting stale concurrent writes |

Milestones must not be inferred solely from `parent == None`: the existing
top-level items mix goals, implementation tasks, investigations, and findings.
Grouping and executable dependency are different relationships. Whether tasks
can belong to several milestones is still open; do not silently encode single
ownership while claiming shared work is supported.

Required operations include creating/editing milestones and criteria, proposing
and revising a plan, organizing existing tasks, setting dependencies, recording
or reopening decisions, binding an attempt, reporting verification, recording
integration, and pausing/resuming dispatch. References retain stable identities
when tasks move. Splitting or merging tasks must preserve the old references and
explain where remaining work moved.

The host validates referential integrity, cycles, invalid transitions, stale plan
revisions, and duplicate launches. These constraints do not depend on an agent
remembering prose instructions. Migration keeps the current board log readable
and preserves existing comments/IDs. Classifying legacy items is an explicit,
reviewable organization operation, not an automatic claim that they are all goals.

## 4. One complete run

| Trigger | Horizon action | Visible result |
|---|---|---|
| Owner describes an outcome | Persist intent; investigate code, board, decisions, and related work; propose criteria and tasks | A concise plan, known facts, assumptions, and any genuine unresolved decisions |
| Investigation establishes a fact | Update evidence and plan; retire questions it resolves | Research disappears from the attention list; affected tasks become eligible where appropriate |
| A decision is answered | Preserve the answer and resulting interpretation; revise affected work and dependencies | The new plan and reasons; the same question is no longer active |
| A task becomes eligible | Select work by goal priority, dependencies, unresolved blockers, conflicts, and capacity; reserve and launch an attempt | Task, rationale for starting it, and a reachable execution session |
| Worker produces output | Capture structured artifact/result references and evaluate task checks | Separate implementation, verification, and integration states |
| A check fails | Preserve the attempt and failure evidence; repair or investigate within scope; surface a new judgment only if necessary | Specific cause, next action, and effect on dependent work |
| Verified work is integrated | Record integration evidence; evaluate criteria; make downstream work eligible | Updated milestone progress and remaining work, without manual status repair |
| Intent or priority changes | Revise queued work; show consequences for active attempts and completed artifacts | Inspectable changes and reasons instead of a hidden queue reshuffle |

Existing permissions continue to govern implementation and integration. In this
repository, integration into main needs explicit owner clearance; the product
must distinguish a verified branch from an integrated change. This is not a new
blanket approval step for planning, task execution, or ordinary reversible work.

An unresolved decision blocks only work affected by it. Eligibility is a
computed predicate, not just a manually assigned `ready` string. A first rollout
can use one dispatch slot while exercising dependency selection; this is not
evidence that parallel scheduling or cross-milestone fairness works.

The execution packet contains milestone intent, task outcome and checks, current
decisions, dependency artifacts, relevant source pointers, and the plan revision.
It should not be the unbounded board comment history. No owner copy/paste into a
new session is required.

## 5. Reliable execution and recovery

Record the launch intent and attempt ID before dispatching. A daemon restart
must reconcile that attempt against existing sessions rather than spawn a second
worker for the same task. Retries have distinct attempt IDs, and stale results
must not complete a newer plan revision accidentally. Interrupted or unreachable
execution is distinct from success, failure, or task completion.

Dispatch pause stops new launches. It does not silently terminate workers or
their worktrees. Existing sessions remain reachable; termination remains an
explicit operation. A scope change records which active attempts have become
stale and what should happen to their results.

The coordinator uses persisted board state as the source of truth for scheduling.
Agent memory is useful for reasoning but is not the authoritative queue or
recovery journal. Comment arrival and worker turn completion are not sufficient
signals for task completion.

## 6. Composition with existing code

| Existing surface | Proposed responsibility |
|---|---|
| `horizon-board` | Domain records, operations, folds/projections, plan validation, and persisted coordination state; retain the boundary without a dependency on `horizon-agent` |
| `horizon-logd` | Serialized durable operation application and subscription notification; no model prompting or dispatch policy |
| `horizon-agentd` | Compose the board capability and agent runtime; planning/execution coordination, session binding, reconciliation, and result collection |
| `horizon-agent` | Tool contracts and model-facing schemas through a host capability; existing role/skill registration seams; no direct board crate dependency |
| Shell and `horizon-workspace` | Milestone/decision/task views and command bindings; every new user operation follows the shared command model |
| `horizon-cli` | Equivalent inspect/edit/decision/control operations for automation and diagnosis |

There is no decision here to add a daemon or one persistent agent per feature.
The number and lifetime of reasoning sessions need separate validation. Existing
Keeper is a context-restoration role with comments as its only write capability;
it must not be described as an already functioning planner or dispatcher.

New wire operations need the existing schema artifact/version discipline for
the affected hub. A protocol bump requires a full shell restart during the
real application trial, not just a runtime reload. UI updates must subscribe to
plan/execution changes, including title/body and status changes; the current
Keeper policy ignoring `ItemUpdated` cannot be reused as a workflow policy.

Board #46 is a concrete execution-visibility dependency: a daemon-created agent
must be inspectable and attachable from the shell. A work-item-to-session binding
is not useful if the user cannot open the referenced session.

## 7. Implementation increments

Each increment needs a visible operation and an acceptance check. None alone is
the completed milestone workflow.

| Increment | Deliverable | Acceptance check |
|---|---|---|
| A. Executable design example | Interactive simulation, object responsibilities, operation contracts, and this verification plan | The reader can distinguish a decision, an implementation attempt, verification failure, and remaining milestone work without reading logs |
| B. Durable milestone/decision/task operations | Records and transitions; organization of existing items; dependencies; current projection; shell/CLI parity | Organize actual existing tasks, answer/reopen a decision, restart, and recover the same plan with stable references |
| C. Planning into that projection | Grounded investigation and structured plan changes; decision deduplication; evidence and revision handling | A real goal produces actionable tasks; facts are researched; a decision changes the affected plan and does not recur unchanged |
| D. One real implementation attempt | Eligibility, durable reservation, session spawn/visibility, context handoff, result capture, pause and recovery | An eligible task runs without manual prompt transfer; a restart does not duplicate it; failure remains visible and recoverable |
| E. Verified result to next work | Checks, integration state, evidence-based criteria evaluation, and downstream eligibility | A failed check never advances completion; successful integrated work advances affected criteria and starts the next eligible task |
| F. Multiple goals and concurrency | Shared work, milestone ordering, resource/conflict-aware dispatch, priority changes during work | Reordering goals changes upcoming work predictably; concurrent attempts do not duplicate a task or overwrite conflicting plans |

B through E form the first real trial. A is the artifact introduced by this
document and does not count as that trial. #47's existing gate repair is already
on another branch; it is a prerequisite for a clean implementation baseline, not
a new board workflow feature or a duplicate work item to create here.

## 8. Acceptance of the first real trial

Use an actual owner-selected goal and an implementation task with observable
completion checks. Preserve the record across a shell and daemon restart.

- The owner can state the goal, current work, remaining decisions, and next
  action from the overview alone. Feedback on this is required from actual use;
  unit tests and the HTML demonstration cannot establish usability.
- The plan identifies assumptions as assumptions. Resolved and duplicate
  questions do not remain in the active decision list.
- Decisions update the plan and task eligibility without manual re-entry.
- At least one real worktree-backed session receives the correct task context,
  produces a code change, and returns traceable verification/artifact references.
- A forced failure remains a failure until corrected and verified. Merely ending
  a model turn cannot mark the task done.
- Restart during a launch or execution recovers the original attempt and never
  creates a duplicate worker. Pause does not terminate active sessions.
- Task verification, integration, and milestone acceptance remain distinct.
  Remaining criteria stay open after partial implementation.
- All required repository gates pass on the integrated candidate, with the
  sandboxed nextest profile used inside containment. Boundary recovery tests
  must also be covered by the integrator's default profile.

## 9. Open design choices

Only product direction and the single-milestone-first validation approach are
agreed. These remain proposals to resolve from concrete scenarios:

- Whether task membership in milestones is single or multiple, and how shared
  work contributes evidence to each goal.
- The durable representation and migration path for richer records alongside
  existing items, including plan revisions and concurrent edits.
- How autonomous decisions are classified and made inspectable. The owner has
  not supplied a blanket approval policy, and should first be able to see the
  actual decisions.
- Execution/session topology and how much planning memory to preserve.
- The conflict and capacity model used for parallel work.

References: [Keeper](board-keeper-design.md),
[runtime boundaries](view-runtime-principle.md),
[wire evolution](remoc-adoption-design.md),
[Japanese walkthrough](research/board-milestone-flow-2026-09-13.md),
[interactive prototype](assets/board-milestone-flow/index.html).
