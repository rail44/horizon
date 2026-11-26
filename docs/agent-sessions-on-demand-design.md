# Board and wiki as ACP clients, and sessions hosted on demand

Status: designed. Board item #117 holds the decisions this design
follows.

## Goal

The agent knows nothing of the board or the wiki. The board and wiki
views are ordinary ACP clients: they start a session (`session/new`),
send it a prompt (`session/prompt`), and write the answer back where it
belongs themselves. Their tools reach a session as MCP servers the client
names at `session/new`, the standard ACP form. A role's prompt and
skills come in the client's first prompt.

With that knowledge gone, what one session needs of another is little,
and a session is hosted only while something needs it: a pane showing
it, or a client prompting it. Whatever process first takes the session's
own lock hosts it, and a process runs one session.

Order (#117): the board and the wiki move to the client side first; the
process per session follows.

The board's mechanism is to be made smaller later, after the shape of
other agents' tasks (#117). What is built for it now is what that will
stand on: a board view that starts a task's session, prompts it and
writes the answer back, and the board's tools as an MCP server. The
board's own operations beyond that are taken out of the agent and not
rebuilt.

## What the agent holds for them today

Read 2026-11-27 at `41678ed0`.

- **Composition.** `horizon-agent-acp` depends on `horizon-board-flow`
  and `horizon-wiki-flow` and composes both (`lib.rs:101-110`); its
  `Places` carries the logd socket only for the board (`lib.rs:38`), and
  `Composition.start_dir` exists only so the wiki of that project is
  watched (`lib.rs:78-80`).
- **The extension seam.** `horizon-agent-host`'s `Extension` trait
  (`extension.rs:16-36`) has four hooks; three of them (`trigger_sources`,
  `reply_sinks`, `board_host`) serve only the board and the wiki.
- **Board tools in the core.** `board_read`, `board_update`,
  `board_session`, `board_comment` are builtins of `horizon-agent`
  (`tools/catalog.rs:354-416`, `tools/board.rs`), offered to every
  session whose role has no allowlist, through the `BoardHost` seam.
- **`board_session`** (`horizon-board-flow/src/operations.rs`):
  `consult` binds a task to a session and starts it with role
  `board-task`; `implement` sends the caller `ActivateWorktree`;
  `review` makes a detached worktree at the tip and starts a
  `board-reviewer` session in it; `send` passes text to another session.
  Each queues `Event::SessionInputSent` with a `reply_to`.
- **The outbox** (`horizon-agent-host/src/outbox/`) tails every session
  log, delivers those sends as `Command::SessionInput`, and hands each
  answer (`InputOutcome` with its `reply_to`) to a sink: `BoardSink`
  posts a comment to the task's thread or relays a review to its
  caller, `WikiSink` appends the reply to the page's thread. The only
  trigger source is the wiki's: it polls the page logs of the start
  directory's project and prompts a session per thread, whose id is
  derived from the thread (UUID v5), starting or resuming it.
- **What exists only for these deliveries.** `SessionInput` and its
  queue in the rig session (`providers/rig/session/input.rs`),
  `InputAccepted` / `InputStarted` / `InputQueuePaused` / `InputOutcome`
  / `DeliveryAcknowledged` / `SessionInputSent`, `SendSessionInput` and
  `AcknowledgeDelivery` (no production sender), the reply addresses
  (`board`, `review-result`, `session`, `wiki`), `ExternalTool` (only the
  wiki registers one), `suspend_session` (only the wiki calls it), and
  the roles' `unattended` and `RoleWrites::InOwnWorktree` (only the board
  and wiki roles set them).
- **Startup** resumes every session its log leaves alive, board task and
  reviewer sessions among them, and one process holds the data home's
  lock (`startup.rs:96-120`), because the outbox and the sessions no
  window holds are the host's.

The shell, for its part: the board views are native plugins
(`src/board/`) that read and write the board store directly; they find
a task's session through `session/list` and attach to it, but cannot
start one or prompt it. The wiki view is a wasm extension
(`plugins/wiki`) whose host interface (`crates/horizon-ui/wit/plugin.wit`)
has no way to reach an agent at all. The agent never reads
`session/new`'s `mcpServers`, and rig (0.46) has no MCP client.

## The shape

### The agent

