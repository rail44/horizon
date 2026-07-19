# Session Daemon — Design Decisions

Status: decided 2026-07-07 (owner consultation in the project session);
the `horizon-terminal-core` extraction-slice decisions (8, 9, and the
color amendment to 4) added 2026-07-09. The exploration material and
option analysis is `docs/research/session-daemon.md`; this file records
the decisions and is the scope reference for the migration. Steps 0 and 1
are implemented end to end; the Step 2A recovery boundary was approved on
2026-07-12.

Motive: terminal sessions must outlive the UI process (a UI crash or
restart must not kill the shell and its children), and delegated
terminal sessions need the same daemon-hosted home that agent sessions
already have. Horizon's agent runtime already proved the shape
(`docs/agent-runtime-split-design.md`): a UI-agnostic library brain,
hosted by a daemon, with the UI as a reconnecting client.

## Decisions

1. **The whole terminal brain moves to the daemon; no PTY-only stage.**
   The daemon owns the PTY *and* `TerminalCore`/emulation, mirroring how
   agentd owns providers/tools/persistence. A PTY-only split (VT
   interpretation left in the UI) is rejected outright: it loses screen
   state on a UI crash, which is the very motive, and re-creates the
   "reinterpret bytes at each layer" pattern the owner criticized.

2. **Expand agentd; rename the daemon to `sessiond`.** Grow the existing
   daemon to host both session kinds rather than starting a new binary
   or standing up a sibling `terminald` — the design record already
   named this ("the embryo of the tmux-style session daemon"), and it
   reuses the proven socket discovery, `Hello{binary_id}` handshake, and
   drain/reconnect. The binary is renamed `agentd` → `sessiond` (it no
   longer hosts only agents); the socket path and `binary_id` follow.
   The `horizon-agent` library keeps its name and scope (the agent
   brain, UI-agnostic) — it is the template the terminal is raised to,
   not something absorbed. A new sibling library crate (working name
   `horizon-terminal-core`) is extracted from `src/terminal` to be the
   terminal's UI-agnostic brain; `sessiond` hosts both crates. The
   terminal gains a client seam in the `horizon` binary mirroring
   `src/agent/`.

3. **Sister contracts, not one union.** Following the `horizon-control`
   precedent (shared Envelope, separate contract): the terminal and
   agent command/event vocabularies stay as separate enums sharing
   `wire.rs`'s `Envelope{v,id,body}`, matching the separate library
   crates. The agent contract (`contract.rs`) is preserved as-is rather
   than rebuilt to absorb the terminal. Cross-kind operations already
   carried by the CLI control plane (`horizon-control`: CreateSession,
   AttachSession, listing) are not duplicated; only new daemon⇄UI needs
   (frame subscription, attach) get a thin shared layer.

4. **Row-diff push for frames; full snapshot on attach/reconnect.** With
   `TerminalCore` on the daemon side, the UI renders interpreted grid
   state received over the socket. Payload is per-row diff: the daemon
   runs `layout.rs`'s existing `TerminalLine` `PartialEq` comparison at
   the send point (moving proven logic across the boundary, agentd
   Step3 pattern), sending only changed rows. The existing rate control
   (16ms window, dirty flag, no idle wakeup) is transport-independent
   and carries over unchanged. Transport is push (the session side sends
   on dirty), except the initial attach and each reconnect, which pull a
   full snapshot to establish the diff baseline. True alacritty_terminal
   damage tracking (the heavier real-diff option) is deferred: adopt it
   only if bandwidth measurement on redraw-heavy panes (e.g. large
   `htop`) shows the row-diff is insufficient. The rows compared here
   carry *logical* colors, not resolved RGB (decision 8), so a theme
   change produces zero row diffs — the UI re-paints from the same
   logical rows rather than re-diffing every row.

5. **Reload terminates terminal sessions; keep live-PTY hand-off out of
   the critical path.** Agent state is an append-only log (restorable on
   restart); terminal state is the live screen plus live child
   processes, which a log cannot replay. Surviving a `sessiond` *binary
   update* with the PTYs alive needs execve re-exec / fd passing —
   reliability agentd's drain has never proven. This is split out
   (backlog 20): the UI-crash-survival motive is already met by
   `sessiond` being a separate process, so the first migration form
   accepts "sessiond reload terminates terminal sessions; agent sessions
   restore from the log."

