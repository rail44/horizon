# Board redesign: tasks, consultation, implementation, and review

**Status: implementation and approved live cutover complete, 2026-09-16.**
Commit `8e057dd` contains the replacement data model, view, session runtime
changes and operating roles; the live cutover used revision `eb6c99c`.
The full host gate (1,952 tests passed), isolated flow, native GUI verification,
and selected-data migration rehearsal passed. The approved cutover preserved
exactly 47 tasks and 185 messages, the ordinary agent and terminal session IDs,
and the workspace layout. The terminal daemon remained running. Independent
live verification confirmed matching prepared binaries and an unchanged board
hash after import, with no retired roles or new board work. Validation and
cutover details are recorded in the implementation plan below.

Current implementation entry points are `crates/horizon-board` and
`horizon-logd` for data, `horizon-agent/src/providers/rig/session/` and
`horizon-agentd/src/session/` for runtime boundaries,
`horizon-agentd/src/board_flow/` for delivery/roles, and `src/board_pane/` for the
view. See the [implementation map](board-redesign-implementation-plan.md) for
package status and verification obligations.

Current choices: completion is a separate `completed` fact alongside free-form
`status`; read state is the furthest displayed message in each task, including
all earlier posts. The task keeps its `session_id`; every review
request creates a fresh ordinary session/worktree pinned to its exact tip, and
`review_session_id` points only to the latest reviewer. Explicit source sends
are persisted in the automatic event batch before the successful tool result,
then delivered through the same durable machinery as final outcomes.

Section 1 records accepted owner requirements. Later sections retain proposal
history and examples where marked; they do not establish additional product
requirements. Source observations about the old implementation refer to audit
revision `07ca0c8d87790092d935eed4e845fbf93d7dd1b3`, not the replacement
code now integrated into main.

## 1. Accepted foundation and its sources

**Technical change scope, 2026-09-16:** the owner accepted rebuilding the board
and extending the existing agent execution machinery. This includes same-session
working-environment activation, explicit resumption of an ended task session,
and input/final-result delivery through the board connection. The integration
work registers task-linked sessions and connects task details to their working
history through the existing session machinery. Existing task and board-message
content is migrated on the board side. The concrete explanation and plan are recorded in
[board-redesign-implementation-plan.md](board-redesign-implementation-plan.md).

**Removal scope, 2026-09-16:** the owner explicitly asked to remove unnecessary
existing features and data, retaining only the functionality discussed here and
the data required to support it. This includes deleting the old workflow's
dedicated UI, operations, automation, and state, plus unused fields and features
outside the agreed task model. Hiding those features while keeping them active
does not satisfy this scope. Existing implementation or generated records alone
do not establish a reason to retain them. The implementation plan lists the
removals and proposes an isolated, one-time importer for useful task and
conversation content; normal board operation would use only the new model.

The owner requested a redesign from the desired development flow:

1. Decompose milestones into concrete tasks.
2. Model task priority and dependencies as data.
3. Clarify judgment points on high-priority tasks through consultation with
   the owner.
4. Begin implementation as tasks become ready; the execution-session choice
   below refines the original request to launch implementation sessions.

**Automatic organization, 2026-09-16:** the owner explicitly expects task
priorities and prerequisite tasks to be organized automatically. Direct human
editing is an additional interaction, not the full organizing workflow. The
owner accepted a board-wide organizing responsibility separate from the
sessions that investigate, discuss, and implement individual tasks. The
organizer compares tasks and maintains their priorities and dependencies;
task sessions supply prerequisite needs and constraints found during detailed
work. Registration first brings a new task to the organizer. After organizing
priorities and dependencies, the organizer selects tasks whose investigation
or consultation should proceed and starts their task sessions. Selection and
parallel-work scope belong to project skill policy. A prerequisite need not
be complete if investigating or consulting beforehand is useful. Direct owner
posts also start the corresponding task session. The task session checks the
consultation outcome and prerequisites before proceeding to implementation.
Reshaping the existing keeper is a candidate for the organizing role; its
current comment-only capabilities do not implement this responsibility. The
concrete role packaging and operations remain technical design work; communication
follows the response-routing and input-handling choices below.

**Task model clarification, 2026-09-16:** the owner uses “milestone” to mean
an ordinary board task at a larger granularity. It is not a separate entity,
task kind, or registration flow. Decomposition creates further ordinary tasks
related to the original task; the relationship can be represented as parent
and children without restricting work to two levels. The larger task has its
own ordinary task-associated consultation session, in which direction and
decomposition can be discussed. A separate milestone planning-session path is
therefore unnecessary for this flow. Subsequent uses of “milestone” in this
document describe that granularity, not another data type.

The initial consultation policy includes technical implementation strategy;
code-level details are handled during implementation. **This boundary
belongs in the board's skill prompt.** It is not a fixed product rule, a
decision-category enum, or a mandatory checklist. The owner accepted designing
the view and its operating skill together: the board retains and exposes the
work structure; the skill guides decomposition, prioritization, consultation,
and when to proceed to implementation.

**Initial skill scope, 2026-09-16:** the owner considers an initial skill that
captures the intent agreed so far sufficient. Natural-language instructions
can be adjusted later; further review of their wording is not an outstanding
design decision.

**Follow-up, 2026-09-16:** for option B (consultation in the board), create
ordinary consultation sessions associated with individual tasks and keep the
scope modest. This replaces the assistant's provisional milestone-wide
consultation-session proposal. The owner made this choice after reviewing the
existing keeper and the proposed changes to session-manager behavior. Detailed
screen behavior and session lifecycle below remain proposals; this decision
does not adopt the earlier workspace attachment redesign.

**Task entry, 2026-09-16:** the owner expects both creation through a
conversation with an agent and direct entry on the board. The entry points
lead to the same ordinary task model, including for large tasks described as
milestones. Task sessions start when selected by the organizer or when the
owner posts directly to their consultation.

**View scope, 2026-09-16:** the owner accepted a simple task list with a filter
for top-level tasks, and a common task detail page showing decomposed child
tasks, their priorities, and their dependencies. The list shows the hierarchy
and task names, states, and unread indications in priority order. Opening a task
switches to its detail within the same board; returning restores the original
list position.

