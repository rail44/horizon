# The agent as a stdio ACP child of the shell

Status: stage 1 implemented (the library and the two flow crates);
stages 2-4 not started. Board item #114 holds the decisions this design
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

The shell still compares `HORIZON_ACP_EXT_VERSION` in `initialize`'s
`_meta.horizon`, since the binary can be rebuilt on its own. On a
mismatch the agent panes say the app needs a restart; there is no drain
and respawn loop, since the child is the shell's own. Every `_horizon/*`
extension the shell gives up narrows what a mismatch can break.

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
- **Config reload.** `Reload Config` sends the child a request to reload
  `[providers]`, `[sandbox]`, `[grants]` and trusted projects
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
the data home. The process hosting a session holds an exclusive lock on
its file while the session is live; a second process asked to host a
session whose file is locked refuses with that reason. Two windows, the
probe and a TUI can then share a data home, each owning the sessions it
hosts. Listing sessions reads each file's head and tail; resuming one
reads its file alone; a session started by a fork or a task names its
parent, as its events do now. The data home's path is an argument of the
library, not read from the environment inside it.

The existing log is split into session files once, by a migration the
binary runs when it finds `agent-events.jsonl` and no `sessions/`.

### The DuckDB projection

The projection is one database across every session. It serves two
readers: resume in its incremental mode, which per-session files make
unnecessary, and `recall_search`/`recall_read` with `scope: "all"`,
which search every session's history. DuckDB lets one process at a
time write a database file, so a shared projection needs one writer.
Which form recall takes is open (below).

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

## Stages

Each stage ends with the gate clean.

1. **The library.** `horizon-agent-host` takes agentd's code and its ACP
   side; board flow and wiki flow move into crates of their own.
   agentd becomes its socket over the library. No behaviour changes;
   agentd's unit tests move with their modules.
2. **One log file per session.** The writer, the readers, resume and
   the migration; the projection's place settled with the recall
   question.
3. **The stdio child.** `horizon-agent-acp`; the shell starts it in
   place of connecting to agentd. `Reload Agent Runtime` restarts the
   child; `Reload Config` reaches it. Tests move to `Channel::duplex()`.
   The probe becomes a client of the binary.
4. **agentd goes.** The binary, the agent half of
   `horizon-daemon-testkit`, the socket helpers only agentd used, the
   `sandboxed` profile's agentd entries (with the stale `milestone::git`
   and `isolation_announcement` ones), AGENTS.md's agentd paragraphs,
   and the docs that describe agentd as current.

## Open questions

1. **Recall across sessions.** Either DuckDB reads the session files
   directly at query time (`read_json` over `sessions/*.jsonl`), with no
   database kept; or a database kept as an index, opened for writing
   only briefly by whichever process appends. Which one depends on the
   queries recall runs and how long they take over the whole history,
   which have not been measured.
2. **The ACP version.** The shell speaks v2 to agentd; external agents
   and clients speak v1, the stable version. The binary answers the
   version a client asks for in `initialize`, if the parts of v2 the
   shell uses have a v1 form; which do is not yet listed.
3. **The `_horizon/*` extensions the shell keeps.** Each is a place the
   shell and the binary can disagree. Listing providers and their
   models, the board organizer and continuing a paused turn are the
   ones in use.

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
