# Agent Runtime Split (horizon-agentd)

Decision record and implementation guide, agreed 2026-07-04. Motivation:
change agent code without killing sessions (the daily-driver requirement),
make the agent mechanism a reusable asset, give delegated agent sessions a
home, and — as a direct payoff of the log-ownership decision below — let
agent sessions survive UI restarts before any terminal daemon exists. See
`docs/trust-boundaries.md` for the tier framing.

## The five decisions

**1. One agent-host process, not one per session.** `horizon-agentd` hosts
every agent session, multiplexed by session id. It is the embryo of the
long-term tmux-style session daemon — when that lands, this process grows
PTY ownership rather than being rewritten. One replay path, one supervisor.
Per-session crash isolation is not worth N processes: sessions are
event-sourced, so a crash loses at most the in-flight turn.

**2. Tools execute in the child.** fs and bash move with the session loop
(this also removes the fs.grep/glob UI-thread jank and the thread-local
bash registry from the UI process). Approval *decisions* stay with the
human in Horizon: approval-request events flow out, approve/deny commands
flow back, unchanged. Horizon-coupled tools (`workspace.snapshot`, future
UI-state tools) become **host tools**: the child asks the client to run
them over the same connection (see guardrail 4).

**3. The child owns the event log and the DuckDB projection.** The state
owner is the log owner. Horizon becomes a rendering client that folds
received events into frames. Consequences: agentd restart = read own log,
rebuild `rig_history`, mark turns that died mid-flight as cancelled
(cancellation is already a stop reason, not an error); Horizon restart =
reconnect and re-bootstrap from the child (agent sessions survive).

**4. Unix socket, JSONL envelopes, transport-agnostic framing.** Envelope:
`{v, kind, session_id?, payload}` where kind is command | event | control.
Control messages: `hello` (contract version, binary id, capability list),
`session_list`, `session_new` (carries per-session config overrides),
`session_load`, `host_tool_request`/`host_tool_response`, `ping`,
`drain`. Startup is spawn-or-connect: Horizon connects if the socket
exists, spawns agentd first if not. Hot reload is an explicit command
(`Reload Agent Runtime` as a `CommandId`): drain → agentd flushes and
exits → Horizon spawns the rebuilt binary → reconnect → `session_load`.

**5. Four landing steps, each gated.**
1. Crate split: `crates/horizon-agent` (contract, providers, tools,
   persistence — no floem, no horizon types); `horizon` keeps views,
   live signal state, and the `workspace.snapshot` host tool. The
   boundary becomes compiler-enforced.
2. `horizon-agentd` bin + socket + hello handshake, sessions still
   in-process behind a switch.
3. Move tool execution and the log writer into the child; host-tool
   channel for Horizon-coupled tools.
4. Replay, reconnect, and the reload command; retire the in-process path
   (contract-level tests stay in the lib crate; e2e tests speak the
   socket).

After step 1, iterating on agent code rebuilds only the agent crates.

## Replay and reconnect

- **agentd start**: read the log → per session, rebuild provider history
  via the existing mapping → any turn open at the log's tail is committed
  as cancelled → ready.
- **Horizon connect**: `hello` exchange; a contract-version mismatch is
  surfaced to the user as "reload required", never silently ignored →
  `session_list` → `session_load` per attached pane. v1 bootstrap: agentd
  re-emits the fold-relevant committed events for that session (bounded by
  log size; a server-side frame snapshot is a later optimization).
- **Turn end** becomes an explicit contract event carrying a stop reason
  (completed / cancelled / failed / halted-by-guard) instead of being
  implied by state transitions — needed for clean bootstrap semantics and
  ACP mapping, and it makes log forensics easier.

## ACP compatibility guardrails

Adopting the Agent Client Protocol later (either direction: Horizon as ACP
client hosting external agents, or agentd speaking ACP to other frontends)
must stay a bindings problem, not a redesign. Six rules keep it that way:

1. **Contract ≠ wire.** `Command`/`Event` stay transport-free; the JSONL
   envelope lives in a thin wire module. An ACP adapter is a second
   binding beside it, translating JSON-RPC ⟷ contract.
2. **Framing over any stream.** The wire layer takes a generic async
   read/write pair. ACP is JSON-RPC over stdio (client spawns agent);
   `horizon-agentd --acp` on stdio must be a configuration, not a fork.
3. **Explicit turn end + stop reason** (above). ACP's `session/prompt`
   response carries a stop reason; ours must be derivable, not inferred
   from state churn.
4. **Host tools are a negotiated client capability, not a hack.** ACP's
   core shape is the agent asking the client for fs/terminal capabilities.
   Our host-tool channel is that shape; tool implementations stay behind
   the catalog so a session can route fs through the client when a future
   client (e.g. an editor with unsaved buffers) demands it.
5. **`session_new` ≠ `session_load`,** and `session_new` carries
   per-session config (model, base_url, …). ACP separates creation from
   restoration and passes per-session context; a global-config-only
   assumption would foreclose that.
6. **Keep a mapping table, not an implementation.** Event ⟷
   `session/update` variants, ApprovalRequested ⟷
   `session/request_permission`, Cancel ⟷ `session/cancel`. Our extras
   (provider-request lifecycle) fold away in an adapter; ACP extras (plan
   updates) are future contract extensions. The table's job is catching
   contradictions early.

## Out of scope here

Terminals in the daemon (the long-term shape; this split must not preclude
it — the single host, the socket, and the client-capability channel are
the compatibility guarantees), MCP, multiple simultaneous clients.

## Step 1 implementation notes

Landed as `crates/horizon-agent` (a `[workspace]` member; `horizon` depends
on it by path). Boundary decisions the design above didn't fully pin down:

- **`HostTools` trait** (`horizon_agent::tools::HostTools`, one method:
  `execute_auto(tool_id, input) -> Option<Value>`) is the tool-catalog seam.
  `execute_agent_tool`/`process_agent_provider_event` take `&dyn HostTools`
  instead of `&Workspace`; Horizon implements it in `agent::host_tools`
  (`WorkspaceHostTools`, wrapping `&Workspace`) and passes it in at the one
  call site (`app/runtime/agent.rs`). `workspace_snapshot` itself (the
  function that reads `Workspace`) moved to `agent::host_tools`, tested
  there as a horizon-side integration test of the seam.