**Detail layout, 2026-09-16:** the owner chose a vertical arrangement, with the
task description and child task list above the task-associated consultation.
The accepted detail outline is: parent-task link; task name and state; task
body; names of prerequisite tasks; child rows with names, states, dependencies,
and unread indications in priority order; then the task's consultation and
message input. Child tasks open in the same detail structure. The detail also
provides an entry point to the associated session's working history.

**Priority representation, 2026-09-16:** the owner accepted storing priority as
the child tasks' list order, with dependency references stored separately and
their task names shown in the list. The fields below are a concrete data-model
proposal following that choice. The owner accepted direct editing:
reorder list rows using keyboard up/down operations or dragging, and add
dependencies by searching for and selecting tasks in the task detail, with
existing dependencies removable there. Human and agent operations update the
same priority and dependency data. Exact keybindings and visual treatment
remain open.

**Priority across parent groups, 2026-09-16:** the owner chose parent priority
as the basis for choosing work across groups, with child order expressing
priority within each group. Prefer work under the higher-priority parent,
and also take tasks from the next-priority parent when they can proceed in
parallel. For example, while a task under parent A is being implemented, an
independent task under the next-priority parent B can proceed too. The organizer
uses the hierarchy, dependencies, and project's parallel-work policy to make
that selection. Store ranks among siblings, including the top-level group;
this choice does not require a separate global task rank or a fixed scheduler.

**Completion, 2026-09-16:** the owner accepted recording the required outcome
and completion conditions in the task description, having the skill-guided
agent check those conditions, and recording completion in task state. The
conditions may vary by task, including whether a usable implementation result
is sufficient or review and integration are required. Availability of a
prerequisite's result to dependent work is part of that assessment.

**Progress state policy, 2026-09-16:** the owner accepted keeping completion
recognizable by the shared mechanism while leaving intermediate state names
and their use to project-specific skills. The board stores and displays those
states. Projects can decide whether consultation, implementation, or review
warrants a distinct state; the product does not impose those stages. Unread
message state remains independent. The concrete representation of completion
alongside project-defined states is now the separate `completed` boolean.

**Execution and review, current basis, 2026-09-16:** the owner chose to have
the task's consultation session continue into implementation, with a different
agent session reviewing the work. This supersedes the separate implementation
session proposed earlier in this document. Forking is not required by this
configuration. The same task session handles code-level investigation and
decisions within the agreed purpose and constraints. If proceeding requires
changing agreed behavior, scope, or technical direction, it pauses the affected
work and returns to the owner with the failed premise, its impact, options,
and a recommendation. The skill guides this distinction.

Human-facing records and replies explain the goal, design decisions and their
reasons, completion conditions, results, and unresolved concerns. Detailed code
work and review findings are handled by the agents. The reviewer reads the task's
requirements and agreed direction alongside the actual changes and validation
evidence; implementation details are corrected between the agents, while issues
requiring a design decision return to the owner. This is the current design
basis, with concrete runtime and interaction details still to be worked out.

**Worktree timing, 2026-09-16:** the owner chose to create the task's dedicated
worktree when it proceeds to implementation. Consultation uses the existing
code beforehand. The same task session retains its conversation while changing
its working environment; creating that environment is not required merely to
consult on or decompose a task.

**Worktree base, 2026-09-16:** the owner accepted having the skill-guided agent
check prerequisite results and choose a suitable starting revision. The creation
operation takes the selected commit, creates the worktree from it, and records
that base. Selection can use main or a branch containing prerequisite work;
the choice remains skill policy.

**Review scope, 2026-09-16:** the owner accepted reviewing a coherent set of
task changes that can contain multiple commits. A request identifies the task,
the recorded starting base, the target tip commit, and validation results.
The reviewer checks the code at that tip and its changes from the base, with
intermediate history available as context. Commit ids fix the reviewed state;
they do not require one commit per task or one review per commit. Findings
return to the task session, with necessary re-review targeting a new tip after
corrections.

**Project-specific integration policy, 2026-09-16:** the owner explained that
integration expectations depend on the project using this mechanism. For
board-driven work on Horizon itself, a personal project, the intended policy
is to proceed to main after agent review and required validation, accepting
that regressions may be corrected afterward. Other projects may require a PR,
human confirmation, or another project-defined process. Integration destination
and the conditions for proceeding belong in project-specific skills and
instructions. The shared board/session mechanism provides the operations and
records their outcomes; it does not impose one integration policy on every
project.

**Dependency-wait resumption, 2026-09-16:** the owner accepted notifying an
existing waiting task session when a prerequisite task completes. For example,
if B has settled direction and is waiting for A, A's completion is delivered
to B's session. The skill-guided agent then checks the current direction,
remaining prerequisites, and availability of the required results before
proceeding. The mechanism delivers the completion event; the decision to begin
implementation remains with the agent's skill policy.

**Parallel work, 2026-09-16:** the owner accepted supporting parallel task
execution in the mechanism while leaving the choice of which tasks to run
together to project-specific skill policy. Independent tasks can proceed
concurrently; closely related changes can be sequenced. A project can also
instruct its agents to work on one task at a time. This decision does not
require the September 13 scope-conflict scheduler or a new concurrency setting.

**Unread messages, 2026-09-16:** the owner chose unread-message state and UI
indication as the way to notice new board messages. The interface tracks what
the owner has read. The owner rejected fixed attention labels and agent
operations to set or clear a human-attention flag. The owner accepted marking
messages read through the position actually displayed in the consultation.
Opening a task detail alone does not mark messages read when the consultation
is still below the viewport. The owner also accepted showing unread indication
on an ancestor when any descendant task has unread messages. This is derived
by the interface from read state. Opening the ancestor does not clear a
descendant's unread messages; its child list lets the owner follow the unread
indication down to the relevant consultation. Exact visual treatment remains
to be designed.

