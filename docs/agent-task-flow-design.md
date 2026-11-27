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

The board carries what earlier flows needed and the current one does
not (a log daemon, threads copying an agent's answers, free statuses,
dependencies, read positions, review sessions, a CLI, an MCP server).
This design builds what the goal needs as a new extension beside it.

## The goal

**One pane where the owner hands work to agents, sees which work waits
on them, judges what it produced, and lets it into `main`.** How an agent
divides its work among other agents stays inside the agent.

## The loop

1. **Write a task**: a Markdown file under Horizon's data, one per task,
   in a directory per project. Its front matter holds the agent session
   started for it and whether it is closed; the rest is the owner's text.
   Only the owner writes it, through the extension or an editor.
2. **Start it**: an agent session in the project's directory, titled for
   the task, told the task's text first.
3. **Talk to it in its pane** (`session-control.show`). Nothing is copied
   into the extension.
4. **The agent works** in its own worktree (`worktree_enter`), dividing
   the work among its own subagents if it wants (`task`).
5. **The task's row shows where it stands**, derived (below).
6. **Review** the branch's diff in the extension; line comments go to the
   session as one message.
7. **Integrate**: the project's check runs in a terminal the owner sees,
   then `main` is fast-forwarded to the commit it ran on. The owner closes
   the task.

| Shown | When |
| --- | --- |
| Not started | no session |
| Working | `sessions`: at work |
| Needs you | `sessions`: attention (approval, input, login, blocked, failed) |
| Stopped | the session idle, nothing above |
| Ready for review | stopped, its worktree clean, its branch ahead of `main` |
| Conflicts | ready for review, and the branch does not merge cleanly |
| Integrated | the branch's tip is on `main` |
| Ended | the session no longer exists |

## What the extension needs, and where each comes from

The extension needs no interface made for tasks. Each need is met by
WASI, by an interface the host already has, by one generic addition to
the host, or by the agent extension.

| Need | From |
| --- | --- |
| Read and write the task files | `wasi:filesystem`, with `access(write = ["{data}/tasks"])` |
| Notice a task file or a branch changed | `horizon:ui/files.watch` (exists; WASI has no way to wait for a change) |
| Sessions' state, attention, directory | `horizon:ui/sessions` (exists) |
| Open a session's pane | `horizon:ui/session-control.show` (exists) |
| Branch state: checked out, clean, ahead/behind, conflicts, diff | the guest reads the repository itself through `wasi:filesystem` (below) |
| Start a session, send it a message | the agent extension's `horizon:agent/sessions` (below) |
| Run the check and fast-forward `main` | a terminal running a command: one generic host addition (below) |

### Git in the guest

