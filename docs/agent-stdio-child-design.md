# The agent as a stdio ACP child of the shell

Status: stages 1 to 4 implemented (the library and the two flow crates;
one log file per session and the projection caught up on demand; the
stdio child, with agentd gone, its crate now `horizon-agent-acp`); stage
5 not started. The end-to-end tests still start the binary: moving the
ones not about the process to `Channel::duplex()` is not done yet. Board item #114 holds the decisions this design
follows.

## Goal

The shell draws ACP agents; it does not host one. Horizon's own agent
becomes an ordinary ACP agent: a binary, `horizon-agent-acp`, that
speaks ACP over stdin and stdout, which the shell starts as its child
the way ACP clients start agents (Zed starts `claude-agent-acp` as its
child, and the shell's own client for external agents,
`docs/acp-agentd-design.md` stage E, is built the same way). The same
binary is what a future TUI, a web front end or another editor would
start.

`horizon-agentd`, the daemon that hosts every agent session behind a
socket today, goes. `horizon-terminald` and `horizon-logd` stay as they
are.

## What changes for the person using Horizon

| | Today (agentd) | The stdio child |
|---|---|---|
| Closing the window | Agent sessions go on; board and wiki sessions keep starting | The child ends with the window; the next start resumes the sessions from their logs, a turn in flight settled as interrupted |
| Two windows | Share one daemon | Each starts a child of its own |
| Agent code changed | `Reload Agent Runtime` drains and respawns the daemon | `Reload Agent Runtime` closes the child's stdin and starts the rebuilt binary; panes re-attach |
| A changed `_horizon/*` extension | A full app restart | Still a full app restart: the shell's side changed too |
| Trying the agent without the app | `horizon-agent-probe` starts a daemon of its own | Start the binary and speak ACP to it |
| A crash in native agent code (DuckDB) | Ends agentd | Ends the child; the window and terminals go on |

## The shape

```
shell (ACP client: draws, sends prompts and approvals)
  │ stdio, ACP
  ▼
horizon-agent-acp (one per window)
  └─ horizon-agent-host (library): sessions, resume, approvals,
       worktrees, checkpoints, the fold to ACP, the outbox
         ├─ horizon-agent (the agent itself)
         └─ board flow, wiki flow (crates of their own, composed in)
```

### The library is an ACP agent

`horizon-agentd` is a binary crate with no library: everything it does
beyond its socket is unreachable from another crate. That code moves
into a library crate, `horizon-agent-host`:

- the host state (`AgentdState`, renamed), session spawn and the session
  thread (`spawn.rs`, `run.rs`, `setup.rs`, `environment.rs`, `events.rs`,
  `input.rs`, `model_selection.rs`, `subscription.rs`, `panic.rs`);
- the session's attachment stream and its lease (`session/attachment.rs`);
- resume, approvals and completion folding;
- the exploration host, the board host and the outbox;
- worktrees and checkpoints;
- fork and rewind;
- `providers::agent_config`;
- the ACP side of the hub: the handlers for each ACP method, and the
  fold from a session's own events to ACP updates (`hub/mapping.rs`).

The fold belongs to the agent. A session emits Horizon's own events
(`AgentWireEvent`: a message committed, a tool call requested or
finished, a state change, an approval requested); ACP is what the shell
reads. Turning one into the other is how the agent speaks ACP, so it
lives in the library, and nothing outside the library sees the agent's
own events. The library exposes one ACP agent, through
`agent-client-protocol`'s `ConnectTo<Client>`, that any ACP connection
can be attached to.

What goes with agentd: binding the socket first, the accept loop, the
`initialize` version gate as a gate for a shared daemon, `_horizon/drain`
and its `_exit`, the SIGTERM flush hook.

Each session already runs on an OS thread of its own (its `LiveState`
and tool state are `Rc`-based and thread-local), so nothing in the
session loop changes.

### Board and wiki flow are composed in, not part of the core

Board flow and wiki flow (the triggers that start sessions from board
and wiki writes, the replies they write back, the roles, skills and
tools they add) move into crates of their own. They already plug in
through seams: the outbox's `TriggerSource` and `ReplySink`, the
`Packages` builder, and the `BoardHost` tool seam. The core keeps the
outbox and the traits; the binary composes the two flows in.

Loading them at run time, as the shell loads its wasm extensions, is a
later step this design leaves room for: the core does not name the
board or the wiki.

### The shell is an ACP client