- **Config**: the crate mirrors the `[agent]`/`[provider]` subset of
  Horizon's config schema as `config::AgentFileConfig` (plain data, no
  serde-from-file — Horizon still owns parsing). Horizon converts its
  `RawConfig` into this shape in `agent::mod`'s
  `agent_file_config_from_raw`, the one seam-crossing point. The crate's
  `*_::from_env()` convenience wrappers (which used to call Horizon's
  `crate::config::load()`) were removed; callers now resolve
  `AgentFileConfig` first and pass it to `*_::from_env_and_file`/
  `AgentConfig::from_env_and_file`. `ToolSessionState::for_current_dir` and
  `pane_status_tick_secs` gained an explicit config parameter for the same
  reason. agentd's own config loading (step 2) still needs its own answer —
  parse straight into `AgentFileConfig`, or something richer.
- **SessionId**: the crate defines `contract::SessionId` (a `Uuid` newtype,
  serde-transparent). `agent::mod` in Horizon holds the `From` impls both
  ways, round-tripping through `Uuid` (`SessionId::as_uuid`/`from_uuid` on
  both types, added to Horizon's `session::SessionId` too). Every call from
  Horizon into the crate converts with `.into()` at the call site.
- **Cross-crate `cfg(test)`**: a downstream crate's test build can't trigger
  an upstream crate's `cfg(test)`, so a few items the crate previously
  gated as test-only had to become real API because Horizon's *own* tests
  need them: `persistence::projection::duckdb::{Store::sessions,
  AgentStoredSession}`, `persistence::event_log::WriterHandle::same_channel`.
  Everything else that stayed test-only in the crate (most of the DuckDB
  query surface, `AppendEvent`, etc.) is only ever exercised by the crate's
  own `tests.rs`/inline test modules, which moved with the code.
- Visibility inside the crate: former `pub(crate)` (crate-wide in the old
  single-crate `horizon`) became `pub` only where Horizon or the new
  crate-external boundary actually needs it (contract types, provider
  registry, tool/persistence entry points, config types); purely
  crate-internal pieces (`MockProvider`, the rig `Provider` struct,
  `policy`, `providers`) stayed `pub(crate)`.

## Step 2 implementation notes

Landed as `horizon_agent::wire` (framing), `horizon_agent::socket` (the
default socket path), and a new workspace member `crates/horizon-agentd`
(the daemon binary). Sessions still run entirely in-process inside Horizon —
`horizon-agentd` answers only the connection-global control messages this
step calls for; nothing routes a real session through it yet.

- **Envelope and `CONTRACT_VERSION`**: `wire::Envelope { v, session_id,
  body: EnvelopeBody }`, `EnvelopeBody::{Command, Event, Control}`
  (adjacently tagged as `{"kind":..,"payload":..}`). `wire::CONTRACT_VERSION:
  u32 = 1` lives in `wire`, not `contract` — it stamps every envelope's `v`
  field *and* is echoed by `Hello::contract_version`, but the two checks are
  independent (see next bullet), which only made sense to keep next to the
  framing code that performs the structural one. Deserializing is
  hand-rolled (`wire::parse_line`) rather than derived, so an unrecognized
  `kind` or a `v` this build doesn't speak return `WireError::UnknownKind`/
  `WireError::VersionMismatch` instead of a generic serde error — both
  non-panicking, per the deliverable. Unknown top-level JSON fields are
  tolerated for free (no `deny_unknown_fields` anywhere in the envelope
  types).
- **Two version checks, not one.** `wire::read_envelope` rejects a
  structurally-incompatible envelope (`v != CONTRACT_VERSION`) before it
  even looks at `kind`/`payload` — this is `wire`'s own job, since decoding
  `payload` into today's `Command`/`Event`/`Control` shapes assumes today's
  envelope schema. Separately, `Hello::contract_version` is compared by
  *handshake* logic (`horizon-agentd`'s connection handler, `horizon`'s
  `agent::agentd_client::handshake`) — deliberately not folded into `wire`,
  because a transport without an envelope at all (ACP's JSON-RPC over
  stdio, guardrail 2) has no `v` field to check, so the semantic version has
  to travel inside the payload independently of the wire format.
- **`Control::HandshakeRejected(String)` — a deviation from the design's
  literal control-message list.** The design names `hello`, `session_list`,
  `session_new`, `session_load`, `host_tool_request`/`_response`, `ping`,
  `drain` but no explicit rejection/error payload. Step 2 needed one:
  `horizon-agentd` must make a `contract_version` mismatch observable on the
  wire (not just close the socket, which is a weak, easily-misread signal)
  so both the real client (`agent::agentd_client`) and the e2e test can
  assert on a concrete reason string. `horizon-agentd` sends this instead of
  a normal `hello` reply and then closes the connection; it does not
  validate a client's claimed version any further than that one check.
- **`horizon_agent::socket::default_socket_path`** (`$XDG_RUNTIME_DIR/
  horizon/agentd.sock`, else `/tmp/horizon-agentd-$UID.sock`) is shared
  between `horizon-agentd` (bind default) and Horizon's `agentd_client`
  (connect default) specifically so the two independently agree on the same
  path without either depending on the other. Kept out of `wire` (which
  must stay transport-agnostic per guardrail 2) as its own tiny module.
- **`horizon-agentd`** (`crates/horizon-agentd`, bin-only crate, package
  name doubles as the binary name so `env!("CARGO_BIN_EXE_horizon-agentd")`
  works from its own integration test). `--socket <path>` overrides the
  default. Accepts one connection at a time by construction (the accept
  loop `await`s a connection's full handler before accepting the next —
  multi-client is out of scope per the design). Stale-socket recovery: if
  the path exists but connecting to it fails, removes it and rebinds; if
  something *is* accepting, refuses to steal the path. `SIGTERM` breaks the
  accept loop, removes the socket file, and returns normally. `drain`
  flushes the write half and calls `std::process::exit(0)` immediately
  (mid-connection, not just closing the one connection) per the design's
  "drain → agentd flushes and exits" wording.
- **`horizon_agent::config::load_file_config`**: a minimal `toml`+`serde`
  loader added to `horizon-agent` for `AgentFileConfig`, duplicating
  Horizon's `HORIZON_CONFIG` > `$XDG_CONFIG_HOME/horizon/config.toml` >
  `~/.config/horizon/config.toml` > built-in-default resolution verbatim
  (own copies of the same env var names/logic — this crate still can't
  depend on `horizon`'s loader). Parses Horizon's *actual* config file:
  `AgentFileConfig`'s `#[serde(default)]` (no `deny_unknown_fields`) means
  the file's other sections (`[terminal]`, `[ui]`, `[keybindings]`,
  `[theme]`) parse fine and are silently ignored, so `horizon-agentd` only
  ever sees `[agent]`/`[provider]`. No `[agentd]`-specific section exists —
  see the next bullet for where that switch actually lives.
- **Horizon-side gating**: `[agent].agentd` (bool, default `false`) in
  Horizon's own `RawAgentConfig` — this is a Horizon-only "should Horizon
  try to talk to agentd at all" switch, not part of the contract/file
  schema `horizon-agent` mirrors, so it was added to `crate::config`
  directly rather than threaded through `AgentFileConfig`.
  `agent::agentd_client::agentd_enabled()` reads it;
  `agent::agentd_client::maybe_connect_at_startup()` is the one production
  call site (`app::view::app_view`, right after `spawn_initial_sessions`) —
  a no-op when the flag is `false` (the default, so default behavior is
  unchanged), and otherwise a fire-and-forget connection attempt on a
  background OS thread (Horizon has no tokio runtime of its own; floem
  drives its own event loop) that only logs the outcome. No session routes
  through this connection yet — that is step 4.
- **Testing scope / a gap worth naming**: `horizon-agent`'s `wire` tests
  round-trip every envelope kind (including every `Control` variant) over
  `tokio::io::duplex`, plus torn-line, unknown-kind, wire-version-mismatch,
  and unknown-field-tolerance cases. `horizon-agentd`'s `tests/e2e.rs` spawns
  the real binary (`CARGO_BIN_EXE_horizon-agentd`, available because the
  test lives in the same package as the bin target) and drives
  hello/ping/session_list/drain plus the `HandshakeRejected` path over the
  real socket. Horizon's `agent::agentd_client` tests exercise the
  `handshake` function itself (generic over `AsyncRead + AsyncWrite`, so
  testable over `tokio::io::duplex` against an in-test fake peer) for the
  success, version-mismatch, rejection, and closed-before-reply cases —
  but *not* the actual `connect_or_spawn` subprocess-spawn path, since
  `CARGO_BIN_EXE_horizon-agentd` is only set for `horizon-agentd`'s own
  integration tests, not for a different package's (`horizon`'s) test
  binary. `spawn_agentd`/`retry_connect` are implemented per the design and
  reachable from `maybe_connect_at_startup`, but only manually verified
  (run Horizon with `[agent].agentd = true` and no socket present); a
  cross-package integration test for the cold-start spawn path is left for
  a later step if it proves worth the setup.