The repository is files; reading it needs no host interface. Tried
2026-11-27 with gitoxide (`gix` 0.91, features `status`, `blob-diff`,
`merge`, `revision`, `sha1`) built for `wasm32-wasip2` and run under
wasmtime 51 with the repository lent as a directory: from a worktree
under `.horizon/worktrees/`, the branch and its head, the merge base,
commits ahead and behind, whether the worktree is clean, the conflicting
paths of a three-way tree merge, and the changed files of a tree diff
all came out as native git gives them, on a test repository and on this
one (0.25 s, wasmtime's start included). Two things had to change:

- **No mmap in WASI.** gitoxide maps object, pack, index and ref files
  with `memmap2`, which builds for the target and fails when run
  (`unsupported`). A stand-in `memmap2` that reads the file whole (and
  leaves the file's position where it was) made every step work. The
  workspace's `[patch]` cannot be the way in: the shell uses `memmap2`
  natively (through `fontdb`). It is to be a change to gitoxide that
  reads instead of mapping on `target_family = "wasm"`, or a fork of the
  crates that map.
- **No file modes in WASI.** Without permission bits, every executable
  file reads as changed; opening the repository with
  `core.fileMode=false` set makes the status match git's.

A worktree's `.git` names the repository's common directory, so the
extension's read access has to cover it; Horizon's worktrees are inside
the repository, so lending the project's root covers both.

### Where extensions meet

The agent is becoming an extension (an ACP client and its view). What
it offers other extensions is a WIT package of its own, not part of
`horizon:ui`:

```wit
// plugins/agent/wit/agent.wit
package horizon:agent@0.1.0;

/// Agent sessions, for another extension.
interface sessions {
    /// How a session begins.
    record begin {
        /// The directory it works in.
        directory: string,
        /// Its title, in the tab strip and the sessions list.
        title: string,
        /// What it is told first, as its owner's message.
        prompt: string,
    }

    /// Starts a session, shown in no pane, and comes back with its id
    /// (the one `horizon:ui/sessions` names it by) once it has taken the
    /// prompt.
    start: async func(begin: begin) -> result<string, string>;

    /// Sends `text` to the session `id` as its owner's message. Refused
    /// while the session is at work, so that nothing is cancelled.
    send: async func(id: string, text: string) -> result<_, string>;
}

world agent {
    include horizon:ui/imports;
    export horizon:ui/events;
    export sessions;
}
```

And the extension that uses it:

```wit
package horizon:tasks;

world tasks {
    include horizon:ui/imports;
    export horizon:ui/events;
    import horizon:agent/sessions@0.1.0;
}
```

- **The provider's WIT is the published contract.** It lives with the
  provider (`plugins/<name>/wit/`) under a package named for it, with a
  semver version; a user builds its bindings against it as a WIT
  dependency. What an extension provides is read from its component's
  exports, so nothing else declares it.
- **The host links them.** Each extension runs in a store of its own,
  driven by one future (`extension-boundary-design.md`, "Who drives").
  For an import of another extension's interface, the host defines the
  functions itself: a call is handed to the providing extension's
  driver, and its answer comes back to the caller. Values are copied
  between the two stores, so **a provided interface carries values
  only**: no resources, streams or futures, whose handles belong to one
  store. What changes over time is reported to the host's own
  interfaces (the agent's sessions reach `horizon:ui/sessions`), not
  watched across extensions.
- **A use is declared and checked.** The user names it in its `uses`
  (`"horizon:agent/sessions"`, or `".start"` of it), and the host checks
  it as it checks the host's own interfaces (`plugin_host.rs`, `may`).
- **When no extension provides it**, or none of a compatible version,
  the host still satisfies the import (a component's imports are not
  optional) with functions that answer
  `err("no extension provides horizon:agent/sessions")`. The task list
  works as a list then; starting a task says why it cannot.
- **When the provider is read again**, a call in flight answers with an
  error, and the next call reaches the new one.

How the agent extension itself reaches its ACP child (a process with
standard input and output, which WASI does not give) and how its
sessions reach `horizon:ui/sessions` are the agent extension's design;
this one needs only that they do.

### The one host addition: a terminal running a command

Running the check and moving a checked-out `main` need processes (the
check is the project's program, and a fast-forward of a checked-out
branch updates its files and index, which is git's porcelain). WASI has
no processes, and the owner should see the check run. Terminals are the
host's sessions, so the addition is generic, beside `open-terminal`:

```wit
interface session-control {
    // ...
    /// A new terminal in `directory` running `command` through the
    /// shell, in a pane beside the active one; comes back with its
    /// session's id.
    run-in-terminal: async func(directory: string, command: string) -> result<string, string>;
}
```

Integrate composes the command: a detached worktree at the commit to be
integrated, the project's check in it, then
`git -C <where main is checked out> merge --ff-only <commit>`. Nothing
reports the command's end to the extension: the extension sees `main`
reach the commit through the repository (watching its ref), which is
what "integrated" means. The commit is fixed when the command is made,
so `main` can only become the commit the check ran on. The project's
check is a setting of the project (where it lives is open).

`TerminalSpawnSpec` already takes a program and its arguments, so the
terminal runtime needs no change for this.

## What this does not do

- No threads, statuses, dependencies, read positions or roles.
- No agent writes a task, starts another task's session, merges or
  closes.
- No daemon: the files are the store.

## The existing board

It stays as it is while the extension is built. Once the extension
carries the loop, whether the board, its daemon, its CLI and its MCP
server are removed is decided then.

## Stages

Each ends with the gate clean and the extension tested through a pane.

1. **The task list**: files, writing, the phases `sessions` gives, open
   the session. Starting works once the agent extension provides
   `horizon:agent/sessions`; until then the host links it to the
   stand-in, and the linking itself is built and tested here with the
   fixture extension as provider.
2. **Branch state** in the guest: gitoxide reading instead of mapping on
   wasm, and the phases from it.
3. **Review**: the diff, and comments through `sessions.send`.
4. **Integrate**: `run-in-terminal`, and the composed command.

## Decisions to take on the board

1. A new extension for the task flow, the board left as it is until the
   extension carries the loop.
2. The session's pane as the conversation, with no thread.
3. Tasks as Markdown files under Horizon's data, written only by the
   owner.
4. Extensions offering interfaces to one another as WIT packages of
   their own, linked by the host, values only, with a stand-in where no
   provider is loaded; the agent extension's `horizon:agent/sessions`
   as the first.
5. Git read in the guest through WASI, with gitoxide changed to read
   instead of map on wasm.
6. `session-control.run-in-terminal` as the one host addition, and
   integration as a command composed from it.
