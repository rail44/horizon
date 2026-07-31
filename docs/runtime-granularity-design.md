# Runtime granularity: crate/process/version seams

Status: consult held 2026-07-31 (fork session 6fa635f0). Questions 1-4
are **decided** — the decision record and implementation plan is
`docs/runtime-crate-alignment-design.md`; per-question outcomes are
annotated inline below. Question 5 (the WASM runtime story) stays open
for when that view kind arrives.

## Ratified baseline (do not relitigate, cite instead)

- **View kind = runtime** (`docs/view-runtime-principle.md`, owner
  2026-07-30): a pane's view kind names the runtime that hosts it; a
  runtime deserves its own process when its update frequency differs
  measurably from its neighbors' AND its sessions have survival value.
  Terminals and agents met the criterion → `horizon-terminald` /
  `horizon-agentd` (`docs/terminald-split-design.md`).
- **Lockstep wire versioning, no per-feature gates** (owner
  2026-07-30): `MIN_SUPPORTED_PROTOCOL_VERSION` rises with
  `SESSION_PROTOCOL_VERSION`; a mismatched hello is rejected and
  recovered by the client's auto-drain-and-respawn
  (`src/runtime/agent.rs`). Per-feature gate constants
  (`SCROLLBACK_WINDOW_MIN_VERSION`-style) were rejected as
  unscalable; same-machine self-spawned daemons don't need
  cross-version interop, they need honest restart. v18 (config-only
  `[provider]` reload) shipped under this policy.
- **Per-request "unsupported" errors are not cheaply available**: remoc
  rtc carries the reply channel inside the request payload, so an
  undecodable (unknown-index) request leaves nothing to answer on.
  Version mismatch must be caught at `hello`, not per call.

## The forcing problem

One version constant spans **both** hubs (`horizon-session-protocol`
serves `SessionHub` and `TerminalHub`; one schema artifact,
`session-wire.json`). Under lockstep this couples the daemons: an
agentd-only wire addition (v18 was exactly this) makes a new shell
reject the running **terminald** too, and the recovery auto-drains it —
every PTY dies at every version boundary, for changes that never
touched the terminal wire. That resurrects, at bump frequency, the
exact pain the terminald split removed. Flagged at the v18 decision
(2026-07-30) as the follow-up; this consult is that follow-up.

## Open questions

1. **Decided 2026-07-31**: per-hub version pairs living in each hub's
   own crate, two schema artifacts with unchanged inner keys,
   section-scoped checking (`docs/runtime-crate-alignment-design.md`
   judgments 3 and 6, phase 2). The v17 "drain stays put" concern
   becomes per-hub as predicted.
   Original question: **Per-daemon wire versions.** Split the version pair per hub
   (agent wire vs terminal wire), so an agentd bump leaves terminald
   pairings negotiable. What carries the split: two constant pairs in
   one protocol crate, or two crates (see 2)? What happens to the
   schema artifact and `check-wire-schema.sh` — two artifacts, or one
   artifact with two version keys and section-scoped diffing? The v17
   "drain stays put" index-alignment concern
   (`horizon-session-protocol/src/lib.rs` v17 note) is per-hub and
   should get simpler, not harder — verify.
2. **Decided 2026-07-31**: `horizon-session-protocol` dissolves —
   domain-free foundation into a new `horizon-wire`, each hub into its
   domain crate (`horizon-agent::wire` / `horizon-terminal-core::wire`)
   (judgments 2-3 there).
   Original question: **Protocol crate granularity.** Does `horizon-session-protocol`
   itself split (agent wire / terminal wire / shared plumbing:
   `VersionRange`, `WireCodec`, caps, hello discipline)? Or stay one
   crate with two version pairs? Weigh against the view-runtime
   principle: the wire slices already belong to different runtimes
   with different change cadences (terminal wire: append-only
   discipline, tmux precedent; agent wire: still evolving fast).
3. **Decided 2026-07-31**: sessions are attachment records; the
   "sessiond" vocabulary is swept in phase 3 (`src/sessiond/` →
   `src/runtime/`, `SessiondState` → `AgentdState`).
   `horizon-control`'s versioning story explicitly stays as-is
   (a shell-local socket, not a runtime hub). Process composition is
   unchanged.
   Original question: **Crate/process map coherence.** With the principle ratified, audit
   the seams: `src/sessiond/` hosts both daemons' client runtimes
   under a stale name; the control plane (`horizon-control`) is a
   third socket with its own ad-hoc versioning story; `horizon-agent`
   vs `horizon-agentd` and `horizon-terminal-core` vs
   `horizon-terminald` layering. What, if anything, moves — and what
   explicitly stays, recorded with reasons.
4. **Decided 2026-07-31**: delete, with their fallback paths, in phase
   2 when the terminal wire moves — dead code under MIN>=17 lockstep,
   so the deletion is wire- and behavior-neutral. *Amended the same
   day*: phase 2 kept one residue — the structured-input check
   survived as `negotiated.is_some()`, since a keystroke typed before
   the terminal runtime's first `hello` really does reach it. The
   owner ruled that not worth keeping: a negotiated version carries no
   information for a feature decision under lockstep, and the
   pre-`hello` window is better served by the structured encoding
   anyway (it carries the platform's associated text, which is what an
   IME commit needs). Deleted in phase 3, along with the negotiated
   version's whole client-side plumbing, which had no other reader.
   Original question: **Vestigial gate constants.** `SCROLLBACK_WINDOW_MIN_VERSION` (12)
   and `TERMINAL_STRUCTURED_INPUT_VERSION` (13) and their fallback
   code paths are dead under MIN=17+ lockstep. Delete with their
   fallbacks, or keep until the per-daemon split settles the
   versioning shape?
5. **The third runtime kind.** `plugins/` (WASM views) is the future
   pane family. What does the principle imply for its process story
   (in-shell? one plugind? per-plugin?) and its version/compat story?
   No implementation now — but the granularity decisions above should
   not paint it into a corner.

## Pointers

`docs/view-runtime-principle.md` · `docs/terminald-split-design.md` ·
`docs/agent-runtime-split-design.md` · `docs/remoc-adoption-design.md`
§3/§4/§6 · `crates/horizon-agent/src/wire/hub.rs` (the whole v4–v18
version-note history, kept undivided; the terminal pair in
`crates/horizon-terminal-core/src/wire.rs` points back at it) ·
`docs/cli-control-plane-design.md` · AGENTS.md "Branch and
Integration Flow".
