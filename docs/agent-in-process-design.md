# Agent sessions in the shell's process

Status: proposed, not started. Board item #114 holds the decisions this
design follows.

## Goal

Horizon's own agent sessions run inside the shell's process, not in
`horizon-agentd`. The shell drives them through the library directly:
no socket, no ACP line, no extension version between the shell and its
agent. Separately, a small binary built from the same library speaks
standard ACP over stdio, one process per client, the way ACP clients
start agents (Zed spawns `claude-agent-acp` as its child, and the shell's
own client for external agents is built the same way). That binary is what a future TUI, a web front end or another
editor would use.

`horizon-terminald` and `horizon-logd` stay as they are.

## What changes for the person using Horizon

| | Today (agentd) | In the shell |
|---|---|---|
| Closing the window | Agent sessions go on; board and wiki sessions keep starting | Agent sessions stop; the next start resumes them from the event log, a turn in flight settled as interrupted |
| Agent code changed | `Reload Agent Runtime`; a wire bump needs a full app restart as well | An app restart. Terminals go on in `horizon-terminald` |
| `Reload Agent Runtime` | Restarts agentd | Gone |
| Trying the agent without the app | `horizon-agent-probe` starts a daemon of its own | The probe links the library, or drives the stdio binary |
| A crash in native agent code (DuckDB) | Ends agentd | Ends the window |

Panics in a session are caught per session today and still are: no
profile sets `panic = "abort"`.

## Where the code goes

`horizon-agentd` is a binary crate with no library: everything it does
beyond its socket is unreachable from another crate. Both the shell and
the stdio binary need that code, so it moves into a library crate,
`horizon-agent-host`:

- the host state (`AgentdState`, renamed), session spawn and the session
  thread (`spawn.rs`, `run.rs`, `setup.rs`, `environment.rs`, `events.rs`,
  `input.rs`, `model_selection.rs`, `subscription.rs`, `panic.rs`);
- the session's attachment stream and its lease (`session/attachment.rs`);
- resume (`session/resume*`), approvals and completion folding;
- the exploration host, the board host, board flow, wiki flow and the
  outbox;
- worktrees and checkpoints;
- `providers::agent_config` and `compose_packages`;
- fork and rewind (`Connection::handle_session_fork`/`_rewind`);
- the fold from session events to ACP updates (`hub/mapping.rs`).

What stays daemon-only, and goes with agentd: binding the socket first,
the accept loop, the `initialize` version gate, `_horizon/drain` and
its `_exit`, the SIGTERM flush hook.

Each session already runs on an OS thread of its own (its `LiveState`
and tool state are `Rc`-based and thread-local), so nothing in the
session loop needs a daemon or an async runtime.

## The seam to the views

The views already depend on one pair per session: a sender of
`AgentCommand` (prompt, cancel, approve, deny, continue, close) and a
receiver of `AgentUpdate` (`horizon-shell-runtime`'s `attachment.rs`),
handed to `AgentSession::attached` (`src/agent/session.rs`). The
in-process host hands the views the same pair, fed from the session's
attachment lease instead of the socket.

What flows through `AgentUpdate` stays ACP-shaped: the host runs the
same fold from session events to ACP v2 updates that agentd runs before
writing to the socket, in-process, and `agent_model` folds those as it
does now. One fold then serves the pane and the stdio binary, and the
shell's client for external ACP agents (stage E of
`docs/acp-agentd-design.md`, not started) can show them in the same pane.
The alternative, folding the agent's own events in `agent_model`, would
leave the stdio binary with a second fold to keep in step.

What the shell asks of the host directly, as calls instead of
`_horizon/*` messages: list sessions, start, attach, terminate, set the
model and the effort, list providers and their models, ensure the board
organizer. `workspace_snapshot`, the one host tool, stays a request from
the session thread to the UI thread, since the workspace model is the
UI thread's.

## What the host needs from the shell's process

- **An async runtime.** The outbox loop, attaching with a timeout,
  listing a provider's models, waiting for resume and the board's calls
  to `horizon-logd` are async. The shell has no process-wide runtime;
  the host owns one multi-thread tokio runtime of its own.
- **Startup off the UI thread.** Opening the event log, rebuilding the
  projection and resuming sessions run on the host's threads. The
  window opens at once; restored agent panes show `Restoring` until
  their session is back, which `AttachmentState` already has.
