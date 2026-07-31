# Runtime-boundary crate alignment — sessions become attachment records

Status: core judgments decided with the owner, 2026-07-31 (consult
session 6fa635f0). Implements `docs/view-runtime-principle.md` principle
3 ("one shared foundation") and principle 4 ("crates follow runtime
boundaries") at the crate/module level, and abolishes the residual
`sessiond` concept. Subsumes `docs/tasks/backlog.md` #70 (the
`horizon-wire` carve-out) as phase 1.

## The decision, in one line

A session is an **attachment record** — (view kind, runtime, session id)
— and nothing else at any shared layer. Everything with behavior (state,
transcript, PTY, lifecycle, persistence) belongs to the one runtime that
owns the session; "sessiond" disappears as a concept because there is no
shared session abstraction left to host.

## Core judgments

1. **Session = attachment record.** The shared vocabulary for a session
   is (view kind, runtime, session id). The workspace model already
   treats panes' sessions this way (attach/detach over ids); that is the
   whole cross-view surface.
2. **`horizon-wire` (new crate) holds the domain-free foundation**:
   `WireCodec` and the codec pin, `ClientHello`/`VersionRange` and
   negotiation, the `*_MAX_ITEM_BYTES` caps, `DecodeSkipLog`, socket
   path conventions for every daemon, the schema-check machinery, and
   the `SessionId` type. No session semantics, no domain types, ever.
3. **Hub wire lives with its domain.** `SessionHub` (agent) moves into
   `horizon-agent::wire`; `TerminalHub` and its frame/window types move
   into `horizon-terminal-core::wire`. Each carries its own version
   pair (`AGENT_PROTOCOL_VERSION`/`MIN_…`, `TERMINAL_PROTOCOL_VERSION`/
   `MIN_…`) and its own schema artifact. `horizon-session-protocol`
   dissolves (its `legacy` module — the pre-remoc JSONL drain — is
   agent history and moves to the agent side).
4. **The shell mirrors the shape.** `src/sessiond/` becomes generic
   connection machinery (connect/hello/respawn/drain/route, parameterized
   by hub) plus one client per runtime, renamed so the word "sessiond"
   (and `SessiondState` in agentd) leaves the tree.
5. **Cross-view surfaces are composed views, not shared state.** Manage
   Sessions and workspace restore query each runtime and compose; parent
   lineage is workspace-level data about attachments (its ownership move
   is issue 013 territory and does not block this work).
6. **Everything is wire-neutral.** Types move between crates without
   touching serde shapes or Postbag indexes; the schema generators keep
   emitting the same JSON keys (the artifact's section names are ours to
   pin, independent of Rust item names). Splitting the artifact file in
   two is checker infrastructure, not a wire change: both new version
   pairs start at the current 18/18 and `scripts/check-wire-schema.sh`
   learns to diff two files. No bump, no daemon restart, no
   auto-drain.

## Why (the measured misalignment)

- `horizon-session-protocol` is a union, not a foundation: one 983-line
  lib.rs holds the negotiation base plus BOTH hubs' vocabularies, so
  both daemons depend on the union and one `SESSION_PROTOCOL_VERSION`
  spans both wires. Under the v18 lockstep policy (no feature gates,
  mismatch → auto-drain-and-respawn) that shared constant means every
  agent-side wire change drains the terminal runtime too — killing the
  PTYs the terminald split exists to keep alive.
- `horizon-terminald` links `horizon-agent` (and through it libduckdb)
  for exactly one symbol: `socket::default_terminald_socket_path`, a
  foundation utility parked in the wrong crate (backlog 70's
  observation).
- `horizon-agentd` dev-depends on `horizon-terminal-core` solely because
  the union schema artifact makes its generator name terminal types.
- After the split, principle 4's manifest test holds:
  `agentd → horizon-agent + horizon-wire` (no terminal crates),
  `terminald → horizon-terminal-core + horizon-wire` (no agent crates).

## Phases (each lands gate-green and wire-neutral on its own)

1. **Carve `horizon-wire`** (backlog 70): move the foundation listed in
   judgment 2 out of `horizon-session-protocol` (and the socket-path
   helpers out of `horizon-agent`); repoint daemons and shell.
   Acceptance: `horizon-terminald`'s manifest has no `horizon-agent`;
   wire artifact byte-identical.
2. **Move the hubs, split the versions and the artifact**: judgment 3.
   `horizon-session-protocol` is deleted at the end of this phase. Both
   version pairs start at 18/18; the artifact becomes
   `agent-wire.json` + `terminal-wire.json` with unchanged inner keys;
   `check-wire-schema.sh` and the two `wire_schema` generator tests
   follow. Acceptance: `horizon-agentd`'s dev-dependency on
   `horizon-terminal-core` is gone; both artifacts' content matches the
   old union artifact section-for-section.
3. **Rename the shell/daemon residue**: `src/sessiond/` → per-runtime
   clients + shared machinery under a runtime-named module,
   `SessiondState` renamed, "sessiond" vocabulary swept from code,
   comments, and docs. Pure rename; no wire, no behavior.

## Explicitly out of scope (recorded so they are not lost)

- **Aligning session semantics across hubs** (making `new`/`attach`/
  `drain` shapes uniform so future view kinds — WASM plugin views —
  slot in cheaply). The owner holds this intent for when the next view
  kind arrives; deliberately not folded into this realignment.
- Moving lineage ownership to the workspace (issue 013's fix decides
  this).
- Any wire shape change whatsoever.