**Board conversation, 2026-09-16:** the accepted direction
is to store the consultation as task-associated board messages. The ordinary
task session receives the owner's posts. The harness stores final answers
addressed to the board as board messages; board tools remain available for
reading and updating board data and for explicit posts during work. The ordinary
transcript is not the board consultation's display source.
The same session continues through consultation and implementation, retaining
the investigation, implementation judgments, and open questions needed for
the work. Limiting what the owner reads must not suppress those working records.
The skill guides the content and depth of answers for the owner.

**Response routing, 2026-09-16:** the owner accepted having the triggering event
specify the final answer's destination and the harness deliver that answer
automatically when the turn completes. The destination can differ from the
sender and is shown to the agent so it can address the intended audience.
Notifications can have no automatic return destination. Explicit send tools
serve communication with another recipient during work; automatic forwarding
and tool-initiated sends use the same session delivery machinery. Only the
final answer is forwarded; working records remain in session history.
The initial version has no mandatory reply-tool checks, automatic reminders
for omitted reply-tool calls, or general reply-obligation tracker. This
supersedes the earlier turn-end board-reply check for human-facing replies as
well as internal communication. Content and when consultation is needed remain
skill and review judgments. Concrete routing context, queueing, and delivery
recovery are implemented in the session input/outcome and board dispatcher
modules; automated validation is recorded in the implementation plan.

**Failure and interruption, 2026-09-16:** the owner accepted having the harness
deliver failure or interruption to the specified return destination when no
final answer is produced. For a board destination, the task detail exposes the
associated session's state or error. For a session destination, the requesting
agent receives the unsuccessful outcome and can decide its next action. Retry
decisions belong to the owner or the requesting agent's skill policy. A session
explicitly stopped by the owner does not automatically restart. This concerns
the work's outcome and subsequent action; it does not change existing
transport-level retry behavior.

**Posts during implementation, 2026-09-16:** the owner accepted delivering new
board posts to the same task session before its next work decision. For example,
when a tool is running, the agent receives the post after the result arrives
and before deciding its next action. An ordinary post does not forcibly stop
the running command. The agent then uses the message and skill policy to
continue, revise its approach, or return to consultation.
The owner accepted separating reading new input from taking on its reply:
additions to the ongoing board consultation are incorporated and answered
together; review results and prerequisite-completion notifications inform the
next decision without changing the current final-answer destination. A request
requiring a reply to another destination is shared as input, but its reply is
handled in a subsequent turn. A new input must not silently overwrite the
current destination. Concrete input classification, queueing, and delivery
details now have an implementation in the session input queue and board
dispatcher; the isolated full-flow fixture passed.

**Resuming an ended task session, 2026-09-16:** if the owner explicitly ends
the task's session using the ordinary session controls, a subsequent owner
post resumes that same session with its history and delivers the post to it.
The task/session association is retained. Ending the session does not itself
restart work. This concerns session termination, not closing a pane or a turn
ending with the session waiting for input. Runtime support for resuming an
explicitly ended session and restoring its working environment is implemented
and covered by focused restoration tests and the daemon-restart fixture.

Sources are the owner's messages in the current design conversation and the
following board comments, read directly using `horizon board list --all --json`:

| Source | Owner's concern |
| --- | --- |
| #24, owner comments | Distinguish roadmap-scale work from concrete assignments. The meaning of “task” drifts between a problem and a unit of work. Full agent context makes the human view verbose. |
| #43, owner comment | Dependency, granularity, and presentation are needed to understand and rearrange priorities as a roadmap. |
| #44, owner comment | Reconsider the board's role; a separate mechanism for agent-to-agent communication is a possibility. |
| Current conversation | The September 13 implementation mostly did not reflect the owner's intention. Redesign from the four requirements above and keep operating policy in prompts. |

The owner's criticism of Codex proceeding unilaterally concerns the Codex
harness/model conducting this work. It does not describe Horizon agent or board
behavior and must not be turned into additional product-skill requirements.

Board status, agent comments, code, and earlier design documents do not establish
owner agreement. The owner has **not** adopted the September 13 feature set or
screen design as this redesign's requirements. The existing verifier/coordinator
pipeline has no assumed place in this scope. Horizon's integration policy above
is a current project-specific choice, not adoption of that earlier pipeline.
The `board-decision-summaries` implementation and `board-human-ui-study` prototype
also remain unadopted alternatives.

## 2. Proposed responsibility split

The package brings together the board's data and operations, its human view,
and the skill that guides an agent using those operations. This is a product
boundary. A board-wide organizer maintains priorities and dependencies using
findings from task sessions. Each task session handles consultation and
implementation, with a separate reviewer. Role and tool details remain open.

| Data, operations, and view | Skill policy |
| --- | --- |
| Preserve ordinary task identity and decomposition relationships; expose the relevant granularity. | Choose useful task granularity and explain each child's contribution to the larger task. |
| Store, display, and change priority and dependency relationships. | The board-wide organizer automatically organizes priorities and prerequisite tasks using the goal, dependencies, owner priorities, and findings from task sessions; incorporate owner corrections. |
| Associate consultation, current direction, and supporting conversation with the relevant work. | Identify judgment points, investigate options, and choose the depth and order of consultation. |
| Store task-associated board messages, track the owner's read position, and display unread messages. | Write questions, answers, and results at a depth appropriate for the owner, preserving detailed working context in session history. |
| Deliver inputs with their origin and optional final-answer destination; automatically forward the final answer there. Provide explicit sends using the same session delivery machinery. The initial version has no mandatory reply-tool check. | Address the specified recipient in the final answer, communicate with other recipients explicitly during work, and recognize when implementation requires further consultation. |
| Distinguish owner decisions, agent proposals, and agent judgments in records. | Read the actual conversation and record its outcome without inventing agreement. |
| Retain the task/session association as the session proceeds from consultation into implementation. | Assess whether direction and prerequisites permit work; handle implementation details and return to consultation when agreed premises need to change. |
| Notify waiting task sessions when their prerequisite tasks complete. | Recheck current direction and required results on notification, then decide whether to proceed. |
| Support concurrent execution of task sessions. | Choose which ready tasks to run together using their relationships, priorities, and the project's concurrency policy. |
| Associate a separate review session and its findings with the task and implementation being reviewed. | Review actual changes against the task's requirements and agreed direction; handle code corrections between agents and surface design questions to the owner. |
| Preserve completion conditions in the task description, store and display progress states, and recognize recorded completion. | Define intermediate state names and their use for the project; check the task's conditions and the availability of its result to dependent work before recording completion. |
| Provide operations for applying the reviewed changes to the specified destination and recording the result. | Follow the project's integration instructions, including direct integration, PR submission, or required human confirmation. |

