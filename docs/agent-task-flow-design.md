# Tasks: a minimal extension from a task to a change on main

Status: proposed 2026-11-27, nothing built. The decisions it asks for
are listed at the end and belong on the board.

## The problem

Horizon's agents can do the work; what is missing is the path between
the owner and that work.

- **The board is disconnected from the agents.** Since #114 removed the
  organizer and the automatic deliveries, and #117 is removing
  `board_session`, nothing starts a task's session from the board, and
  the owner's reply in a thread is stored and delivered to no session
  (`src/board/thread.rs`, `PostMessage`).
- **The owner cannot see what a task produced** without reading git
  output or trusting the agent's summary; Horizon has no diff.
- **Which tasks wait on the owner** is not visible at the level of tasks.
- **Integration has no form** in Horizon.

The board itself carries what earlier flows needed and the current one
does not: a log daemon, threads that copy an agent's answers, free
statuses, dependencies, read positions, review sessions, a CLI and an
MCP server. Rather than reshape it, this design builds what the goal
needs as a new extension, small, beside it.

## The goal

**One pane where the owner hands work to agents, sees which work waits
on them, judges what it produced, and lets it into `main`.** How an agent
divides its work among other agents stays inside the agent.

## The shape

An extension, `plugins/tasks` (`horizon new tasks`), over `horizon-ui`
and `horizon-components`, with one new host capability per stage.

### A task

A task is a Markdown file the extension writes under Horizon's data, one
per task, in a directory per project:

```markdown
---
session: 3f2a...        # the agent session started for it, once started
closed: false
---
# Title

What is wanted, as the owner wrote it.
```

The owner writes it in the pane (or in any editor; the extension watches
the files). Nothing else writes it: agents are given the task's text when
their session starts and do not read or change tasks.

### The loop

1. **Write a task.**
2. **Start it.** The extension asks the host to start an agent session
   in the project's directory, titled for the task, whose first prompt
   is the task's text; the session's id is written to the task's file.
3. **Talk to it in its pane.** The session's own pane is the
   conversation: the extension opens it (`session-control.show`) and
   does not copy answers anywhere.
4. **The agent works** in its own worktree (`worktree_enter`) and divides
   the work with its own subagents if it wants (`task`, whose writing
   children get worktrees of their own and report back to it).
5. **The task's row shows where it stands**, derived (below).
6. **Review** the task's branch as a diff in the extension; line comments
   go to the session as one message.
7. **Integrate**: the branch fast-forwards `main` after the project's
   check, by the owner's command. The owner closes the task.

A large piece of work is several tasks the owner writes; a task's file
may name a parent, and the list shows children under it.

### Where a task stands

Computed by the extension from what the host lends it; nobody sets it.

| Shown | When | From |
| --- | --- | --- |
| Not started | no session | the task's file |
| Working | the session is at work | `sessions.activity.working` |
| Needs you | the session waits on its owner | `sessions.activity.attention` (approval, input, login, blocked, failed) |
| Stopped | the session is idle and nothing above holds | `sessions` |
| Ready for review | stopped, its worktree clean, its branch ahead of `main` | stage 2's `repository` |
| Conflicts | ready for review, and the branch does not merge cleanly | stage 2 |
| Integrated | the branch's tip is on `main` | stage 2 |
| Ended | the session no longer exists | `sessions` |

The attention it reads is the one the host already derives for tab marks
and the sessions list (`program-status-design.md`).

### What the host lends it

What exists: the document, files under directories the manifest names,
`sessions` (every session's activity, attention, directory, parent),
`session-control` (show, terminate), `modal`, `workspace`.

What it adds, each a capability the manifest asks for and the host
checks as it checks the others (`plugin_host.rs`, `may`):

- **`agents.start(directory, title, prompt) -> session id`** (stage 1):
  a new agent session in `directory`, in a pane not activated, whose
  first prompt is `prompt`: the path `horizon new agent --prompt`
  already takes (`src/agent/plugin.rs`, `Begins`). It is a subset of the
  `agent` interface #117 plans for the wiki.
- **`repository`** (stage 2), read-only, over the repository a directory
  is in: the branch checked out there, whether it is clean, its commits
  ahead of and behind a base, whether it merges cleanly
  (`git merge-tree --write-tree`); and (stage 3) the diff of
  `base...branch` by file and hunk.
- **`agents.send(id, text)`** (stage 3): a message to a session, the
  path `horizon send` takes.
- **Integration** (stage 4): the project's check run in a terminal pane
  at the branch's tip, and a fast-forward of `main` once it has passed.
  How the host learns the check passed (the terminal's exit status) is
  settled in that stage.

## What this does not do

- No threads: the session's pane is the conversation.
- No agent writes a task; no agent starts another task's session; no
  agent merges or closes.
- No statuses, dependencies, read positions or roles.
- No daemon: the files are the store.

## The existing board

It stays as it is while the extension is built; nothing here changes
it. Once the extension carries the loop, whether the board, its daemon,
its CLI and its MCP server are removed is decided then.

## Stages

Each ends with the gate clean and the extension tested through a pane
(`tests/extension.rs`).

1. **Tasks and their sessions.** The extension: list, write, start,
   open the session, the phases from `sessions`. The host's
   `agents.start`.
2. **Branch state.** `repository`'s read-only state; the phases from it.
3. **Review.** The diff in the extension; `agents.send` for the comments.
4. **Integrate.**

## Decisions to take on the board

1. A new extension for the task flow, with the board left as it is until
   the extension carries the loop.
2. The session's pane as the conversation, with no thread.
3. Tasks as Markdown files under Horizon's data, written only by the
   owner (through the extension or an editor).
4. Agents divide work through their own subagents; nothing lets an agent
   start or steer another task's session.
5. The host capabilities above, one per stage.