The shell's ACP client stays: `agent_model` folds ACP updates, the
views draw them, and `AgentCommand`/`AgentUpdate` are what a pane holds.
What changes in `horizon-shell-runtime`'s `agent.rs` is how it reaches
the agent: it starts `horizon-agent-acp` (from beside its own
executable, as it finds the daemons today) with piped stdio, instead of
connecting to a socket or spawning a daemon. The one host tool,
`workspace_snapshot`, stays an `_horizon/host_tool` request answered on
the UI thread.

While `_horizon/*` extensions remain, the shell compares
`HORIZON_ACP_EXT_VERSION` in `initialize`'s `_meta.horizon`, since the
binary can be rebuilt on its own. On a mismatch the agent panes say the
app needs a restart; there is no drain and respawn loop, since the child
is the shell's own. Once the extensions are replaced by standard forms
(below), the check goes with them and ACP's own versioning is all.

### What the child needs that agentd had

- **Its directory and environment.** Board and wiki flow are rooted at
  agentd's current directory. The child is started in the directory the
  shell names and takes it as an argument. Its environment carries
  `HORIZON_SOCKET`, the shell's control socket, as agentd's does, and no
  `HORIZON_SESSION_ID`.
- **No process exit on a bad log.** agentd exits when the event log
  cannot be opened. The child answers `initialize` and refuses sessions
  with the error, which the panes show.
- **A flush when stdin closes**, as agentd flushes on drain and SIGTERM.
- **Config reload.** The child reads the config file again when it
  changes: `[providers]`, `[sandbox]`, `[grants]` and trusted projects
  (`AgentdState::reload_provider_config` rebuilds the providers today;
  nothing calls it). `config.example.toml` already says provider
  settings reload live, which this makes true.
- **Startup off the UI thread.** The shell starts the child and
  attaches restored panes off the UI thread; a pane shows `Restoring`
  until its session is back.

## One log file per session

Every session of every process writes one file today,
`agent-events.jsonl` in the data home. That is why the probe needs a
data home of its own, why reading one session's history means
filtering everyone's, why a full projection rebuild reads every session
there ever was (563 MB in October; 749 MB moved aside on 2026-11-24),
and why two processes on one data home would write the same file.

Each session gets a file of its own, `sessions/<session-id>.jsonl` under
the data home. One process hosts a data home's sessions at a time: it
holds a lock on `sessions.lock` beside the directory for as long as it
runs, and a second process is refused at startup (board #114: the outbox
and the sessions no window holds are the host's, so two hosts would both
act on them). One writer thread appends each record to its session's
file, and a record's `sequence` counts within its session. Where records
of different sessions are ordered (the outbox's deliveries, the latest
session of a role), their time orders them, then the session, then the
sequence.

Startup reads each file backwards only as far as its latest state and
its latest context (`session_summary`), and reads the whole file only for
a session it resumes. A fork's history is its branch: its own file, and
the file of the session it was forked from (named in its context) up to
the record it was forked at.

The shared log is split into session files once, at the first start
that finds `agent-events.jsonl`: its lines are copied as they are into a
directory beside `sessions/` that then takes its name, and the log is
moved aside as `agent-events.jsonl.split-<ms>`. A start that finds the log
and a `sessions/` already holding sessions was stopped between the two,
and only moves it aside. A log of another version is moved aside unsplit,
as a format change always was.

### The DuckDB projection

The projection (`agent-state.duckdb` in the data home) is one database
across every session. Today agentd's writer thread holds it open for
writing for the life of the process and projects every append. Its
readers are resume in its incremental mode and reading one session's
records, which per-session files make a read of that file, and the
recall tools, which search and list every session's history.

DuckDB lets a process that holds a database open for writing exclude
every other, readers too (measured 2026-11-25: a read-only open of the
live file failed with "Conflicting lock is held in horizon-agentd").
With no writer, several processes open it read-only at once.

So no process holds it open. It is caught up when recall needs it:

1. The process running recall opens it for writing. If another process
   holds it, it waits until that one closes it.
2. It reads each session file from where the projection last stopped
   (a cursor per file, kept in the database) and appends what is new,
   then closes it.
3. It opens it again read-only, waiting the same way if a writer holds
   it, runs the query and closes it.

The host also catches it up once at startup, after resuming the
sessions, on a thread of its own: a rebuild (the first start after the
split, or a new projection format) took 89 s over the 25,129 records of
2026-11-24 to 2026-11-25, and that must not land on a recall.

Recall is rare (15 of 20,047 tool calls from 2026-08-03 to 2026-11-25,
3 across every session). Measured on a copy of four months' projection
(1 GB, 2026-08-03 to 2026-11-24): opening and closing it read-only
takes about 53 ms, a search across every session 170-370 ms, listing
sessions 15-20 ms.

