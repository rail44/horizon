# Session Daemon — Design Decisions

Status: decided 2026-07-07 (owner consultation in the project session);
the `horizon-terminal-core` extraction-slice decisions (8, 9, and the
color amendment to 4) added 2026-07-09. The exploration material and
option analysis is `docs/research/session-daemon.md`; this file records
the decisions and is the scope reference for the migration.
Implementation not started.

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