- **No process exit.** agentd calls `std::process::exit(1)` when the
  event log cannot be opened. The host returns the failure instead, and
  agent panes show it; terminals are unaffected.
- **A flush at exit.** agentd flushes the event log on SIGTERM and
  drain. The shell flushes the host's writer when the window closes and
  on SIGTERM.
- **The control plane's environment for `bash`.** agentd is started with
  `HORIZON_SOCKET` set to the shell's control socket, and its `bash`
  children inherit it. In the shell, the host sets it on each child
  explicitly, with `HORIZON_SESSION_ID` removed; the shell's own
  environment may name an outer Horizon.
- **Its directory.** Board and wiki flow are rooted at agentd's current
  directory, which is the directory of the shell that first started it.
  The host takes the directory as an argument.
- **Config reload.** `[sandbox]`, `[grants]` and trusted projects are
  picked up today by `Reload Agent Runtime`. `Reload Config` reaches the
  host instead (`AgentdState::reload_provider_config` already rebuilds
  the providers; nothing calls it). `config.example.toml` says provider
  settings reload live, which is not the case today; this makes it so.

## One owner of the data home

Today one agentd per user owns `agent-events.jsonl` and the DuckDB
projection, and a second Horizon shares that daemon. In the shell, two
Horizon processes, or the shell and the stdio binary, would each open
the same log: the JSONL writer takes no lock.

The host takes an exclusive lock on the data home when it opens the log.
A second process that cannot take it does not host agent sessions; how
it says so is open (below). The stdio binary takes the same lock, so it
runs against a data home of its own unless Horizon is closed.

## The stdio binary

`horizon-agent-acp`: reads ACP from stdin, writes to stdout, logs to
stderr, one client, ends when stdin closes. Sessions it starts are
recorded in its data home and resumed by `session/load`.

It speaks standard ACP, which agentd does not: agentd accepts only v2,
requires `_meta.horizon` ids on `session/new` and keeps only the text
of a prompt. The binary takes `session/new` with no extension data
(the session id is its own, the provider is the configured default),
and approvals are `session/request_permission`. What has no standard
form (the board, the organizer, continuing a paused turn, rewind) is
left out until a client needs it.

## Stages

Each stage ends with the gate clean.

1. **The library.** `horizon-agent-host` takes everything above but the
   daemon-only parts; `horizon-agentd` becomes its socket and hub over
   the library. No behaviour changes. agentd's unit tests move with
   their modules.
2. **The shell hosts.** The shell creates the host at start and hands
   the views the in-process pair. The agent half of
   `horizon-shell-runtime` (`agent.rs`, `AgentdHandle`, the agent routes),
   `src/agentd.rs`, `Reload Agent Runtime` and the version handshake go.
   `tests/agentd.rs` runs the pane over the in-process host. The data
   home lock lands here.
3. **agentd goes.** The binary, its e2e tests (rewritten over the
   library: of 32, the four about the socket itself go), the agent half
   of `horizon-daemon-testkit`, `acp-ext-wire.json` and its arm in
   `scripts/check-wire-schema.sh` (with a transition arm, as
   `agent-wire.json` had), and the `sandboxed` profile's entries. The
   probe links the library. AGENTS.md's agentd paragraphs and the docs
   that describe agentd as current are updated.
4. **The stdio binary.** `horizon-agent-acp` over the library, with an
   ACP client test that speaks to it over pipes.

## Open questions

1. **A second Horizon.** When the data home is held by another Horizon,
   does the second one show agent panes as unavailable, or refuse to
   start?
2. **The stdio binary's ACP version.** The stable protocol is v1, which
   external agents and clients speak; the shell's line to agentd is v2.
   The recommendation is v1 for the binary, since its clients are
   others'.
3. **When the stdio binary is made.** Stage 4 can wait until a client
   needs it; the library from stage 1 is what makes it cheap.

## What this replaces

`docs/agent-runtime-split-design.md` (2026-07-04) moved the agent into
agentd to change agent code without killing sessions, to make the agent
a reusable asset, to give delegated sessions a home, and to let sessions
survive a UI restart. Sessions survive a restart through the event log,
not the daemon; the reusable asset becomes the stdio binary; delegated
sessions live in the host wherever it runs; and sessions outliving the
window is not wanted (#114). `docs/acp-agentd-design.md` made ACP the
line between the shell and agentd; that line goes, and ACP stays as the
stdio binary's protocol and the shell's client for external agents.
