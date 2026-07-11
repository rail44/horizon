# CLI Control Plane — Design

Status: decisions settled 2026-07-06 (owner approved the recommended
options conditional on a reversibility audit, recorded below).
Implementation has not started; ordering against the workspace-mode
work is an open scheduling question. Survey material with the full
option space and prior art: `docs/research/cli-control-plane.md`.

The CLI is the third surface over the unified command model (after the
palette and keybindings): "the command model is the core; surfaces are
replaceable" (owner). It is also the seam Phase-1 delegation needs —
an agent driving the workspace uses the same contract.

## Decisions

### Channel: Unix domain socket + newline-delimited JSON

Reuses the `wire.rs` framing philosophy (transport-generic envelope,
structural version field, hello handshake) and the socket-path
discipline sessiond already practices. Unix file permissions are the
authorization boundary, as in sessiond and most surveyed prior art
(tmux, i3/sway, emacsclient-local). Horizon's main process has no
tokio runtime: the listener is a `std` UnixListener on a dedicated OS
thread, and accepted commands cross into the reactive graph via the
established `ext_event` bridge — so `execute_command` keeps running
serialized on the UI thread, preserving the codebase's existing
synchronization assumption even with external command sources.

### Endpoint: in-process now, contract written for the daemon later

The listener lives in the Horizon app process today. The contract is
written so the endpoint can move to the future tmux-style session
daemon without breaking clients: sessions are referenced by
`SessionId` only, the contract assumes multiple concurrent clients
(sessiond's single-connection simplification is deliberately not
inherited), and discovery goes through a socket path that clients
never hardcode (see below).

### Contract: shared framing, sibling vocabulary

The envelope/framing layer (version field, handshake, line framing) is
shared with the agent wire; the command vocabulary is a separate
sibling contract with its own version. Workspace control and agent
session hosting are different domains and evolve independently. The
framing module may need extraction into a small shared crate so the
CLI binary does not depend on all of `horizon-agent` — implementation
detail, noted, not decided.

### Command exposure: stable external names via a mapping table

External clients speak stable string command names mapped to
`CommandId` internally (guardrail 6 of the agent-runtime split:
"a mapping table, not an implementation"). Internal renames never
break external clients. External names are declared **provisional
until Phase-1 delegation lands** — the one real irreversibility here
is a published name someone scripts against, mitigated by the
handshake version and the current single-user reality.

### v1 operation shapes: fire-and-forget + query; subscription later

v1 ships one-shot commands and queries (session list, command-state
snapshot). Event subscription (i3 `SUBSCRIBE`-style upgrade on the
same connection) is deliberately v2; supervision loops can poll
queries in the interim, and the vocabulary reserves room so adding
subscription is additive under version negotiation.

### Composite create-with-prompt

`new-agent --prompt "..."` creates the session and delivers the first
user message Horizon-side, as a new explicit `CommandInvocation`. The
two-step path (create, then message) remains available. Rationale: the
mission-dispatch use case is the driving one, and readiness races
between create and first message belong to Horizon, not to every
caller (the SessionNew persistence race taught this once already).

### Discovery: instance env var injection

Horizon exports its control socket path into every pane's environment
(`HORIZON_SOCKET`-style). A CLI invoked inside a pane targets the
enclosing instance by default — tmux/zellij/i3's convergent answer —
which makes the stable/dev nested-instance workflow resolve itself:
the inner instance shadows the variable for its own panes. An explicit
flag/env override always wins (same shape as `horizon-sessiond
--socket`).

### Targets are explicit in v1

External invocations name their targets (`session_id`, pane index) —
no implicit "first found" or cursor-relative resolution over the wire.
The workspace-mode design's cursor stays a human-surface concept;
cursor-relative sugar for interactive CLI use can be added later
(additive), whereas retiring an implicit form after scripts depend on
it would be a breaking change. Scripts and supervising agents get
environment-independent semantics from day one.

### Authorization: socket permissions + client-side confirmation

The server relies on Unix socket file permissions (per prior art). The
`CommandSpec.destructive` flag travels in query responses so a CLI
front-end can prompt for confirmation; headless callers pass an
explicit acknowledgment flag by convention. Server-side policy
(allowlists, per-capability grants) can be layered later through
handshake capabilities if remote/ACP scenarios ever demand it.

## Second revision (2026-07-06, owner-approved)

Four amendments settled after v1 shipped and was exercised end-to-end:

1. **Single binary, subcommand client.** Running multiple Horizons is
   an anti-pattern in this design's philosophy, so a second binary name
   earns nothing: `horizon` with no arguments launches the GUI as
   today; `horizon <subcommand> ...` runs the control-plane client and
   exits (tmux's model). The separate `horizon-ctl` binary is retired;
   its client code survives as a library the root binary dispatches to.
2. **Fixed well-known socket path.** The single-instance norm justifies
   `$XDG_RUNTIME_DIR/horizon/control.sock` (sessiond's discipline,
   including stale-socket handling) instead of the per-pid path — so
   the client works from anywhere, not just inside panes.
   `HORIZON_SOCKET` remains the override and is still injected into
   panes/sessiond, which is what keeps a nested dev instance addressable
   (it shadows the variable for its own panes). A second instance
   finding a responsive owner does not steal the socket: it starts
   without a control listener and logs a warning.
3. **`activate` rides on creating/attaching operations.** Origin
   decides the default: human surfaces dive (see the workspace-mode
   design), control-plane calls default to `activate=false` so
   scripted/agent-driven view creation never steals the owner's focus.
   The CLI flag is `--active`.
4. **Placement vocabulary.** `attach <session-id>` joins the external
   names; `new-terminal`/`new-agent` gain `--split` placement. "Here"
   is resolved client-side from `HORIZON_SESSION_ID` (newly injected
   into pane environments — the pane's session id is the stable
   external pane reference) and sent as an explicit target, keeping
   the explicit-targets rule intact.

## Deferred

- **Subscription streams** — v2, additive (see above).
- **Approval policy for supervised agents** (what a supervising agent
  does when its delegate hits WaitingForApproval — auto-approve
  policies vs human escalation) is a Phase-1 delegation question, not
  a CLI protocol question. The protocol only guarantees the state is
  visible in queries/streams so an escalation can exist.
- **Cursor-relative targeting sugar** for interactive use.

## Reversibility audit (the approval condition)

Everything above is additive or version-negotiated except the external
command names, whose compatibility burden begins when something
scripts against them; mitigated by the handshake version and the
provisional-names declaration. Nothing regresses the session-daemon
future (the contract is designed for the move), the ACP direction
(framing stays separate from any wire's semantics), the mode/cursor
design (explicit targets keep cursor human-side), or the
reusable-asset boundary (workspace control never enters
`crates/horizon-agent`'s provider contract).