6. **One client connection; leave room for `client_id`.** Inherit
   agentd's single-connection simplification. Multi-client fan-out
   (same session in two windows; a future headless viewer observing a
   delegate live) has no present motive and is an additive extension,
   not a contract rebuild. The one cheap hedge: implement decision 4's
   send state so the per-client send cursor is not structurally hard-set
   to one (can become a map later). zellij's per-client
   `ServerToClientMsg::Render` is the reference if/when adopted.

7. **Migration order.** (0) Extract `horizon-terminal-core` from
   `src/terminal` — headless, daemon-independent, low-risk (agentd Step1
   analogue). (1) Rename to `sessiond` and stand up terminal hosting:
   the sister terminal contract with initial-snapshot pull + row-diff
   push, minimal enough to render one session through `sessiond`. (2+)
   Migrate existing terminal sessions, reconnect (reload terminates
   terminals per decision 5), the `client_id` hedge. The CLI endpoint
   move (a candidate starting point) is already done — the control plane
   landed as an in-process listener designed to be endpoint-transparent,
   so "the peer turns out to be sessiond" needs no client change.
   Starting only new session kinds in the daemon, or a partial PTY
   split, are both rejected (they miss the motive / were excluded by
   decision 1).

8. **Color resolves in the UI, not the daemon; logical colors on the
   wire.** Today `TerminalCore` resolves each cell to raw RGB at snapshot
   time (reading `ui::theme::terminal_colors()` — its one cross-crate
   dependency, and the thing that blocks the extraction). Instead it
   emits each cell's *logical* color — an ANSI/256 index, a named color,
   or "default fg/bg" — and the UI resolves index→RGB at paint time with
   its own theme. Rationale: `sessiond` may serve one terminal to more
   than one client (decision 6's hedge; a future headless viewer), and a
   client's theme is its own presentation, not shared daemon state —
   baking one theme into the daemon frame would be wrong for a
   differently-themed second client. Consequences: (a) the frame/row
   payload carries logical colors, feeding decision 4's theme-independent
   row-diff; (b) `Reload Config` live re-theming needs no daemon
   round-trip (the UI re-resolves on the next paint); (c) it removes
   `TerminalCore`'s last cross-crate dependency, unblocking decision 9.

9. **`horizon-terminal-core` is the byte-driven brain; the PTY stays in
   `sessiond`.** The crate extracted in decision 7 step 0 contains the VT
   core (`TerminalCore` + emulation: bytes → frame) and the session loop
   (`run_terminal_core`: the 16ms coalescing / snapshot / sync-update
   failsafe), driven purely by byte channels. It does *not* contain the
   spawn layer (`portable-pty`, threads, environment setup) — that stays
   in `sessiond`, which already owns process/FS concerns (worktrees,
   isolation, lineage, per `session-relationship-design.md`). Rationale:
   the crate stays headless-pure (no `portable-pty` dependency; the
   existing `terminal/tests.rs` already drives the core exactly this
   way), the "brain" moves while the PTY "body" is spawned by the host,
   and PTY spawning (fork+exec, fd management) is a host/process concern.
   Consistent with decision 1 (the daemon owns PTY + core): `sessiond`
   the binary owns the PTY and *uses* this crate for the brain.
   Extraction done-definition (verification, not an owner decision): the
   crate builds standalone with **no floem/`ui` dependency** (that is what
   "extracted" means), and a golden test confirms the visual output —
   colors included, once resolved with the default theme — is unchanged
   from before the color cut.

## Step 0 implementation notes (2026-07-09)

The extraction (decisions 7's step 0, 8, 9) is implemented:
`crates/horizon-terminal-core` holds `TerminalCore`/emulation
(`core.rs`/`core/{color,events,input,render}.rs`), the kitty keyboard
protocol (`protocol/kitty_keyboard.rs`), the shared types
(`types.rs`/`types/*`), the sister contract (`contract.rs`:
`TerminalCommand`/`TerminalUpdate`/`SelectionCommand`), and the session
loop (`session_loop.rs`: `run_terminal_core`, coalescing, the sync-update
failsafe). `horizon`'s `terminal/` keeps only the spawn layer
(`session.rs`, `session/{environment,runtime,trace}.rs` — PTY ownership,
threads, environment, `HORIZON_PTY_TRACE`) and the view
(`terminal/view/*`), and re-exports the crate's public surface from
`terminal/mod.rs` so no call site outside `terminal/` had to change import
paths. A few judgment calls surfaced only once cutting the code, not
foreseen at the design-decision stage:

- **A second cross-crate config dependency, resolved the same way as
  color.** `TerminalCore::new` also read `terminal::config::TerminalConfig`
  directly (for `scrolling_history`) — a second cross-crate read decision 9
  didn't name (it only called out color as "the one cross-crate
  dependency"). Fixed with the same crate-local pattern as color:
  `TerminalCore::new` keeps a built-in default (`DEFAULT_SCROLLBACK_LINES`,
  matching the host's own default) for the crate's own tests, and
  `TerminalCore::with_scrollback`/a new `TerminalCoreOptions` struct (also
  carrying the color scheme, see below) is what a real session actually
  uses, fed from the host at `TerminalSession::spawn`.
- **Logical color reuses `alacritty_terminal`'s own `Color`/`NamedColor`/
  `Rgb` types directly** (`horizon_terminal_core::TerminalColor` is a
  `pub use` alias of `vte::ansi::Color`), rather than a hand-rolled parallel
  enum — a cell's color already arrives in exactly this shape from the VT
  parser, alacritty_terminal is not a UI dependency (so this doesn't
  compromise the zero-floem-dependency requirement), and it avoids an
  otherwise-pure duplication. The host's `terminal::view::color` module
  (now `theme::resolve` in `src/theme/ansi.rs`) moved
  `resolve_color`/`named_rgb`/`indexed_rgb` over unchanged, operating
  on the same type.
- **OSC 4/10/11/12 query replies still need real RGB, right now, from
  inside the crate** — that feature (not cell rendering) is the one place
  this crate still resolves a logical color to RGB, since a byte reply to
  the app can't be deferred to a UI paint. It resolves against
  `TerminalColorScheme`, a plain-data mirror of the host's `TerminalColors`
  (the same "crate-local newtype" pattern the task brief pointed at for
  `SessionId`, applied to config instead since `SessionId` never actually
  crosses into this crate). `TerminalCore::set_color_scheme` pushes the
  host's live theme in at `TerminalSession::spawn` time, and again on every
  live theme apply (`Reload Config`/the theme settings view) via
  `TerminalCommand::SetColorScheme` -- see the bullet below.
- **Per-session live OSC 4/10/11/12 palette overrides ride the frame and
  are honored at paint time.** The initial cut left a narrowing here:
  `core::render::resolve_color` used to check `Term::colors()` (the live
  per-session override table an app can set at runtime) before falling
  back to the theme, but that table was core-only state that didn't cross
  the `TerminalFrame` boundary once cells carry logical colors, so an app
  that redefined a palette slot at runtime stopped recoloring rendered
  text — only that same app's OSC 4/10/11/12 *query replies* still honored
  it. This has since been closed: `TerminalFrame::palette_overrides`
  carries the override table as a sparse logical-index → literal-RGB list
  (sorted, for the frame's `Eq`), populated from `Term::colors()` at
  snapshot time and consulted by `terminal::view::color::resolve_color`
  (now `theme::resolve`) before the theme. A literal override always wins
  for its slot; the theme
  governs only non-overridden slots, which stays coherent with decision
  8's multi-client theming (a differently-themed second client still shows
  the app's literal override for that slot). `TerminalCore`'s own OSC
  4/10/11/12 *query replies* are unaffected — a separate, still-crate-local
  read of the same `Term::colors()` state.
- **A live theme apply re-pushes the color scheme to already-spawned
  sessions.** `Reload Config` and the theme settings view's live apply
  both call `SessiondHandle::broadcast_terminal_color_scheme`, which sends
  a fresh `TerminalCommand::SetColorScheme` to every attached terminal
  session; `crates/horizon-sessiond` demuxes it onto the session loop's own
  channel, which calls `TerminalCore::set_color_scheme` again. Only OSC
  4/10/11/12 *query-reply* defaults needed this — painted cell colors
  already pick up a live theme change on the next PTY-driven snapshot via
  `TerminalFrame`'s logical colors, independent of this push.
- **`TerminalEvents` widened from `pub(crate)` to `pub`.** `TerminalCore::
  write_vt`/`flush_sync_update` are necessarily part of the crate's public
  API (the host's `initial_terminal_text` calls `write_vt` directly), so
  its return type can't be less visible than the methods returning it
  (`private_interfaces` lint, promoted to an error under `-D warnings`).

Extraction done-definition, verified: `cargo build -p horizon-terminal-core`
succeeds with zero `floem`/`ui` dependency (`cargo tree` confirms), every
pre-existing terminal test moved with the code and is green (53 tests in
the crate, plus session-loop/kitty-protocol tests), and a new golden test
(`terminal::view::color::tests::known_bytes_resolve_to_the_pre_cut_rgb_values_under_the_default_theme`,
host-side; the resolver is now `theme::resolve`) confirms a known byte
sequence's logical-color frame resolves to the exact RGB values
(`[224, 108, 117]` for ANSI red, etc.) the pre-cut
`TerminalCore` used to bake in directly.

## Step 1 implementation decisions (2026-07-11)

The owner approved these concrete boundary choices after the GPUI shell
became Horizon's sole frontend:

- **Neutral shared framing crate.** `horizon-session-protocol` owns the
  JSONL envelope, handshake, wire session identifier, and framing. Agent and
  terminal command/event types remain sister vocabularies in
  `horizon-agent` and `horizon-terminal-core`; neither domain crate depends
  on the other.
- **Sparse terminal frame patches.** Attach/reconnect sends a full
  `TerminalFrame`. Later pushes carry changed indexed rows, the final row
  count, and only changed cursor/mouse/kitty-mode/palette metadata. Frame
  text is reconstructed from rows by one shared terminal-core helper rather
  than repeated in every patch.
- **Creation inputs cross once; process state stays daemon-side.** Horizon
  sends a resolved terminal spawn specification (shell, arguments, TERM,
  scrollback, OSC-query color scheme, control socket, and fallback cwd).
  `sessiond`, which owns the child pid and session tables, resolves a
  spawn-source session's live cwd. No synchronous UI-thread cwd RPC or
  daemon pid leakage is introduced.
- **Startup remains non-blocking.** A shared `SessiondRuntime` queues initial
  and user-requested creates until its asynchronous connection is ready;
  window creation never waits on the daemon.
- **GPUI entities are the update boundary.** Each terminal session already
  lives in its own `Entity<TerminalSession>` and notifies only its observers.
  The Floem-era per-field signal-sharding proposal is obsolete; no second
  reactive store is added. Row patches are applied to the entity's frame and
  GPUI performs one entity notification.
- **Hard rename, no migration compatibility layer.** This is a pre-release,
  single-owner project without a deployed compatibility surface. The binary,
  socket, environment override, reload command, and diagnostics move
  directly from `agentd` to `sessiond`; no legacy socket drain probe,
  environment alias, or reload-command alias is retained. Existing agent
  persistence schemas and agent/provider configuration names do not change.
  The development environment is checked once for a surviving old daemon at
  integration time instead of carrying permanent migration code.
- **Daemon identity includes its name.** Handshake diagnostics use a
  `horizon-sessiond/<version>` binary id rather than the former ambiguous
  version-only value.

## Step 1 implementation notes (2026-07-12)

The daemon-side hosting slice is implemented. The shared session protocol is
version 3: lifecycle traffic uses `session_control`, while agent and terminal
traffic use qualified `agent_*` and `terminal_*` kinds. Before `Hello`,
`sessiond` accepts only shared controls; its successful handshake advertised
both `agent` and `terminal` capabilities via a `Hello.capabilities` field
(removed in protocol v6, 2026-07-18 -- every sender hardcoded the same two
values and nothing ever read them). Existing agent commands, events,
persistence, and resume behavior remain on the same connection behind the
agent-qualified kinds.

`TerminalControl::Create` carries the resolved spawn inputs recorded above;
`Attach` addresses an existing daemon-owned terminal by session UUID. The
daemon retains the PTY, child pid, command channel, and latest full frame when
the GPUI connection disappears. Spawn-source cwd resolution samples the live
source child in the daemon and falls back to the supplied cwd when the source
is absent or cannot be sampled.

Frame baselines are connection-local. Create and every Attach on a new
connection send a full `TerminalUpdate::Snapshot`; later core snapshots are
converted to `TerminalUpdate::FrameDiff` against that connection's last sent
frame. Disconnect drops only the send cursor and attachment set, not the PTY
or retained frame. `Shutdown` kills the child through a retained PTY child
killer, PTY EOF/error produces a final `Exited`, and the daemon then removes
the terminal from its process-lifetime session table. Shared `Drain` requests
shutdown for every hosted terminal before the daemon exits.

Real-socket E2E coverage exercises initial full snapshot, applicable row
diffs, disconnect/reconnect Attach, PTY survival, fallback/source cwd, and
Shutdown/Exited cleanup.

The GPUI integration uses one eager `src/sessiond` runtime for both domains.
It returns before connect/Hello, queues typed agent and terminal requests in
one raw FIFO, retries the initial connection indefinitely with capped backoff,
and never starts a competing lazy connection. After one connection has been
established, an unexpected disconnect reports errors to every registered
route and stops; Step 1 does not reconnect or replay automatically. Dropping
the runtime is non-destructive, while `Reload Session Runtime` is the explicit
Drain path.

Terminal GPUI entities now contain only the daemon command/update handle and
latest frame. Horizon sends `TerminalSpawnSpec` once, with the pre-mutation
spawn-source session id and a `current_dir` → `$HOME` → `.` fallback cwd;
sessiond owns PTY spawn, live cwd sampling, and process lifetime. Reload
removes terminal sessions from the workspace model because Drain terminates
their live children, retains agent sessions/panes, starts exactly one new
runtime, then lists and loads the persisted agents before rebuilding their
entities and views. Reload remains deliberately destructive for terminals;
terminal discovery and adoption after a UI-process restart are Step 2A work.

## Step 2A recovery decisions (2026-07-12)

The owner approved the first UI-restart recovery slice with these boundaries:

- **Discovery stays in the terminal sister contract.** Terminal `List` /
  `ListResult` and `Attach` / `AttachResult` traffic carries request ids so
  asynchronous replies can be matched without relying on ordering. Listing is
  deterministic. If a listed terminal exits before adoption, Attach returns
  `NotFound` and Horizon never registers a stale workspace entry.
- **Startup remains immediate and conservative.** Horizon opens a fresh
  terminal without waiting for discovery. Surviving daemon terminals are
  registered as detached sessions and adopted in the background, making them
  available through Session Manager without choosing one to foreground.
- **Workspace presentation is not daemon identity.** The session UUID remains
  stable, but `Terminal #N` display numbers are assigned by each UI process and
  may change after restart. Stable user-visible names and restoration of tabs,
  split layout, focus, and attachments are a separate Step 2B persistence
  design rather than additions to the daemon summary.
- **Recovery means a new UI client, not transparent transport healing.** This
  slice covers a UI process exiting and a new process discovering the terminals
  retained by the still-running daemon. Automatic reconnect after an
  established sessiond connection fails, stale-client takeover, and
  multi-client fan-out remain deferred.
- **Reload keeps its explicit destructive meaning.** `Reload Session Runtime`
  still drains sessiond and terminates live terminals; Step 2A does not attempt
  live-PTY transfer across daemon replacement or reinterpret reload as UI
  restart recovery.

## Step 2B workspace restoration decisions (2026-07-12)

The owner approved the workspace-persistence boundary in
`docs/workspace-persistence-design.md`; it shipped 2026-07-12. Horizon
persists a versioned UI-owned JSON DTO containing tabs, weighted split topology,
focus, attachments, detached-session metadata, display numbers, and titles.
Writes are synchronous temporary-file-plus-atomic-rename replacements without
`fsync`.

A valid saved workspace suppresses creation of the usual fresh startup terminal.
The UI holds the restored topology behind a mutation barrier until terminal and
agent inventories succeed, then attaches surviving sessions, prunes missing
ones and collapses their layout, registers inventory-only sessions as detached,
and creates one fresh terminal only if no pane survives. Inventory failure must
not overwrite the saved state. Resize results feed back into
`LayoutChild.weight`, making user-adjusted split ratios part of the durable
model.

This is a UI persistence feature, not a daemon identity expansion. Agent traffic
stays on shared protocol v4; the local agent-list API becomes fallible so an
empty inventory is distinguishable from failure. Multi-UI ownership, automatic
reconnect after an established connection fails, and non-destructive sessiond
replacement remain unsupported.

## Connection to delegation (stage 1)

The terminal's move to `sessiond` supplies a delegation precondition,
not a separate feature. agentd already gives delegated *agent* sessions
a home; the missing half is that a delegate driving a shell has no
equally-addressable *terminal* home. Once the terminal is `sessiond`
-hosted, a terminal session is `SessionId`-stable and supervisable over
the CLI control plane exactly like an agent session — which is what a
supervising agent needs to watch a delegate's terminal pane. The CLI
control plane's settled design (explicit targets, fixed socket,
`SessionId`-only reference) composes across this move unchanged (its
reversibility audit holds). Inter-agent messaging is designed together
with this daemon, per the roadmap's shared-foundations item.