Priority is an ordering preference. Dependencies describe required preceding
work. An unresolved question is a reason for further consultation. These facts
must remain distinguishable: a high-priority task can still await a prerequisite
or a decision. Their meaning should not be compressed into one queue position.

Operations still need coherent task references and dependency data. Moving
judgment into a prompt does not make that data optional. Conversely, storing a
consultation outcome does not require the program to classify every possible
technical question or interpret conversational agreement through fixed rules.

### Implemented minimum task record

One ordinary task record represents work at any granularity. The current wire
schema is generated from the implementation; migration uses the separate
selected-record importer described in the implementation plan.

| Field | Value and purpose |
| --- | --- |
| `id` | Stable task identifier. |
| `title` | Short task name. |
| `body` | Human-readable Markdown description of the goal, design direction and reasons, completion conditions, and useful source references. Detailed implementation context remains available through session history. |
| `parent` | Parent task id, or null for a top-level task. |
| `rank` | Sortable order key representing priority among tasks with the same parent. Top-level tasks form their own ordered group. |
| `depends_on` | Task ids identifying required preceding work; references may cross parent groups. |
| `status` | Free-form progress text defined by project-specific skills. |
| `completed` | Recognizable completion fact, explicitly set after the task conditions are checked. |
| `session_id` | Task session used for both consultation and implementation, or null before creation. |
| `review_session_id` | Latest reviewer session id, or null before review. Each request creates a fresh ordinary session with a worktree pinned to the requested tip. |

The top-level filter selects `parent == null`. A task's child list selects
records whose `parent` is its id, sorted by `rank`. These queries derive the
children from their records. Reordering changes the stored order without
changing dependency references. This proposal defines priority within each
parent group. The accepted cross-group policy uses parent priority to prefer
work and also selects tasks from the next-priority parent when parallel work
is possible. The organizer's skill makes that selection using dependencies
and project policy; rank does not encode a mandatory serial execution order.

For example, within parent #10, task #12 can rank ahead of #11 while depending
on #11. That expresses a preference to prioritize #12 while preserving the
preceding work it needs. The skill can discuss #12 before its implementation
can begin. Structural validation should reject missing references, self-links,
and cycles within the parent or dependency relationships.

Consultation messages are board records associated with the task. The same
ordinary task session receives those messages and retains its investigation
and implementation history. The harness stores final answers addressed to
that task as board messages; explicit posts during work use board tools.
Messages have stable identities, source provenance and optional timestamps;
routing context belongs to durable session inputs/outcomes. The task
description retains human-readable guidance and links to supporting board
conversation; it does not need to reproduce every implementation detail.
The session fields above retain the accepted execution split. A new owner post
resumes the ended task session with its history; separate reviewer sessions
retain their own histories and pinned environments.
Deciding consultation depth and whether the direction is sufficient remains
skill policy.

Read position is interface-managed reader state, separate from the task's
agent-edited fields. Unread indication follows message and read state; it does
not represent whether a task is ready or whether a decision has been settled.
The view advances one inclusive read position per task and reader. Displaying a
later message marks all earlier posts in that task read, even when scrolling
skips them; older visibility callbacks cannot move the position backward.
Opening a detail without viewing the consultation marks nothing read, and
reading a parent does not mark its children's conversations read. Message order,
not timestamp or message-ID sorting, determines the position.

This follows the owner's 2026-09-16 correction after the initial cutover: the
implementation had preserved unread gaps without showing per-message unread
markers or a way to find those gaps. The owner accepted a simple read-through
position. Existing read records are interpreted at their furthest valid message,
so their gaps close without rewriting the task or conversation log.
Viewing the ordinary session transcript is a different operation from
reading the board consultation; this design does not assume shared read state
between those two histories.

The active `Item` no longer carries legacy workflow, assignee or structured
links fields. Selected useful task/conversation content was extracted by the
isolated importer and migrated during the approved live cutover.

#### Fractional indexing and historical audit

The implementation retains lowercase fractional-index rank strings and scopes
new moves to siblings. The generator validates bounds and reduces append growth
without rewriting imported ranks.

The following observations describe the old generator at audit revision
`07ca0c8d87790092d935eed4e845fbf93d7dd1b3`, before those changes.
Review on 2026-09-16 found that successive bottom insertions produce `n`, `nn`,
`nnn`, and so on; a probe of the actual generator confirmed a 100-character key
after 100 such insertions. Boundary validation also needs attention: the
generator returns a value outside the requested interval for bounds `n` and
`na`. These bounds are not claimed to arise from ordinary generated keys.
The audited write path computed ranks across the whole board, so it needed
to be scoped to siblings under the accepted per-parent ordering.

