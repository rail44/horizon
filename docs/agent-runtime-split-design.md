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
