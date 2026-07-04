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