- No board or wiki crate, tool, role, skill, reply address or trigger.
  The outbox goes, and with it `SessionInput` and its queue and events,
  unless something other than the board and the wiki turns out to need
  them (to be checked: whether the rig session's input queue serves a
  plain `session/prompt`).
- **An MCP client.** At `session/new` (and `session/resume`), the agent
  connects to the MCP servers the client names and offers their tools
  beside its own, under the same approval policy as any tool. It
  advertises the transports it takes (`session.mcp.stdio`, and `http`
  if taken) in `initialize`. The Rust MCP SDK (`rmcp`) is the candidate.
- What the roles' `unattended` did, refusing instead of asking when no
  one can approve, is the client's: a client with no person behind it
  answers `session/request_permission` itself.

### The board

- **Tools as an MCP server.** `horizon mcp board`, a subcommand of the
  `horizon` CLI (which already reads and writes the board through logd),
  serves `board_read`, `board_comment`, `board_update` over stdio; the
  agent starts it for a session that names it in `session/new`'s
  `mcpServers` (#117).
- **Sessions the board starts.** The board's code in the shell becomes a
  client of the agent runtime: it starts a task's session, prompts it,
  takes the final answer of the turn, and posts it to the task's thread
  itself. The thread's "Send Board Message", which today writes the
  owner's comment and delivers it nowhere, does this.
- **Not rebuilt:** `board_session` (`consult`, `implement`, `review`,
  `send`), the `board-task` and `board-reviewer` roles and their skills,
  the reviewer's worktree and session, and the board's reply sink. They
  leave the agent; what the remade board needs in their place is
  decided then, on the base above. Where it needs a reviewer's detached
  worktree, the board makes it with git and names it as `cwd`
  (#117).

### The wiki

- **Tools as an MCP server**, `horizon mcp wiki` over stdio: `wiki_list`,
  `wiki_read`, `wiki_write`, `wiki_edit` over the project's wiki store.
- **The view prompts the thread's session.** The wiki extension already
  watches its thread logs; on an owner's comment it starts or resumes the
  thread's session, prompts it with the thread, and appends the answer
  to the thread. A wasm extension can do this only through a host
  interface it does not have yet: an `agent` interface in
  `plugin.wit` to start or resume a session, prompt it, and follow its
  updates. The same interface is what the agent view needs to become an
  extension too (the owner's direction of 2026-11-25 on #114).
- A thread's session is found again by an id the wiki records with the
  thread, since an ACP agent names its sessions.

### Roles

A role's prompt section and its skill pointers become text the client
sends first. What else a role set has no standard carrier, and becomes a
session config option the agent advertises, ACP's form for per-session
settings (#117):

- which of the agent's own tools a session has (the wiki thread is
  given only the wiki's MCP server, and the agent's builtin tools are
  narrowed by this option);
- writing nothing until the session is in its own worktree (what
  `RoleWrites::InOwnWorktree` did for the board's task sessions).

### Sessions on demand (after the move)

- Each session's log has a lock of its own; the data home's lock and the
  startup resume of every live session go. A process hosts a session
  when it takes that session's lock: when a pane opens it, or a client
  prompts it.
- The shell starts one `horizon-agent-acp` per session it hosts, so a
  new session runs the binary as it is when it starts.
- A session is open in the window whose process hosts it; another
  window says so instead of opening it.
- Before this step: the start time and memory of one session's process,
  measured; and whether two processes writing the knowledge store at
  once is safe.

## Stages

Each ends with the gate clean.

1. **The MCP client** in the agent, with a test MCP server.
2. **The board's tools** as `horizon mcp board`; the shell names it at
   `session/new`. The builtin board tools go.
3. **The board's sessions**: the board view starts and prompts a task's
   session and writes the answer to its thread. `board_session`, the
   board flow crate, its roles, skills and sink go.
4. **The wiki**: `horizon mcp wiki`, the `agent` interface in
   `plugin.wit`, the wiki extension prompting its threads; the wiki
   flow crate, its trigger and sink go.
5. **The outbox and the input queue** go, with the events and commands
   only they used.
6. **Sessions on demand**: per-session locks, no startup resume, one
   process per session.

## To settle in stage 3

How a client knows a turn's final answer. `session/prompt` is answered
at once today (`hub.rs:474-492`), so a client would read the last agent
message before the session goes idle. Whether v2's `session/prompt`
should be answered when the turn ends, with its stop reason, is read in
the ACP specification first and then decided (#117).