The existing approach belongs to string-based fractional indexing. Its local
updates are useful for this board: an insertion or move normally assigns one
task a key between its neighbors, preserving their order values. See
[Figma's explanation](https://www.figma.com/blog/realtime-editing-of-ordered-sequences/).
Consecutive integer positions require renumbering affected siblings. Serialized
writes help either representation remain consistent; they do not remove the
extra updates required by consecutive positions.

The observed linear growth on append is specific to the audited generator.
[Rocicorp's implementation](https://github.com/rocicorp/fractional-indexing)
demonstrates prepend/append optimization, including successive keys `a0`, `a1`,
and `a2`. Fractional keys can still grow with repeated insertion into a narrow
interval. The local probe establishes key growth, not a measured performance
problem in the board's workload. Parent-group scope is a separate choice from
key representation.

After comparing these properties, the assistant withdraws the integer-position
recommendation and recommends retaining fractional indexing while assessing
generator improvements and sibling-scoped operations. The specific encoding,
implementation reuse, and any existing-key migration remain open. This revised
recommendation is not an owner-approved implementation decision.

## 3. Proposed interaction through one development cycle

| Moment | What the owner needs to understand or do | What the skill-guided agent does |
| --- | --- | --- |
| Express a milestone | State the outcome and relevant constraints. | Establish enough context to propose concrete work; surface ambiguity that affects decomposition. |
| Review the work structure | See which tasks serve the milestone, their order, and their dependencies; correct the structure or priorities. | Propose tasks and relationships, with enough explanation to judge the order. |
| Consult on a priority task | Understand the task and its current judgment points; discuss or challenge the proposed direction. | Investigate relevant source and behavior, explain technical options, and follow the conversation at the depth the skill calls for. |
| Finish consultation | See what direction was actually settled and what remains open. | Record the direction and its source, distinguishing the owner's decision from the agent's recommendation or judgment. |
| Begin implementation | See that the task is being implemented and follow its existing session when useful. | Continue in the task session when direction and prerequisites permit work. |
| Revisit a premise | Understand what failed and which agreed decision needs to change. | Pause the affected work and explain the impact, options, and recommendation through the task's consultation. |
| Review and correct | Understand the verified result and any remaining design questions or limitations. | A separate reviewer checks the actual changes and validation evidence; agents handle code-level findings and corrections. |
| Integrate according to project policy | See the result and provide any confirmation the project requires. | Use the project's skills and instructions to determine the destination and next integration step. |

For example, task A can be awaiting consultation, task B can have settled
direction and satisfied prerequisites, and task C can depend on B's completion.
The board should make those differences understandable. B's implementation
work can start in its existing session while A still needs discussion; C retains
its dependency. This example illustrates readiness, not a concurrency algorithm.

The proposal does not turn every reply into implementation or require a separate
approval button after clear agreement. How the agent recognizes sufficient
direction is part of the operating prompt. Concrete presentation of the
implementation transition remains view design work; parallel-work policy
belongs in the project's skill instructions.

### Two entry points for the same task model

| Entry | Proposed path to decomposition |
| --- | --- |
| Conversation with an agent | The agent uses the board skill and operations to create an ordinary task. The conversation supplies its context and may already contain direction about decomposition. |
| Direct entry on the board | The owner creates the same ordinary task through the board. Its task-associated consultation can address direction and decomposition, as for any other task. |

The earlier proposal for a separate planning assignment on milestone entry is
superseded by the task-model clarification. A large task's consultation is
already the place to discuss its direction and decomposition. When it is
decomposed, the resulting smaller tasks use the same task and consultation
model. Granularity and how far to decompose are skill policy.

Both paths use the same board operations and task records, so the owner can
inspect and correct the resulting structure from the board. Merely choosing an
entry point does not establish agreement to the proposed tasks or their
implementation strategy. Registration first brings the task to the organizer.
The skill uses the task's content and owner instructions to decide what
investigation or decomposition is useful, including when the task is a memo.
The initial prompt captures the agreed intent and remains adjustable later.

### Minimal task list and detail

The accepted structure is:

- **Task list:** show the parent/child hierarchy and task names, states, and
  unread indications in priority order. Provide a filter showing only top-level
  tasks (tasks with no parent). The filter's default state remains unspecified.
- **Task detail:** switch from the list to the common detail within the same
  board. Show the parent-task link, task name and state, task body, and named
  prerequisites. Follow these with the task's child list, then its consultation
  messages and input. Child rows show names, states, dependency names, and unread
  indications, ordered by priority. A large task uses this same detail page.
- **Child navigation:** opening a child uses the common task detail, where its
  own children can be shown if it has been decomposed further. Returning restores
  the original list position.
- **Working history:** provide an entry point from the task detail to its
  associated agent session's working history.
- **Priority editing:** reorder list rows with keyboard up/down operations or
  dragging. Both inputs change the stored order through the command model.
- **Dependency editing:** search for and select prerequisite tasks in the
  task detail to add dependencies; remove existing dependencies there. These
  operations update the same data available to the agent's board operations.

The task lists indicate unread board messages on the task itself and on
ancestors of tasks with unread messages. This makes descendant messages
discoverable with the top-level filter enabled. Opening a parent exposes the
child rows' unread indication without marking unseen child messages read.
The consultation carries the actual question or report. Exact marks remain
view details; there is no agent-controlled attention indicator.

Task-associated consultation belongs in this common detail, following option B
below. The owner chose to place the task description and child task list above
the consultation. The content outline and navigation above are accepted;
exact control placement, keybindings, and visual treatment remain view design
and validation work. This establishes neither a prototype format nor an
instruction to build screens.

### B: a task session for consultation and implementation

**Accepted scope:** an ordinary agent session associated with each task handles
both consultation and implementation, with a separate review session. The
consultation is stored on the board. The triggering event specifies the final
answer's destination, and the harness delivers the answer there. The board's
skill guides judgment and the level of detail for the recipient, while the
session retains the detailed working context. Board tools support reading and
updating board data and explicit posts during work.
The following develops that scope; exact controls remain proposals.

1. Registration brings task A to the organizer. Its session S starts when the
   organizer selects A for investigation or consultation, or when the owner
   posts directly to A. Retain the task-to-session association. Owner posts
   resume the same S with its history if it has ended.
2. S reads task A, its milestone, relevant dependencies, and board conversation.
   An owner post specifies A's board conversation as the final answer's
   destination. An organizer request to begin consultation with the owner also
   specifies that conversation, even though the organizer sent the request.
   S writes its final answer for that audience; the harness posts it to A.
   A notification without an automatic return destination leaves subsequent
   work and any explicit communication to the skill-guided agent.
   Consultation can precede prerequisite completion; S checks readiness for
   implementation separately.
3. Present task A's board messages in its detail. S's ordinary transcript
   remains its work history. Navigation does not reassign an in-flight reply
   to another task; returning to A restores access to its board conversation.
4. Task B has its own task session. Shared decisions and milestone
   context are read through the board and references when relevant.
5. When ready, S proceeds to implementation of A using the settled direction
   and its existing context. It handles detailed code decisions within that
   direction and returns to consultation if agreed premises need to change.
6. A separate review session R checks A's requirements, agreed direction,
   code at the specified tip, changes from the recorded base, and validation
   evidence. The range may contain multiple commits. Code-level findings and
   corrections are handled between the agents. The review request specifies S
   as the destination for R's final answer; the harness delivers the findings
   to S. Design questions return to the owner.

The minimum conceptual linkage is:

```text
Task A -> session S: consultation -> implementation / further consultation
       -> session R: review, with findings returned to S
```

These are existing kinds of agent sessions; the proposal does not introduce a
new daemon or a session type for each stage. Exact field/tool names remain part
of the data/operation design. Consultation depth is guided by the skill, while
the task association identifies which work the conversation belongs to.
S receives its dedicated worktree at implementation start. Live environment
changes, review requests, and findings delivery now have runtime implementations
under the accepted response-routing principle, with isolated integration validation recorded.

### Messaging scenarios and accepted response routing

The scenarios below organize the required interactions before choosing their
transport. A task-state change, a notification, and a conversational reply can
each be a useful outcome. The initial design need not make all of them tracked
request/reply exchanges.

| Scenario | Communication | Useful outcome |
| --- | --- | --- |
| Register a task | Board registration reaches the organizer. | The organizer reads the task and arranges priorities and prerequisites in board data. A written acknowledgment is not inherently needed. |
| Start investigation or consultation | The organizer starts the task session with the task reference and work to pursue; an owner post can also start it. | The task session investigates and opens useful consultation. |
| Consult with the owner | Board posts reach the task session with the task's board conversation as the final-answer destination. The harness posts the final answer there; tools support explicit posts during work. | The owner and agent clarify direction, including returning to consultation when a premise fails during implementation. |
| Discover work spanning tasks | A task session sends prerequisite needs or constraints and their reasons to the organizer. | The organizer checks and updates board data, or seeks clarification. An extra acknowledgment is only useful if it adds information beyond the shared state. Explicit session sends and prerequisite completion notifications provide the implemented delivery paths. |
| A prerequisite completes | The board's recorded completion notifies existing waiting task sessions. | Each session rereads the current conditions and results, then decides whether to proceed. No conversational reply is needed merely to acknowledge the event. |
| Review an implementation | The task session requests review with the task, base, target tip, and checks, specifying itself as the final-answer destination. The harness delivers the reviewer's final findings to it. | Findings guide corrections or completion assessment; necessary re-review refers to the new tip. This is a substantive result, not just a receipt. |

**Accepted routing principle:** the triggering event supplies the final answer's
destination, and the harness forwards the final answer when the turn completes.
The agent receives this destination as context so the skill can guide content
and detail for that audience. The destination need not be the event's sender.

| Trigger | Final-answer destination |
| --- | --- |
| The owner posts in a task's consultation. | That task's board conversation. |
| The organizer asks a task session to begin consultation with the owner. | That task's board conversation. |
| A task session requests review. | The requesting task session. |
| A completion or result notification arrives. | No automatic return destination; the skill guides subsequent work. |

The harness forwards only the final answer. Investigation records, intermediate
output, and implementation details remain in the session's working history.
Board-directed answers become stored board messages and participate in the
same unread state as other board messages. This does not make the board a view
of the whole session transcript.

An agent can use an explicit send tool to contact another recipient during
work, such as a reviewer requesting additional information. Automatic final
answer forwarding and tool-initiated sends use the same session delivery
machinery. Both are inter-session delivery when their destination is a session;
the difference is whether the harness or the agent initiates the send. The
agent does not need to call a reply tool merely to deliver its final answer to
the event-specified destination. Board events, startup inputs, explicit sends,
and automatic result delivery are wired through the durable board dispatcher.

**Initial reply handling:** do not force a reply-tool call at turn end for
either human-facing board replies or internal communication. Do not add
automatic reminders for omitted replies or a general request/reply obligation
tracker initially. Skills specify what to investigate, change, ask, or report;
ordinary tool results expose actual write or send success and failure.
Automatic routing determines where a final answer goes; its adequacy remains
a skill and review judgment. Turn completion does not mark the task complete:
the task's conditions and skill-guided assessment govern that separate update.

Posts received during implementation are delivered to the same session before
its next work decision. When a tool is running, delivery follows its result
and precedes choosing the next action. The runtime provides this delivery
boundary; the message and skill guide how work changes. Posting alone does not
forcibly stop a running command.

**Accepted handling of input during work:** reading an input and taking on its
reply are separate operations.

| Incoming input | Handling |
| --- | --- |
| An addition to the ongoing board consultation. | Read it before the next work decision, incorporate it into the consultation, and answer together. |
| Review findings or a prerequisite-completion notification. | Use it in the next decision while retaining the current final-answer destination. |
| A request requiring a reply to another destination. | Share its content with the session, but handle that request's reply in a subsequent turn. |

For example, review findings received while preparing an answer for the owner
inform that answer; they do not redirect it to the reviewer. This preserves
prompt access to new input without silently replacing the current destination.
When the work ends through failure or interruption without a final answer, the
harness delivers that outcome to the specified destination. The board exposes
the associated session's state or error in the task detail; a requesting
session receives the unsuccessful outcome as input for its next decision.
The owner or requesting agent's skill policy decides whether to retry, with no
automatic restart of a session explicitly stopped by the owner.
Concrete input classification, routing-context persistence, queueing,
identifying the final output, and delivery recovery remain implementation
questions.

### Historical worktree feasibility assessment

**Audit scope:** this subsection records the source assessment at revision
`07ca0c8d87790092d935eed4e845fbf93d7dd1b3`. Same-session activation and
explicit-base creation are now implemented; the current map/status above
supersedes its statements about missing runtime support.

The existing `crates/horizon-agentd/src/worktree.rs` already separates Git
creation from session startup: `create_isolated_worktree` takes a source
checkout and session id, creates a branch/worktree from that checkout's HEAD,
and returns `WorktreeInfo`. Ordinary `git worktree add` also preserves the
repository's post-checkout build-seeding hook. This is a suitable reuse point.

The proposed responsibility split is:

| Responsibility | Owner |
| --- | --- |
| Create the Git branch/worktree from the specified commit, record the base, and return its identity and paths. | The shared agentd worktree module. |
| Associate the worktree with a session and activate its working environment. | Shared session-runtime code, callable at startup or later. |
| Decide when implementation should start, check prerequisite results, select the base commit, and request the environment. | The task session guided by the board skill. |

The current startup wrapper in `session/setup.rs` also selects the source,
records ownership, and announces the resolved root. Those operations are reuse
candidates, but late activation needs more than that wrapper: `session/run.rs`
currently initializes tool confinement, shell cwd, sandbox-related state, the
provider's environment/instructions, and persisted session context at startup.
Changing the session registry's root alone would leave these inconsistent.

A proposed activation operation should coordinate those changes at a boundary
where old tool work has settled, retain session identity and history, and keep
persistence and UI root information consistent. Repeated requests should reuse
the owned worktree. For this implementation-start operation, failure should
leave the task in consultation and report the error; the existing startup
wrapper's shared-directory fallback is not a suitable implementation-start
outcome. The base-selection responsibility is now accepted; the existing helper
will need to accept an explicit commit instead of always selecting the source
checkout's HEAD. The exact activation mechanism remains open. This is a
source-based feasibility assessment, not an implemented runtime transition.

### Proposed ordinary session-manager behavior

Use the board as a task-oriented interface to an agent session while retaining
the existing workspace meaning of session attachment: an agent session is
attached when it has an ordinary agent pane. Reading or sending a consultation
from the board would not attach that session as the board pane's own session.
This is a small-scope proposal following the owner's task-session decision,
not an additional accepted UI contract.

| Interaction | Proposed behavior |
| --- | --- |
| Session manager listing | Register S as an ordinary agent session, with task A identifiable in its title. Registration must make it visible immediately. |
| Confirm S in the manager | Use the existing behavior: focus its agent pane if attached, otherwise open it in an agent pane showing S's work transcript. |
| Consult through the board | Read/post task A's board messages, with owner posts delivered to S, without changing its ordinary-pane attachment. If S has no agent pane it remains detached in the manager. |
| Switch tasks or close the board | Preserve the task/session association and leave S's lifetime and ordinary-pane attachment unchanged. |
| Terminate S | Use normal session termination and reflect that the consultation session ended in the board. Task and decision records remain. |
| Post after S was terminated | Resume the same S with its history and deliver the new owner post, following the accepted resumption choice. |

Under these semantics, a consultation with no agent pane is included in
`Terminate All Detached Sessions`, just like another registered detached agent.
The board must reflect its termination rather than continuing to present it as
live. A subsequent owner post resumes that same session with its history.
Runtime recovery and restoration are implemented and covered by automated tests. The
keeper's automatic respawn policy is not used for task consultations. Owner
posts explicitly resume task work; passive notifications do not undo a stop.

Active-session commands retain their ordinary pane target. This proposal does
not make a selected board item an implicit target for every session command.
Any contextual operation needed inside the consultation must use the command
model with an explicit session target.

The earlier proposal to classify an embedded conversation as attached to the
board, route session-manager confirmation into a board detail, and extend
workspace attachment persistence is withdrawn from the proposed baseline.
Those changes are not necessary merely to associate an ordinary session with a
task. Option A (conversation in an agent pane) can use the same task/session
association; selecting B's task session scope does not require a conversation
presentation-transfer feature.

### Historical keeper and implementation evidence

This subsection describes audit revision
`07ca0c8d87790092d935eed4e845fbf93d7dd1b3`; the old keeper and workflow
paths have since been replaced in the implementation worktree.

The existing keeper is a daemon-initiated, continuing ordinary agent session
that reads the board and writes context-restoring comments. The board displays
those comments without hosting the keeper's session as its pane attachment.
Its existing role/allowlist does not supply task mutation or assignment tools.
The accepted organizing responsibility requires maintaining priorities and
dependencies across tasks and incorporating findings from task sessions.
Reshaping the keeper is a candidate for that role. Applying the initial
instructions and enabling the necessary tools and communication require runtime
changes; the current keeper does not already perform
this work. This is a board-wide responsibility, not a separate planning agent
for each task called a milestone.

`src/agent/session.rs` already keeps the live agent model independent of its
pane; `src/agent/view/mod.rs` owns transcript, composer, and status presentation.
`src/workspace/modals.rs` implements ordinary session-manager attach/jump.
These are implementation seams to assess, not evidence that task consultation
is already implemented. In particular, daemon-created sessions are not all
pushed into the shell's session inventory immediately (board #46); registration
of a consultation the shell creates must be explicit.

The September 13 discussion path in
`crates/horizon-agentd/src/milestone/launch.rs` instead allocates a fresh session
for each discussion assignment. It does not implement the proposed continuing
task-to-consultation-session association.

## 4. Illustrative board skill prompt

This is the **historical illustrative prompt** used during design. The current
embedded operating instructions are the `board-organizer`, `board-task` and
`board-reviewer` skills in `crates/horizon-board/skills/`; this excerpt is not a
second installed policy. The excerpt distinguishes the
organizer's responsibility from the task sessions' work and captures the
agreed initial intent. The owner considers that scope sufficient for now;
wording can evolve through later use. Role-specific packaging and tool wiring
are implementation work under the established responsibilities.

> Help the owner turn milestones into concrete implementation assignments.
> Read the milestone, related tasks, their priority and dependencies, and the
> relevant owner conversation before proposing changes.
>
> As the board-wide organizer, compare incoming tasks with existing work and
> maintain task priorities and dependencies. Incorporate prerequisite needs and
> constraints reported by task sessions, together with the owner's priorities
> and corrections. The project's skill policy guides these judgments.
> Select the next tasks whose investigation or consultation should proceed and
> start their task sessions under the project's selection and parallel-work
> policy. Use parent priority to choose between groups and child order within
> a group. Prefer the higher-priority parent's work and also take tasks from
> the next-priority parent when dependencies and the project's policy allow
> them to proceed in parallel. Investigation or consultation may be useful
> before prerequisites complete. Each task session assesses its direction and
> prerequisites before deciding to implement.
>
> Decompose work into understandable tasks. Explain their contribution to the
> milestone and identify priority considerations and prerequisite needs. As a
> task session, pass those findings to the organizer for coordination across
> tasks while continuing the task's investigation and consultation. Respect the owner's
> corrections. Keep observations and broad goals recognizable when turning
> them into work; do not silently treat them as implementation instructions.
>
> In a task consultation, keep the assigned task as the conversation subject.
> Read its milestone, dependencies, and related decisions when needed; sibling
> tasks have their own consultation sessions. Keep shared direction in the
> board records so the task session can read the current context.
>
> Read the task's board conversation and the triggering input's specified
> final-answer destination. Address that recipient in the final answer; the
> harness delivers it there. Use explicit board or session send tools when
> another recipient needs a question or report during work, and check the
> operation's result. Retain investigation results, implementation judgments
> and their reasons, and unresolved working questions in session history;
> do not suppress necessary working records to reduce what appears on the
> board. Notifications without an automatic return destination do not require
> an acknowledgment; decide what work or explicit communication is useful.
> Incorporate new owner posts delivered during work before choosing the next
> action. Use their content and the agreed direction to decide whether to
> continue, revise the approach, or return to consultation.
> Incorporate additions to the ongoing consultation into its answer. Use review
> findings and completion notifications as decision inputs while retaining the
> current final-answer destination. A request with another return destination
> can inform the current work, but its reply belongs to a subsequent turn.
> When a requested operation ends through failure or interruption, assess the
> reported outcome and choose the next action under the project's policy.
> Respect an explicit owner stop; do not automatically restart that session.
>
> Bring high-priority tasks forward for consultation. Investigate relevant code
> and behavior so the owner can assess the options. Initially, consult on
> product behavior and technical implementation strategy; handle code-level
> details during implementation. Adapt this depth to the owner's
> instructions instead of treating it as a permanent category boundary.
>
> Explain the current issue and the reasons for a recommendation in language
> the owner can use to decide. Follow questions and corrections as conversation.
> A question, an agent report, or a prior document claiming agreement is not
> evidence of an owner decision. Keep recommendations, agent judgments, and
> the owner's actual decisions distinguishable and connected to their sources.
>
> When the conversation supplies sufficiently clear direction and prerequisites
> permit implementation, record the direction and proceed in the same task
> session. Keep the human-facing task description understandable, with the goal,
> design decisions and reasons, completion conditions, and useful references.
> Preserve detailed investigation and implementation reasoning in session history.
> At implementation start, check that the chosen starting revision includes the
> prerequisite results needed for the work. Request the task worktree from that
> specific commit and retain the recorded base.
> When notified that a prerequisite completed, reread the current task direction
> and dependency results. Proceed when the required conditions are satisfied;
> otherwise retain the remaining reason for waiting or return to consultation.
> Follow the project's policy for parallel work. Consider the task's priority
> and relationship to other work when choosing to proceed concurrently or in
> sequence.
>
> Resolve implementation details within the agreed purpose and constraints.
> If proceeding requires changing agreed behavior, scope, or technical direction,
> pause the affected work and return to the owner with the failed premise, its
> impact, options, and a recommendation.
>
> Request review from a separate agent session, providing access to the task's
> requirements, agreed direction, recorded base, target tip commit, and
> validation evidence. Review the task's changes as a whole across that range;
> use intermediate commit history as needed. Necessary re-review after corrections
> identifies the new target tip.
> Handle code-level findings and corrections between agents. Report the result,
> supporting verification, remaining limitations, and unresolved design questions
> in language the owner can assess.
>
> Keep the required outcome and completion conditions in the task description.
> Follow the project's skill policy for intermediate state names and their use.
> Use the review findings and check the actual result against those conditions,
> including whether dependent work can use it, and record completion in task
> state when they are met. Follow the project's skills and instructions for
> integration destination and conditions, together with the task's conditions
> and the owner's directions. Proceed with direct integration or PR submission,
> or obtain human confirmation, as that project's policy requires.

## 5. Design and transition completion

The owner asked to focus further consultation on consequential design choices;
routine implementation judgments do not each need a separate consultation.
The following records the completed scope; it does not adopt additional
features or authorize a prototype.

### Current product design basis

The main responsibilities, execution flow, and list/detail content and
navigation are agreed. The initial skills need to include the intent agreed
so far; their wording is adjustable later. The technical direction and change
scope are also accepted. Implementation, automated validation, native GUI
inspection, selected-record migration rehearsal, approved live cutover, and
recovery checks are complete.

Concrete controls and visual treatment should follow the accepted outline.
Validate readability and navigation with representative task content as part
of the view work. These details do not each require a separate design
consultation. The native board view passed representative GUI verification,
including actual input, visible-only read state, and same-ID session resumption.

### Technical design and transition work

The [implementation plan](board-redesign-implementation-plan.md) records the
bounded source audit, accepted technical direction and change scope,
proposed work-package dependencies, migration approach, and meaningful validation.

Source inspection supports a serialized environment change that retains session
history, plus explicit ended-session restoration and continuation-aware final
result delivery. Automated runtime validation passed; see the implementation plan. The owner's scope
correction requires deleting unnecessary existing features and active data.
The revised migration proposal confines historical workflow decoding to an
isolated importer and carries selected useful task/conversation content into
the new model. Ordinary rank/edit writes and startup/resume paths have been
replaced and integrated into main. Import-only startup produced no provider
requests or new board events in the isolated rehearsal. Live recovery preserved
the ordinary session IDs and layout; verification found no new board work and
confirmed the imported board hash remained unchanged.

The proposal starts with data and session contracts, then sequences the shared
runtime changes while allowing view work against stable interfaces in parallel.
Organizer/task/reviewer wiring, the full host workspace gate, isolated complete
flow, native GUI checks, and the selected legacy inventory/rehearsal have passed.
The approved live replacement preserved tasks 1–47 and all 185 messages, archived
the original log, and excluded only workflow-generated tasks 48–49 from active
data. The full app restart used matching prepared binaries while retaining the
running terminal daemon. Independent live verification passed. Routine technical
details can be resolved within these choices; material changes to accepted
behavior or scope should return with concrete options and evidence.