## Tests

Most of what agentd's tests check is how the agent behaves over ACP, not
the process boundary. Those tests create the library's agent in the
test's own process and connect a client to it with
`agent-client-protocol`'s `Channel::duplex()`: the same ACP messages,
with no binary to build first and no process to spawn, so they also run
under the `sandboxed` nextest profile. That covers most of the 32 in
`crates/horizon-agentd/tests/e2e.rs` and the agent pane's test in
`tests/agentd.rs`, which connects the shell's own client.

The tests about the binary itself start it: it answers `initialize`
over stdio, it flushes and ends when stdin closes, a second process is
refused a locked session.

## ACP and its extensions

The binary speaks ACP v2. The shell relies on `StateUpdate` (running,
idle, waiting for an approval), which v1 has no form of: in v1 a client
learns a turn ended only from its own `session/prompt`'s response, and
Horizon's sessions run turns no client started (board and wiki inputs,
other sessions' inputs, a child's completion). A v1 entry for editors
that speak only v1 can be added when one is wanted, with that state left
out.

The `_horizon/*` extensions are replaced by standard forms, and the UI
follows ACP where they differ (board #114):

| Extension | Standard form |
|---|---|
| `continue_turn` | A turn the token budget halts ends with the stop reason `MaxTurnRequests`; a new message goes on, as it already does |
| `list_providers`, `list_provider_models` | The `Model` config option's values, before a session exists through `session/options` (unstable) |
| `session/new`'s `_meta.horizon` | The agent names the session; the role is a `Mode` config option and the model a `Model` one, given at creation (`config_options`, unstable) |
| A denial's reason in the permission response | A standard reject option; the reason is the next message |
| `ensure_board_organizer` | Out of ACP: board flow writes the organizer's session id to the board, whose view resumes it |
| `reload_provider_config` | Out of ACP: the child reads the config file again when it changes |
| `task_progress` | `subagent_update`: the child's description is its title, its state is `running`, `requires_action` while it waits for an approval, `idle` when it is done; what it is doing and when it started are not sent |
| `tool_call_progress` | Gone: only the mock provider sent it; rig gives a tool call whole |
| `host_tool` (`workspace_snapshot`) | Out of ACP: the tool asks the control socket (`HORIZON_SOCKET`) itself |
| `session_event` | An error is the stop reason `Error`; resumption, a closed attachment, an exit and skipped lines have no use over a child's stdio; rate limits and history clearing are not shown |
| `drain`, `watch_board`, `rewind_session` | Gone |

The `_meta.horizon` on updates goes too: a tool call's verdict is in its
`rawOutput` and its outcome in its `status`; an approval's kind is in
its options' wording; where an input came from is not marked; a child
session's parent is not shown (#114).

## Stages

Each stage ends with the gate clean.

1. **The library.** `horizon-agent-host` takes agentd's code and its ACP
   side; board flow and wiki flow move into crates of their own.
   agentd becomes its socket over the library. No behaviour changes;
   agentd's unit tests move with their modules.
2. **One log file per session.** The writer, the readers, resume, the
   projection caught up on demand, and the migration.
3. **The stdio child.** `horizon-agent-acp`; the shell starts it in
   place of connecting to agentd, over the extensions it uses now, less
   `drain`, `watch_board` and `rewind_session`. `Reload Agent Runtime`
   restarts the child. Tests move to `Channel::duplex()`. The probe
   becomes a client of the binary.
4. **agentd goes.** The binary, the agent half of
   `horizon-daemon-testkit`, the socket helpers only agentd used, the
   `sandboxed` profile's agentd entries (with the stale `milestone::git`
   and `isolation_announcement` ones), AGENTS.md's agentd paragraphs,
   and the docs that describe agentd as current.
5. **Standard forms.** The extensions in the table above, one at a time,
   each with the UI it changes; the extension version goes with the
   last.

## What this replaces

`docs/agent-runtime-split-design.md` (2026-07-04) moved the agent into
agentd to change agent code without killing sessions, to make the agent
a reusable asset, to give delegated sessions a home, and to let sessions
survive a UI restart. Sessions survive a restart through their logs;
agent code changes by restarting the child; the asset is the stdio
binary; delegated sessions live in the child; and sessions outliving
the window are not wanted (#114). `docs/acp-agentd-design.md` made ACP
the line between the shell and agentd; ACP stays the line, now to a
child over stdio.
