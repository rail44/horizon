# Task flow: from a task on the board to a change on main

Status: proposed 2026-11-27, nothing built. The decisions it asks for
are listed at the end and belong on the board.

## The problem

Horizon's agents can do the work; what is missing is the path between
the owner and that work.

- **The board is disconnected from the agents.** Since #114 removed the
  organizer and the automatic deliveries, and #117 is removing
  `board_session`, nothing starts a task's session from the board, and
  the owner's reply in a thread is stored as a comment and delivered to
  no session (`src/board/thread.rs`, `PostMessage`). The board is a list
  of notes.
- **The owner cannot see what a task produced.** Results reach the owner
  as text in a thread or a pane. There is no diff in Horizon; reviewing
  a branch means reading git output or trusting the agent's summary.
- **Which tasks need the owner is not visible at the board's level.**
  Pane marks and unread counts exist, but nothing says "this task's work
  is finished and waiting for your judgment".
- **Integration has no form.** Merging to `main` is git run by a session
  the owner directs, with the gate's run on the rebased commits as the
  rule (`AGENTS.md`), and nothing in Horizon carrying it.

## The goal

**The board is where the owner hands work to agents, answers them,
judges what they produced, and lets it into `main`. How an agent
divides its work among other agents stays inside the agent.**

The owner's attention is what limits how much runs in parallel, so every
step the owner takes should be one command on what is in front of them,
and every state the owner must react to should be visible without
opening a session.

## The loop, as the owner sees it

1. **Write a task** on the board: title and body, as today.
2. **Start it.** One command on the task. Horizon starts an agent session
   for the task in the project's directory and sends it the task.
3. **Talk in the thread.** The agent's answers appear in the task's
   thread; the owner's replies go to the session. The thread is the
   conversation; the session's pane, when opened, shows the full work.
4. **The agent implements** in its own worktree (`worktree_enter`), on
   its own branch. If it divides the work, it does so with its own
   subagents (`task`, write mode gives each child a worktree; the agent
   merges their branches into its own). The owner does not manage them.
5. **The task shows "ready for review"** when the session has stopped,
   its worktree is clean, and its branch has commits not on `main`.
6. **Review.** The owner opens the task's diff (`main...branch`),
   comments on lines, and sends the comments to the session as one
   message. The task goes back to "working".
7. **Integrate.** One command: Horizon checks the branch fast-forwards
   `main`, runs the project's check where the owner sees it, and
   fast-forwards `main` on success. The owner closes the task.

A milestone is a parent task. Its children are tasks the owner (or an
agent, through the board's tools) writes; each child is started and
integrated through the same loop.

## Who does what

| | Does | Does not |
| --- | --- | --- |
| Owner | writes and starts tasks, answers, reviews, integrates, closes | manage the agent's helpers |
| Board (the shell, an ACP client) | starts the task's session, delivers the owner's replies, writes the session's answers to the thread, derives each task's phase, shows the diff, carries out integration | decide what to work on, or when work is good |
| Task's agent | investigates, asks, implements on its branch, divides work among its subagents, reports what it did and how it checked it | start other tasks' sessions, merge to `main`, close tasks |
| Subagents | the part they were given, in their own worktree; report to the agent that started them | talk to the owner or the board |

## Judgments

### One session per task

A task keeps one session from start to integration: the investigation,
the owner's answers and the implementation judgments stay in one
history, as the board's conversation design already has it
(`board-redesign-design.md`, "Board conversation"). `session_id` stays
as it is; `review_session_id` goes with the reviewer role (#117).

Other environments separate a task from its runs (Orca's Dispatch, Vibe
Kanban's task attempt) because their runs are disposable terminal
processes and they compare several runs of one task. Horizon's sessions
are durable and resumable, and comparing alternative implementations is
not a need the owner's board has shown. If it becomes one, a list of
sessions per task can be added to the board's log without changing the
loop above.

### Agents coordinate through subagents, not through the board

Horizon already has a coordinator/worker mechanism inside the agent: the
`task` tool starts children, pushes their results back to the requester
without polling, gives a writing child its own worktree, forwards its
approvals, and reports how it ended (`agent-subagent-design.md`).
Orca's and Claude Code's team orchestration exist at the application
level because the agents they run cannot start one another; Horizon's
can. So the board adds no second mechanism for agents to start or steer
each other, and the board shows the owner's tasks, not an agent's
internal split of one (#24: the board's tasks are the owner's units of
intent, not the agent's units of work).

Between tasks there is no agent-to-agent channel. Tasks meet in the
repository and in the board's dependencies; the owner decides their
order. The organizer, a project-wide agent deciding that, is what #114
removed.

### Phase is derived, never set

Beside the free-text status, each task row shows a phase the shell
computes from what it can read:

| Phase | When |
| --- | --- |
| Not started | the task has no session |
| Working | the session is running a turn |
| Needs approval | the session waits for an approval |
| Ready for review | the session is not running a turn, its worktree is clean, and its branch has commits not on `main` |
| Integrated | the branch's tip is on `main`, and the task is still open |

A conflict with `main` (`git merge-tree --write-tree`) is shown as a
mark on "Ready for review". New answers in the thread are the existing
unread mark, not a phase: whether an answer asks something of the owner
is for the owner to read, and no agent sets an attention flag
(`board-redesign-design.md`, "Unread messages").

### Integration is the owner's command

Moving `main` is the owner's clearance (`AGENTS.md`), so it is a command
the owner runs, not something an agent does. Fast-forward only: if the
branch does not fast-forward `main`, the command tells the session to
rebase and stops, so the check always runs on the commits that will be
on `main`. The check is the project's (for this repository,
`scripts/gate.sh` with the default profile), run in a terminal pane
outside the sandbox; where a project names it is open.

## What this does not do

- No planner, reviewer or integrator roles, and no plan-approval gate:
  how an agent works is its skills' (#24).
- No scheduler choosing tasks to run together; no task started by an
  agent.
- No agent merging, and no task closed because its branch merged.

## Stages

Each ends with the gate clean.

1. **The loop's connection** (#117 stage 3): a Start command on a task;
   the board view starts the task's session as an ACP client, sends the
   owner's replies to it, and writes each turn's final answer to the
   thread.
2. **Phase** on the board's rows and the thread's header, and a command
   that moves to the next task or pane needing the owner (approval,
   unread answer, ready for review).
3. **The diff view** of a task's branch, with line comments sent to the
   task's session as one message.
4. **Integrate**, with the project's check.

## Decisions to take on the board

1. The goal and the division of roles above, including that the board
   shows the owner's tasks and not an agent's split of one.
2. One session per task, with no list of runs for now.
3. Agent-to-agent work through the existing `task` subagents, with no
   board-level mechanism for agents to start or steer each other.
4. Phase derived from the session and git.
5. Integration as the owner's fast-forward-only command running the
   project's check.