## Step 3 implementation notes

Landed: `horizon-agentd` hosts real sessions (`crates/horizon-agentd/src/
session.rs`), tools (including `bash`) execute there, and it owns the event
log + DuckDB projection. Horizon's `spawn_agent_session` routes to a live
`horizon-agentd` connection when one exists; `agent::agentd_client::
maybe_connect_at_startup` was replaced by a blocking startup connect
(`connect_agentd_at_startup`) so that connection is ready before the first
session might need it. Default (`[agent].agentd = false`) behavior is
unchanged — every new code path added this step is reached only through
`Option<AgentdConnection>` being `Some`, which it never is by default.

- **One dedicated OS thread per session in `horizon-agentd`, not an async
  task.** `LiveState`/`ToolSessionState` are `Rc`-based and `tools::state::
  SESSION_RUNTIMES` is a `thread_local!` — both assume everything for one
  session runs on a single, consistent thread, the role Horizon's floem UI
  thread played in-process. `session::run_session` reproduces that: it
  registers the session's runtime and processes every provider event / bash
  completion / inbound command in a single synchronous loop
  (`crossbeam_channel::select!`) on its own thread, so a later `approve`/
  `deny` command's `resolve_approval` call finds the same thread-local entry
  `register_session_runtime` put there. This also makes the host-tool round
  trip trivial: `AgentdHostTools::execute_auto` really does block this one
  thread on a channel recv while Horizon answers over the wire (`host_tool_
  request`/`_response`, matched by a `request_id` -> reply-channel map) —
  harmless because nothing else needs this thread, but would deadlock a
  single-threaded async runtime.
- **`process_agent_provider_event` reused as-is, unchanged.** agentd's
  `session::handle_provider_event` calls the exact function `app::runtime::
  agent`'s in-process effect used to call, passing `AgentdHostTools` where
  Horizon passed `WorkspaceHostTools` — the `HostTools` seam from step 1 is
  exactly what made tool execution relocatable without touching the tool
  catalog. The resulting `Processing::horizon_events` are folded into
  agentd's own `LiveState` (persisting them) and — filtered to drop items
  carrying ephemeral `tool_call_progress` — forwarded to Horizon as ordinary
  event envelopes.
- **`ApprovalOutcome::Executed`/`Started` gained an `events: Vec<Event>`
  field** (`crates/horizon-agent/src/tools/approval.rs`) — the one crate API
  change this step needed. Both variants already carried the resulting
  whole `AgentFrame`, which is what Horizon's in-process pane click handler
  (`app::command_actions::resolve_and_send_approval`) wants (a whole-frame
  replace via `Frames::update_agent_frame`); agentd instead needs the
  discrete events that produced that frame, to forward as wire event
  envelopes. Additive only — Horizon's existing match arms picked up a
  trailing `..` and are otherwise untouched. Approval resolution itself
  (decision 2: "resolved in agentd") is `session::resolve_and_forward`,
  structurally identical to Horizon's own `resolve_and_send_approval` minus
  the local `Frames` update.
