# Task flow: tasks, attempts, review and coordination

Status: proposed 2026-11-27, nothing built. The decisions it asks for
are listed at the end and belong on the board; until they are taken
there, nothing here is settled.

## Why now

Board #117 takes the board's operations out of the agent
(`board_session`'s `consult`, `implement`, `review`, `send`, the
board-task and board-reviewer roles, the reply sink, later the outbox)
and leaves their replacement open: "the board's mechanism is to be made
smaller later, after the shape of other agents' tasks", and what the
remade board needs "is decided then" (`agent-sessions-on-demand-design.md`).
Its stage 3 makes the board view an ACP client that starts a task's
session, prompts it and writes the answer to the thread. This document
is a proposal for the shape that stage 3 and the stages after it build
toward, drawn from how current agentic development environments run
tasks across several agents, and from what Horizon's own earlier flows
taught.

## What the shape has to respect

These are constraints already on record; the proposal is built inside
them rather than around them.

- **The agent knows nothing of the board** (#117). Everything below is
  the shell's (the board view as an ACP client) or an MCP server the
  shell names for a session; nothing is added to `horizon-agent`.
- **No agent-set attention flag, no fixed attention labels.** Attention
  is derived by the interface from state it can read
  (`board-redesign-design.md`, "Unread messages"; `program-status-design.md`).
- **Status stays free text** and `is_closed` stays the only lifecycle
  bit a person or an agent writes (`crates/horizon-board/src/model.rs`).
- **A milestone is a task with children** (`board-redesign-design.md`).
- **Which tasks run in parallel is project policy**, not a scheduler
  (`board-redesign-design.md`, "Parallel work").
- **Moving `main` is integration and needs the owner's clearance**
  (`AGENTS.md`).
- **What did not hold up:** a standing, project-wide coordinator agent
  (the keeper, then the organizer, removed in #114; standing memory never
  checkpointed in practice), and a fixed planner/implementer/verifier
  pipeline with automatic merge (the September 13 flow,
  `board-milestone-flow-design.md`).

## What other environments converge on

From a survey of Orca, Conductor, Vibe Kanban, Nimbalyst, Sculptor,
Claude Code (agent view, agent teams), Codex, Cursor, Kiro, GitHub
Copilot / Agent HQ, Devin, Antigravity, Warp/Oz, Zed and Amp
(November 2026):

1. **One isolated checkout per unit of work.** Desktop tools bind a
   worktree and branch to each task run; the run's end removes or
   archives it.
2. **A task is separate from its runs.** Orca's Task vs Dispatch, Vibe
   Kanban's task vs task attempt, Agent HQ's one issue given to several
   agents. A failed or lost run leaves the task to run again; several
   runs of one task are compared (best-of-N).
3. **"The agent finished" means "ready for review", never "done".**
   Done means merged, and almost no tool lets an agent merge.
4. **Review is a diff with line comments that go back to the run's
   agent as one message.** CI results and conflicts are routed the
   same way. Conductor's Checks tab collects what still blocks a merge.
5. **"Needs input" floats to the top**, with a key to jump to the next
   session that needs a person (Conductor, Claude Code's agent view).
6. **Coordination is coordinator/worker with typed messages and wake on
   event.** Workers report once with an outcome and evidence; the
   coordinator is woken by the report, never polls; a message from an
   agent never counts as a person's consent. Orca distinguishes
   "finished with problems" from "could not finish" and records an
   unconfirmed end as such rather than as failure.

The criticisms are as consistent: review is the bottleneck, parallel
worktrees isolate files but not intentions (two runs fixing one bug in
opposite directions), reported success is unverified, and status lags.

## The proposal

### 1. A task has attempts

An **attempt** is one session's work on a task: the session, and, once
it writes, its worktree, base commit and branch. A task has any number
of them. The task's first session (the one its thread talks to, which
carries investigation into implementation, as the board's conversation
design has it) is its first attempt; a second attempt is a fresh session
on the same task: a retry, an alternative, or a review.

In the board model this replaces `session_id` and `review_session_id`
with a list:

```rust
pub struct Attempt {
    pub session_id: String,
    pub kind: AttemptKind,        // Work | Review { of: String }
    pub base: Option<String>,     // commit the worktree started from
    pub branch: Option<String>,   // horizon/<slug>/work
    pub started_at: String,
    pub ended: Option<AttemptEnd>,
}

pub enum AttemptEnd {
    Finished,          // the session's turn ended and the session was let go
    Unfinished(String),// it reported it could not finish, and why
    Lost,              // its end was not observed (the process went away)
}
```

On the wire this is additive to `horizon.board.event_log` v2
(`attempt-started`, `attempt-ended`); the existing two fields fold into
the first `Work` attempt and the latest `Review` attempt on read, so
older logs keep their meaning.

The board view writes these, as the client that started the session;
no agent writes them.

### 2. Where a task stands is derived, not set

The board row shows a derived **phase** beside the free-text status,
computed from facts the shell can read:

| Phase | From |
| --- | --- |
| Working / Needs approval / Needs input | an open attempt's session state (`BoardSessionActivity`, already derived) |
| Ready for review | a `Work` attempt has ended and its branch has commits not on the integration branch |
| Conflicts | as above, and `git merge-tree --write-tree` against the integration branch reports a conflict |
| Integrated | the attempt's branch tip is reachable from the integration branch; the task is open |
| Closed | `is_closed` |

No one sets a phase, so it cannot lag behind or claim more than the
repository and the sessions show. It is attention derived by the
interface, as the unread state is.

### 3. Review in Horizon

A **diff view** (the roadmap's first-party git diff viewer, as an
extension) opens an attempt: `base..tip` of its branch, file by file.
A person comments on lines; **Send Review** sends the comments as one
prompt to the attempt's session, through the same client path stage 3
builds for the thread. The view's header lists what still stands
between the attempt and integration: unanswered comments, a conflict
with the integration branch, and the result of the project's check if
it has been run (section 4). It does not decide; it shows.

A `Review` attempt (an agent reviewing another attempt) is optional
and started like any attempt, on a worktree the board makes at the
tip (#117); its answer goes to the thread, where the person reads it
next to the diff.

### 4. Integration is a command of the owner's

**Integrate Attempt** (a `CommandId`, from the diff view, the board, or
the palette) is the owner's clearance, carried out mechanically:

1. refuse unless the attempt's branch fast-forwards the integration
   branch (otherwise prompt the attempt's session to rebase, and stop);
2. run the project's check on the tip, in a terminal pane outside the
   sandbox, where the person sees it (for this repository,
   `scripts/gate.sh` with the default profile, which is what the
   integrator's run requires);
3. on success, fast-forward the integration branch and record the
   attempt as integrated; the task stays open until someone closes it.

The check is a per-project setting (where it lives, `config.toml` or
the repository's `.horizon/`, is open), so the shell carries no
knowledge of any one project's gate.

### 5. Coordination between agents, scoped to a subtree

The owner starts sessions on tasks from the board (stage 3). An agent
can coordinate too, but only **beneath the task its session works on**:
the session of a milestone can run its children; no session can reach
outside its own subtree. A coordinator exists because a person started
one on a task. Nothing runs standing or project-wide.

The tools come as an MCP server the shell names for board-started
sessions, `horizon mcp tasks` (beside `horizon mcp board`, reaching
the shell through `HORIZON_SOCKET`):

- `task_run {id, prompt, worktree: new | none}`: starts an attempt on a
  descendant task; refused outside the caller's subtree or past the
  project's limit of open attempts per coordinator.
- `attempt_send {session_id, text}`: steers an attempt the caller
  started.
- `attempt_stop {session_id}`.

When an attempt the coordinator started ends, the shell prompts the
coordinator once with a short notice: task, attempt, how it ended
(`Finished`, `Unfinished(reason)`, `Lost`), branch, and its final
answer's first lines. The coordinator is woken by the notice, never
polls. Notices and `attempt_send` arrive as prompts marked as coming
from a session; under the approval tiers they confer nothing, so an
approval stays where the attempt's own session asks for it.

Best-of-N is `task_run` twice on the same task; comparing the two is
two diff views. A view that compares attempts side by side comes after.

Workers report "what I changed and the evidence that it works" in their
final answer; that is skill policy, not a mechanism, and the board only
shows it.

### 6. The next thing that needs a person

**Go To Next Attention** (a `CommandId`, bound in workspace mode) moves
the cursor to the next pane whose session needs a person, in the order
approval, input, failed, then tasks Ready for review (opening the diff
view for those). It reads only what `Attention` and the derived phase
already hold.

## What this does not propose

- A plan-approval gate before implementation, or fixed roles. Whether a
  session plans first stays the skills' (#24, and the roles fork in
  `agent-roles-and-skills-design.md` is untouched).
- A scheduler that picks which tasks to run together.
- Agents merging, or closing tasks because something merged.

## Stages

Each ends with the gate clean.

1. **#117 stage 3, with attempts.** The board view starts and prompts a
   task's session and writes the answer to the thread (as #117 plans),
   and records it as the task's first attempt. The board log gains
   `attempt-started` / `attempt-ended`.
2. **Derived phase** on the board rows and in the thread's header;
   **Go To Next Attention**.
3. **The diff view** as an extension, with Send Review. It needs the
   `agent` interface in `plugin.wit` that #117 stage 4 introduces for
   the wiki, so it follows that stage or brings the interface forward.
4. **Integrate Attempt**, with the project's check as a setting.
5. **`horizon mcp tasks`**: scoped `task_run` / `attempt_send` /
   `attempt_stop`, and the end-of-attempt notice to the coordinator.
6. Later: attempts compared side by side.

#117's stages 5 and 6 (the outbox and input queue going, one process
per session) are unaffected: nothing here delivers through the agent.

## Decisions to take on the board

1. **Task and attempts.** Replace `session_id` / `review_session_id` with
   a list of attempts (section 1), or keep one session per task.
2. **Derived phase.** Show a phase derived from sessions and git
   (section 2) beside the free-text status, or keep status alone.
3. **Agent coordination.** Let a session run descendants of its own
   task through `horizon mcp tasks` (section 5); or keep starting
   sessions to the owner alone, with no agent-to-agent path beyond the
   board's own thread.
4. **Integration.** A shell command that runs the project's check and
   fast-forwards only (section 4); or leave integration to a session
   the owner directs, as today.
5. **Order.** Whether the diff view waits for #117 stage 4's `agent`
   interface or brings it forward.
