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
  **Skipped-lines status reporting is omitted**: a corrupt/torn-line summary
  is logged to stderr but not surfaced anywhere a human using Horizon would
  see it (Horizon's own `agent_state_status` bar has no equivalent
  connection to agentd's startup read) — worth wiring once there's a
  concrete signal agentd can push to Horizon for "startup diagnostics", not
  invented here.
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
  (`ToolCallProgress`/`ToolCallPreparing`) does not cross the wire.**
  `session::handle_provider_event` filters out any `ProviderEvent` carrying
  `tool_call_progress` before forwarding (it folds into agentd's own local
  frame, which nothing reads, and is then dropped) rather than inventing a
  wire representation for it — the design's wire `Event` is `contract::
  Event`, which has no variant for this ephemeral, log-excluded signal. In
  agentd mode, a tool call's arguments simply appear once fully formed
  instead of streaming in; a real gap, not called out as in-scope to close
  in this step.
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