- **Horizon-side transport transparency.** `agent::agentd_runtime::
  AgentdConnection::start_session` hands back a `contract::SessionHandle` —
  the exact type `providers::ProviderRegistry::start_session` returns
  in-process — whose `Sender<Command>` forwards each command as a `command`
  envelope (via a small per-session draining thread; commands arrive from
  the UI thread, which isn't async) and whose `Receiver<ProviderEvent>` is
  fed by the connection's demultiplexer. Every existing call site
  (`Registry::agent_sender`, the pane's approve/deny/cancel commands, the
  bash-completion effect's sibling) needed zero changes: they already only
  depend on `SessionHandle`'s shape. `spawn_agent_session_via_agentd`
  (`app/runtime/agent.rs`) is correspondingly small: ask agentd to host the
  session, then fold its event stream through `LiveState::
  extend_provider_events` + `Frames::update_agent_frame` — the same fold
  `spawn_agent_session`'s own effect uses, just without the
  `process_agent_provider_event` step in front of it (that already ran in
  agentd; running it again here would re-execute already-executed tool
  calls). "The fold must not know which transport delivered the events" —
  the design's phrasing — turned out to extend to commands too, for free,
  once `SessionHandle` was the seam.
- **No-double-write, tested at the call-count seam, not by file
  existence.** `AGENT_EVENT_LOG_WRITER` (`app::runtime::agent`) is a
  process-global `OnceLock` shared by every test in the `horizon` binary's
  test process; asserting "the log file at a fresh path was never created"
  is not reliable on its own (an earlier test may have already warmed the
  cache against a *different* path, so a wrongly-reintroduced call would
  silently reuse that writer instead of ever touching the fresh path,
  passing the assertion for the wrong reason). Instead,
  `open_agent_event_log_reuses_process_global_writer`'s neighbor test
  (`agentd_mode_never_opens_horizons_own_event_log`) asserts a `thread_
  local!` call counter on `open_agent_event_log` is unchanged after
  `spawn_agent_session` is given a live (test-only, socket-less)
  `AgentdConnection` — thread-local rather than process-global so it's also
  immune to `cargo test`'s default concurrent test execution. In production,
  `spawn_agent_session_via_agentd` simply never references
  `AgentPersistenceConfig`/`open_agent_event_log` at all.
- **agentd's own persistence open is eager and blocking, at startup, not
  lazy-on-first-session.** Unlike Horizon's `open_agent_event_log` (cached,
  opened on the first agent pane, with the startup read kept off the UI
  thread so pane render never blocks), `horizon-agentd::build_agentd_state`
  opens the writer and waits for its startup-read outcome synchronously in
  `main`, before the accept loop starts, then rebuilds the DuckDB
  projection from that same read if `state_db_path` is configured — a small
  duplication of `app::runtime::agent::rebuild_agent_duckdb_from_event_log`'s
  logic, accepted for the same reason `config::load_file_config`'s
  duplication was in step 2 (no shared third crate for this yet). There is
  no UI to avoid blocking in this binary, so the simpler synchronous wait
  was preferred over threading a `WriterInit` channel through `main`.
  **Skipped-lines status reporting, later restored (see the trims-restored
  addendum below)**: this step originally left the corrupt/torn-line
  summary logged to stderr only, with no connection to Horizon's
  `agent_state_status` bar.
- **Connection loss surfaces an `Event::Error` per affected session, no
  auto-reconnect** (in scope for this step; reconnect is step 4).
  `agent::agentd_runtime::run_connection`'s read loop, on EOF or a
  malformed message, pushes a synthetic error event into every
  currently-registered session's event channel so it folds through the
  ordinary path and appears in that session's transcript, then returns —
  the write task independently notices the same failure on its next send
  and stops. Sessions are **not** killed or moved to a reconnect-pending
  state; the next connection attempt is a fresh `Horizon` process launch.
- **Sessions are scoped to the connection that created them, in
  `horizon-agentd` too.** `session::Connection`'s `sessions`/
  `pending_host_tool_requests` maps are constructed fresh per accepted
  connection (`main::run_session_hosting_loop`), not held at the
  `AgentdState` (process) level. Since `session_load`/reconnect don't exist
  yet and agentd serves one connection at a time by construction, this
  keeps the lifetime story simple: a session's thread has nowhere to send
  events once its connection is gone (the `outgoing` channel's receiver is
  dropped with the connection) and is not explicitly torn down — it idles
  until the process exits. Revisit once step 4 defines what a live session
  should do across a reconnect; moving these maps to `AgentdState` is the
  likely shape of that change.
- **The streaming tool-call-argument-preview feature
  (`ToolCallProgress`/`ToolCallPreparing`) originally did not cross the
  wire, later restored (see the trims-restored addendum below).**
  `session::handle_provider_event` filtered out any `ProviderEvent` carrying
  `tool_call_progress` before forwarding (it folded into agentd's own local
  frame, which nothing read, and was then dropped) rather than inventing a
  wire representation for it — the design's wire `Event` is `contract::
  Event`, which has no variant for this ephemeral, log-excluded signal. In
  agentd mode, a tool call's arguments simply appeared once fully formed
  instead of streaming in.
- **`horizon-agentd`'s connection handling is two-phase**: a fully
  sequential hello handshake (unchanged from step 2 — must complete, with a
  deterministic reply-then-close on rejection, before anything concurrent
  starts) followed by `run_session_hosting_loop`, which spawns a writer
  task and keeps reading — needed because a hosted session can push events
  (or a `host_tool_request`) at any time, not just in reply to something
  Horizon just sent. The writer task is deliberately never awaited on
  disconnect (session threads from `Connection::sessions` may outlive the
  connection and still hold `outgoing` senders — see above — so awaiting it
  to completion could hang the accept loop against ever serving a next
  connection); it is left to run detached until its next send to the dead
  socket fails on its own.
- **Testing scope**: `horizon-agentd`'s `tests/e2e.rs` gained `session_new`
  -> `UserMessage` -> ordered-transcript assertions (mock provider);
  `session_list` reflecting a live session; an auto-allow *host* tool
  (`workspace.snapshot`) round-tripping a real `host_tool_request`/
  `_response` over the socket; an approval round trip (`ApprovalRequested`
  out, `ApproveToolCall` in, `ToolCallFinished` out) via a new mock-provider
  trigger; and `bash` actually spawning a subprocess agentd-side and
  reporting its result back over the wire, via another new mock trigger
  (`crates/horizon-agent/src/providers/mock.rs` gained a `"bash"` trigger
  requesting the real `bash` tool, alongside the pre-existing
  `mock.approval_required`/`workspace.snapshot`/streaming triggers). All of
  it needed the test harness to stop reading the *developer's own*
  `~/.config/horizon/config.toml` and `~/.local/share/horizon/agent-*`
  files — a pre-existing hermeticity gap that step 2's tests never
  surfaced (nothing opened persistence yet); `AgentdProcess::spawn` now
  passes `HORIZON_CONFIG` (nonexistent path) and `HORIZON_AGENT_EVENT_LOG`
  (fresh temp path) explicitly. Horizon-side: `agentd_mode_never_opens_
  horizons_own_event_log` (see above) is the no-double-write proof; every
  pre-existing suite (`cargo test --workspace`) stays green with `[agent].
  agentd` at its default `false`, unexercised by any of this step's new
  code paths.
- **Left for step 4, explicitly**: reconnect, `session_load`, replay/
  bootstrap of an agentd session Horizon didn't create in this run, and the
  `Reload Agent Runtime` command. `AgentdConnection::connect`'s blocking
  startup call and `horizon-agentd`'s per-connection session scoping (above)
  are both shaped for a single Horizon-process-lifetime connection, not a
  reconnect — expect both to change.

### Step 3 trims, restored

Both UX gaps this step's notes called out above were closed in a later pass
(after step 4 landed), without reopening any of step 3's five landing
decisions:

- **Tool-call-argument streaming preview now crosses the wire.** Rather than
  inventing a wire `Event` for something `contract::Event` deliberately
  excludes (see above), the wire gained its own connection-message-shaped
  payload: `wire::Control::ToolCallProgress(contract::ToolCallProgress)`,
  session-scoped via the envelope's own `session_id` field (guardrail 1
  holds — `wire` already depends on `contract` for every other `Control`
  payload; this reuses `ToolCallProgress` as-is rather than mirroring it).
  `session::handle_provider_event` now splits `Processing::horizon_events`
  by whether `tool_call_progress` is `Some` instead of filtering it out:
  progress ticks go out as `Control::ToolCallProgress` envelopes, everything
  else as ordinary `Event` envelopes — both after the same
  `LiveState::extend_provider_events` fold/persist call, unchanged.
  Horizon's `agent::agentd_runtime::dispatch_incoming` answers a
  `Control::ToolCallProgress` by reconstructing a `ProviderEvent` via
  `ProviderEvent::tool_call_progress` and sending it down the same
  `session_events` route an ordinary event takes — `fold_agent_session_events`
  folds it through the exact same `apply_tool_call_progress_to_frame` path
  a persisted event would, so the pane-side rendering code
  (`AgentFrameItem::ToolCallPreparing`) needed no changes at all. Because
  `contract::Event` still has no such variant, the persistence exclusion is
  structural, not just tested: an agentd-hosted session's on-disk JSONL log
  cannot represent this in the first place. Proven end to end (real socket,
  real on-disk log file) by `horizon-agentd/tests/e2e.rs`'s
  `streaming_tool_call_progress_reaches_the_client_but_never_the_event_log`.
- **Skipped-lines status reporting now reaches Horizon's status bar.** A
  second connection-global control message, `wire::Control::
  SkippedLines(String)`, carries `persistence::event_log::ReadReport::
  skipped_summary`'s human-readable text (already computed at startup by
  `main::open_persistence`, previously only `eprintln!`'d). `AgentdState`
  gained a `skipped_lines_summary` cell, set alongside `set_writer` in
  `spawn_resume_task`, before `mark_resume_ready`. Sending it from inside
  `Control::Hello`'s reply was rejected: hello must keep answering
  immediately regardless of whether the background startup resume has
  finished (the whole point of `main`'s bind-first ordering, and a property
  `hello_answers_immediately_while_session_list_waits_for_a_slow_resume`
  pins down) — folding a resume-dependent field into it would mean either
  blocking hello on the same readiness gate `session_list`/`session_load`
  use, or racily omitting the field. Instead, `main::
  run_session_hosting_loop` spawns one detached task per connection that
  awaits `wait_until_resume_ready()` (the same gate, just off hello's
  critical path) and then sends `Control::SkippedLines` once, only if
  there's something to report. Horizon's `agent::agentd_runtime::
  dispatch_incoming` routes it to a dedicated `Receiver<String>` (threaded
  out of `AgentdConnection::connect` alongside the existing host-tool
  receiver, to both production call sites — startup and `Reload Agent
  Runtime`'s reconnect) that `wire_skipped_lines_status` folds into
  `agent_state_status` as `"Agent event log: {summary}"`. Proven by
  `corrupt_event_log_lines_are_reported_to_the_client_once_per_connection`,
  which pre-seeds a corrupt fixture log and asserts the very first envelope
  a fresh connection receives (after `hello`) is the matching
  `Control::SkippedLines`.

## Step 4 implementation notes

**Deviation reversed mid-step.** Step 4 was originally scoped with an
agreed deviation: keep `[agent].agentd` at its default `false` and leave the
in-process path in place, deferring the flip to a later, separate decision
after dogfooding. That was superseded by a direct directive change partway
through this step: retire the in-process path outright and delete the flag
entirely. `horizon-agentd` is now the *only* place an agent session runs;
there is no config knob and no fallback. Everything below reflects that
reversal, not the original plan.

- **`Event::TurnEnded(TurnEndReason)`** (`horizon_agent::contract`) — four
  variants, named after the design's own wording verbatim: `Completed`,
  `Cancelled`, `Failed`, `Halted`. Folds as a no-op everywhere a frame is
  built (`frame::apply_agent_event_to_frame`) and projects as a no-op in the
  DuckDB store (`persistence::projection::duckdb::projection`) — it exists
  for persisted forensics/bootstrap, not pane rendering, matching the
  provider-request-lifecycle markers' existing treatment.
  - **Emission is centralized in `providers::rig::session`, not spread
    across every `StateChanged` send site.** `apply_turn_outcome` (bumped
    from private to `pub(super)` so `tests.rs` can drive it directly) is the
    one function nearly every rig turn's outcome funnels through, and it now
    emits `TurnEnded` for three of the four reasons (`Completed`/
    `Cancelled`/`Failed`) based on `TurnCompletion`'s fields — including a
    new `failed: bool` field, since a failed provider request and a
    completion with nothing to do are otherwise indistinguishable (both:
    not cancelled, no tool calls requested). The fourth reason, `Halted`
    (the iteration-cap/doom-loop guard), is emitted from `halt_turn_loop`,
    which stops the loop *instead of* producing a `TurnCompletion` for
    `apply_turn_outcome` to see. The idle `Command::Cancel` arm (cancelling
    while nothing is mid-stream, only pending tool calls) is the one other
    site, mirroring `apply_turn_outcome`'s cancelled branch inline since it
    never goes through a `TurnCompletion` at all.
  - **`providers::mock` was deliberately left untouched.** It's exercised
    entirely by tests, and every step-4 test that needs a turn to sit open
    indefinitely (for the kill-mid-session scenarios below) uses mock's
    tool-approval trigger — a genuinely open turn with no timer, not a race
    against mock's word-by-word streaming. Nothing in step 4 needed mock to
    emit `TurnEnded` on its own graceful paths; if a future step wants
    provider parity for `TurnEnded` there, it's a small, separable addition.
  - **Unit coverage**: the six pre-existing `providers::rig::tests` session
    tests that assert exact event sequences all gained the new
    `TurnEnded(..)` in their expected order (`Completed` before a normal
    `WaitingForUser`, `Cancelled` before `StateChanged(Cancelled)`,
    `Halted` before `halt_turn_loop`'s closing `WaitingForUser`). `Failed`
    has no existing live-session test to extend (triggering it for real
    needs the OpenAI completion call to error, which isn't worth wiring a
    fake network client for here) — covered instead by a focused unit test
    that drives `apply_turn_outcome` directly with `TurnCompletion { failed:
    true, .. }`.

- **Durability fix required to make replay meaningful against a real `kill
  -9`.** `persistence::event_log::writer::run_writer` used to leave each
  appended record sitting in `BufWriter`'s in-memory buffer until the next
  explicit `flush()` (which `horizon-agentd` never called except on
  `Control::Drain`, and even that is *tokio's* socket-write flush, not the
  event log's). A session parked indefinitely (e.g. `WaitingForApproval`,
  no timer to trigger more traffic) could lose its entire buffered
  transcript to a hard kill with nothing to do with the kill itself —
  discovered while writing the kill-mid-session e2e test below, which was
  silently losing events until this was fixed. `run_writer` now flushes
  after every append. Still not an `fsync` (page-cache durability only, per
  `WriterHandle::flush`'s existing doc comment) — a full machine crash can
  still lose an unsynced write; that tier is out of scope. This is a
  crate-wide behavior change (`crates/horizon-agent`), not agentd-specific,
  but agentd is the only remaining writer of this log.

- **Replay on start: `horizon-agentd::session::resume_persisted_sessions`.**
  Runs once, synchronously, in `main` right after the startup event-log read
  and before the accept loop starts. Groups the startup read's records by
  `session_id`, and for each session:
  1. Folds its events into an `AgentFrame` (`agent_frame_from_events`) and
     checks `AgentFrame::is_turn_in_flight()` — reusing the exact predicate
     the palette's `Cancel Agent Turn` enablement already uses, rather than
     re-deriving "was a turn open" from `persistence::event_log::turn`'s
     `TurnTracker` state machine a second time.
  2. If a turn is in flight, synthesizes and durably appends (via a fresh
     `Appender` for that session) a `ToolCallFinished(cancelled)` for every
     still-outstanding tool call (mirroring what a live `Command::Cancel`
     does — without this, an interrupted `WaitingForApproval` call kept
     reading as pending forever, since nothing else ever resolves it),
     followed by `TurnEnded(Cancelled)` and `StateChanged(WaitingForUser)`.
  3. Spawns the session's thread exactly as `Control::SessionNew` would
     (`spawn_session_thread`, now shared by both call sites), seeded with
     the full (possibly just-extended) event history.
  - **Turn-id continuity is not preserved for the synthesized cancellation.**
    The fixup's `Appender` starts a fresh `TurnTracker`, so the closing
    events land with `turn_id: None` rather than the interrupted turn's own
    id. Acceptable: `turn_id` is a persistence-forensics grouping aid (used
    by the `agent-inspect` skill), not load-bearing for the frame fold or
    for `is_turn_in_flight`, which don't look at it at all. Worth
    revisiting only if turn-id-keyed forensics across a crash boundary
    becomes a real need.
  - **Every persisted session is resumed eagerly at startup, not lazily on
    first `session_load`.** This is what makes "sessions are live again
    (`WaitingForUser`), listed by `session_list`" literally true before any
    client ever connects, and it sidesteps a much harder question (which
    historical source — the startup read's records, or a since-spawned
    thread's own `LiveState` — answers a `session_load` for a session
    nobody's touched yet). The trade-off: a long-lived daily-driver
    accumulating many past sessions means many resumed threads at every
    agentd restart. Acceptable at this project's current (dogfooding)
    scale; a lazy-resume-on-first-`session_load` design is the likely
    follow-up if it ever isn't.
  - **A restart's own "session started" banner becomes permanent history.**
    Every resumed session's thread runs the same `Created`/init-message/
    `WaitingForUser` startup burst a brand-new session gets, which gets
    persisted like any other event. Over many restarts this adds a visible
    "provider restarted" marker to the transcript every time — treated as
    an accepted (arguably useful, forensically) consequence rather than
    something to suppress, not a design goal in itself.

- **`AgentdState` absorbed what used to be `Connection`'s own state**
  (`sessions`, `pending_host_tool_requests`) **plus a new `outgoing` cell**
  (`Mutex<Option<UnboundedSender<Envelope>>>`), exactly the shape step 3's
  notes predicted ("moving these maps to `AgentdState` is the likely shape
  of that change"). `Connection` is now a thin `Arc<AgentdState>` wrapper.
  `outgoing` is what makes a session spawned before any connection exists
  (a resumed session at startup) representable at all: every session thread
  sends through this shared, swappable cell (`send_envelope`, which
  silently no-ops when it's `None`) rather than owning a connection-specific
  sender captured at spawn time; `Connection::new` installs the current
  connection's sender into it, `Connection::disconnect` (called from
  `main`'s two connection-loop exit points) clears it back to `None` so a
  session doesn't keep "successfully" enqueueing into a writer task that's
  already given up on a dead socket.

- **`session_load` is answered by the session's own thread, not read out of
  its `LiveState` from another thread.** `LiveState`'s internal `Rc<RefCell<
  ..>>` is deliberately `!Send` (see its own doc comments), so it can't be
  stashed in the cross-thread `AgentdState.sessions` map the way `inbound`
  already is. Instead, `SessionEntry` gained a `replay: Sender<Sender<
  Vec<Event>>>` — a tiny request/reply channel, agentd-internal only (not
  part of the wire contract): `Connection::replay_events` sends a one-shot
  reply channel down it and the session's own `run_session` loop answers
  from a new fourth `select!` arm by calling `live_state.events()`
  (`LiveState` gained this accessor — a plain clone of its internal event
  vec, `Vec<Event>` being genuinely `Send`) on its own thread, no Rc
  crossing threads at all. `Connection::replay_events` runs the blocking
  wait inside `tokio::task::spawn_blocking` so a slow session can't stall
  the connection's read loop for unrelated traffic, and `main`'s handling of
  `Control::SessionLoad` awaits it inline (not spawned detached) so the
  replay burst can't race a client's very next command for the same
  session.
  - **`LiveState::with_event_log_and_history`** (`with_event_log` now
    delegates to it with an empty history) and **`State::from_history`**
    are the crate-side seam this and `resume_persisted_sessions` both use to
    seed a fresh `LiveState`/`State` with already-committed events so the
    first fold reflects the whole transcript, not just what arrives after
    construction.

- **Horizon's `AgentdConnection` gained `attach_session` alongside
  `start_session`**, both now built on a shared `register_session_routing`
  (event-route registration + the command-draining thread) that neither
  sends anything itself: `start_session` sends `session_new`,
  `attach_session` sends `session_load`. This is the seam that makes a
  reconnected/resumed session's handle indistinguishable from a brand-new
  one at every existing call site, extending step 3's "the fold must not
  know which transport delivered the events" to "...or whether this
  session's history predates this connection."
  - **`session_list` is a blocking round trip with no request id on the
    wire**, deliberately: `AgentdConnection` gained a `pending_session_list:
    Arc<Mutex<Option<Sender<Vec<SessionSummary>>>>>` cell (the same shape
    `horizon-agentd`'s own host-tool-response routing uses, minus the id,
    since Horizon only ever has one `session_list` round trip outstanding
    at a time — startup, or a `Reload Agent Runtime` — never two
    concurrently).
  - **`agent::agentd_runtime::reconnect_all_sessions`/`attach_sessions`/
    `fold_agent_session_events`** implement "on connect: `hello` ->
    `session_list` -> `session_load` for every session": `attach_sessions`
    calls `Workspace::register_detached_session` (new, `workspace::session`
    — a thin wrapper around the existing `ensure_session` insertion, which
    was already idempotent) unconditionally for every summary, then
    `attach_session` + the shared fold. Idempotency does the rest: a
    session Horizon already has a pane for is untouched by the
    `register_detached_session` call (already known) and just gets its
    frame/handle refreshed ("reattach seamlessly"); one Horizon has never
    seen shows up as a brand-new detached session ("survival made
    visible"). `app::state::AppState::new` calls this once, synchronously,
    right after a successful startup connect — a fresh Horizon process has
    no panes yet, so at startup every summary takes the "new detached
    session" branch.
  - **`workspace::session::register_detached_session` is the one edit this
    step made outside its otherwise-declared `crates/**`/`src/agent/**`/
    `src/app/**` ownership boundary.** It's a 3-line, purely additive
    wrapper with no behavior change to anything it doesn't touch, added
    because the "surface unknown sessions as detached" requirement has no
    existing seam to hang off of otherwise; flagged here for visibility
    since `workspace/` is nominally another area's territory.

- **`Reload Agent Runtime`** (`CommandId::ReloadAgentRuntime`,
  `command_actions::reload_agent_runtime` dispatching to
  `agent::agentd_runtime::reload_agent_runtime`) implements "drain -> agentd
  flushes and exits -> Horizon spawns the rebuilt binary -> reconnect ->
  `session_load`" end to end: sends `Control::Drain` on the current
  connection (best-effort), immediately sets `agentd_connection` to `None`
  (so no new session tries to route through the dying connection while this
  is in flight — already-attached panes get `mark_connection_lost`'s
  synthetic `Event::Error` once the old connection's read loop notices it's
  gone), then does the respawn-delay/`AgentdConnection::connect`/
  `session_list` round trip on a background thread and only touches floem
  signals from the `create_effect` callback that receives the result — the
  same "blocking work off the UI thread, signals on it" split every other
  cross-thread bridge in this codebase already uses
  (`spawn_persistence_initialization` et al.). Progress and the outcome
  surface through `agent_state_status`, the pane-independent status the
  status bar already renders — no new UI surface needed.
  - **`CommandId::ReloadAgentRuntime` is unconditionally enabled**, not
    gated on "agentd mode" the way the task's original phrasing assumed —
    once the in-process path was retired, there is no other mode to
    contrast against, and gating it on "is there currently a live
    connection" would need a new `CommandState` field that `control_surface`
    (outside this step's ownership) would also have to learn to populate.
    Reload is exactly the command you want available *while* the
    connection is broken, so always-enabled is also the more correct
    behavior, not just the smaller diff.
  - **Version mismatch at hello surfaces as literally
    `"agent runtime reload required: ..."`** in `agent_state_status`
    (`reload_failure_status`, unit-tested against both the mismatch and a
    generic-failure string) — never silent, per the design's own wording.
  - **The dev-flow gotcha this step also had to close**: `agentd_client::
    spawn_agentd` used to do a bare `Command::new("horizon-agentd")`, which
    only resolves via `$PATH` — `cargo run` alone never puts
    `target/debug` on `PATH`, so a workspace build that only ran `cargo
    run` (not `cargo build --workspace`) would fail to spawn agentd with an
    opaque "not found" error. `resolve_agentd_binary` now looks next to
    Horizon's own `current_exe()` first (the directory `cargo build
    --workspace`/`cargo run` both actually put both binaries in) before
    falling back to a bare `PATH` lookup, and `spawn_agentd`'s error message
    explicitly says to run `cargo build --workspace`. `AGENTS.md`'s
    Commands section now lists `cargo build --workspace` as the canonical
    build step for the same reason.

- **In-process retirement — what actually got deleted.**
  `app::runtime::agent` shrank from the full in-process session
  loop/persistence-open/DuckDB-rebuild machinery (~800 lines, including its
  own test suite covering the JSONL/DuckDB replay paths already covered by
  `crates/horizon-agent`'s own tests) down to just `spawn_agent_session`
  (agentd-routed) and an error-frame fallback for "no connection". Also
  removed: `agent::load_agent_config`/`agent_file_config_from_raw` (no
  longer any production caller — agentd resolves its own `AgentConfig`
  independently and always has); `config::RawAgentConfig.agentd`;
  `AppState.agent_config`/`SessionRuntimeState`'s `agent_config` parameter
  (nothing downstream of the retired in-process path needed it).
  `host_tools::WorkspaceHostTools` (the in-process `HostTools` impl) has no
  production caller left either — demoted to `#[cfg(test)]`, still
  exercising the same seam its tests always did, since
  `host_tools::workspace_snapshot` (the function it wraps) is genuine
  production code, called by `agent::agentd_runtime::answer_host_tool_request`
  over the wire instead.
  - **`app::runtime::shutdown`/`app::shutdown` are kept as no-ops** rather
    than removed, since `main.rs` (outside this step's `src/app/`
    ownership) wires `app::shutdown()` to floem's `AppEvent::WillTerminate`
    and there's nothing gained by touching that call site just to delete a
    now-empty function.

- **Testing scope / gaps worth naming.** `horizon-agentd`'s `tests/e2e.rs`
  gained: a hard-kill-mid-session scenario (parks a session in
  `WaitingForApproval` — no timer, so no flakiness — kills the process,
  respawns at the same socket/log paths, and asserts the transcript
  survives, the interrupted turn shows `TurnEnded(Cancelled)`, the pending
  approval no longer reads as pending, and a fresh message still works); a
  same-running-process disconnect/reconnect scenario asserting
  `session_load`'s replay folds to the *exact* frame a live connection saw
  (not just "some events"); and a graceful-drain-then-respawn scenario
  covering the clean-shutdown path `Reload Agent Runtime` actually drives
  (distinct from the hard-kill scenario, which explicitly also proves that
  a *cleanly completed* turn is never mis-marked as cancelled on resume).
  All three reuse the mock provider and the same `AgentdProcess` harness,
  extended with `spawn_at`/`kill_and_wait` for the respawn-at-same-paths
  shape. **What's still only unit-tested or manually verified, matching the
  precedent already accepted in steps 2-3**: `agent::agentd_runtime::
  reload_agent_runtime`'s own spawn/reconnect orchestration (the
  respawn-delay thread, `AgentdConnection::connect`'s cold-start spawn
  path, `session_list`'s wire round trip) has no test exercising it against
  a *real* spawned `horizon-agentd` from Horizon's own test binary —
  `CARGO_BIN_EXE_horizon-agentd` is only set for `horizon-agentd`'s own
  integration tests (a different package), so a cross-package test needs
  its own setup (passing the binary path explicitly, or building it as a
  test fixture) that wasn't judged worth adding on top of everything else
  in this step. `reload_failure_status`'s string-mapping and the pre-
  existing `agentd_client::handshake` tests (version-mismatch, rejection,
  closed-before-reply) cover the pieces that don't need a real subprocess.

### Step 4 addendum: readiness no longer waits on DuckDB, and reload is visible

Landed after dogfooding surfaced two complaints: `Reload Agent Runtime`
looked like it did nothing, and a restart with a nontrivial event log sat
noticeably before `session_list`/`session_new` answered.

- **The DuckDB rebuild moved off the resume-readiness path.** It used to run
  synchronously inside `open_persistence`, *before* `set_writer`/
  `resume_persisted_sessions`/`mark_resume_ready` — so every readiness-gated
  request waited on a full rebuild of a derived, non-authoritative read
  model that no session actually needs (sessions resume from the JSONL log
  directly; nothing routes a live session through DuckDB). `main::
  spawn_resume_task` now marks readiness right after resuming sessions and
  only *then* kicks off `main::spawn_duckdb_rebuild_task` as its own
  separate `spawn_blocking` task — `hello`/`session_list`/`session_load`/
  `session_new` no longer wait on it at all. A rebuild failure keeps the
  existing non-fatal stderr line and is also folded (appended, not
  clobbered) into `AgentdState`'s skipped-lines-style status, so a client
  that connects *after* the failure still learns about it via the existing
  `Control::SkippedLines` channel.
- **A cheap freshness check skips the rebuild on an unchanged log.**
  `main::rebuild_duckdb_projection` opens the store, and — unless opening it
  just migrated a legacy pre-`event_at` schema (`Store::
  migrated_legacy_schema`, added for exactly this: a migration drops and
  recreates `agent_events` without touching `agent_sessions`, so that
  table's `last_sequence` values would otherwise look deceptively "current"
  against a now-empty projection) — compares `Store::max_last_sequence`
  (a single `MAX(last_sequence)` aggregate over `agent_sessions`) against
  the log's own final record's sequence. `Record::sequence` is a single
  counter global to the whole log, not per-session (`event_log::writer`'s
  "Ordering guarantee"), and the startup read's records are already sorted
  ascending, so the last element is the log's overall maximum — no
  additional scan needed. Equal means the projection already reflects
  everything on disk, so the (expensive) full rebuild is skipped entirely,
  logged as a one-liner; any mismatch, migration, or error in the check
  falls through to the same full rebuild as before. On a long-lived
  daily-driver log this makes a clean restart's DuckDB work nearly free
  instead of a full re-import every time.
- **`Reload Agent Runtime` gained staged, short status messages** instead of
  a single "waiting..." line followed by silence until success or failure:
  `agent::agentd_runtime::ReloadStage`/`reload_stage_status` format
  "agent runtime: draining…" → "…spawning…" → "…replaying N session(s)…" →
  "…reconnected (X.Xs)", pushed through a dedicated progress channel (kept
  separate from the existing `ReloadOutcome` channel, which still carries
  the final `Connected`/`Failed` result) into the same `agent_state_status`
  signal the status bar already renders.
- **A real drain-timeout, where none existed before.** The old code sent
  `Control::Drain` and unconditionally slept a fixed 200ms courtesy delay
  before trying to reconnect — if the old process's drain somehow never
  landed or never completed, `reload_agent_runtime` would eventually just
  reconnect to whatever was listening, silently defeating the point of a
  reload (or worse, racing a stale process). `agent::agentd_runtime::
  wait_for_drain` now polls (bounded by `DRAIN_TIMEOUT`, 2s) until nothing
  answers a connection on the socket path before proceeding to spawn-or-
  connect; timing out surfaces as an ordinary `ReloadOutcome::Failed`
  through the same `reload_failure_status` mapping every other reconnect
  failure (spawn failure, handshake/contract-version mismatch) already
  used, so none of reload's failure paths are silent.
- **Testing.** `crates/horizon-agentd/tests/e2e.rs` gained: a rebuild-delay
  test-only hook (`HORIZON_AGENTD_TEST_DUCKDB_REBUILD_DELAY_MS`, the DuckDB
  analogue of the existing resume-delay hook) proving `hello`/`session_list`
  answer promptly while a slow rebuild is still running; a skip-path test
  (second spawn against an *unchanged*, already-terminated-session log
  never re-runs the rebuild — observed via the rebuild-or-skip stderr
  marker, polled for while the process is still alive, since nothing over
  the wire signals it by design); and a stale-log test (a log that grew
  between spawns still triggers a full rebuild). The existing legacy-schema
  migration test continues to pass unchanged. `agent::agentd_runtime`'s
  staged-status ordering and `wait_for_drain`'s timeout/short-circuit
  behavior are unit-tested directly (message formatting/ordering, and a
  real-but-local `UnixListener` for the timeout case) rather than end-to-end
  through floem's signal plumbing against a real spawned `horizon-agentd` —
  matching the precedent already accepted above for this same function's
  spawn/reconnect orchestration.
- **Pre-existing flake, unrelated to this addendum.** `killed_agentd_
  respawns_and_replays_transcript_with_open_turn_cancelled` and `drained_
  agentd_respawns_and_preserves_a_completed_session` intermittently fail
  under heavy parallel `cargo test` load (confirmed present on unmodified
  `main` too, via a stashed before/after comparison) — a race between the
  JSONL writer's background flush and a `SIGKILL`/respawn happening fast
  enough to race it, not anything this addendum touches. Worth a follow-up
  if it proves disruptive.
