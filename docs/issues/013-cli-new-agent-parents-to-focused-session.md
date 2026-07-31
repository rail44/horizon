---
id: 013
title: CLI new-agent parents the session to whatever pane was focused, not the pane that issued the command
status: resolved
severity: medium
area: cli, control-plane, workspace, session-manager
---

## Repro
1. Run a CLI (e.g. a Claude Code session) inside a Horizon terminal pane;
   its environment carries `HORIZON_SESSION_ID` for that pane.
2. From that CLI, dispatch two independent agents with
   `horizon new-agent --prompt ...`, focusing a different pane between the
   two dispatches (e.g. the first agent's pane, opened by the first
   dispatch, is focused when the second dispatch runs).
3. Open Manage Sessions and look at the lineage tree.

## Observed
Each new agent is parented to whatever session happened to be *focused*
at the instant of dispatch: the second independent agent shows as a
child of the first (observed live: scrollfix session f4aee3e4 nested
under configreload session 0e390540), and the first carries a parent id
(c1881634) that no longer resolves to anything. Lineage is
focus-dependent, so the tree records coincidences of window focus, not
who actually spawned whom.

## Expected
The parent should be the session that *issued* the spawn. A CLI running
inside a pane already has that identity in its environment: an agent
launched from a Claude Code session in a terminal pane should be a child
of **that terminal session** — the terminal is the orchestrator, and the
modal's tree should show its agents under it, regardless of which pane
had focus when the command ran. A dispatch from outside any pane (no
`HORIZON_SESSION_ID`) has no issuer and should be a root, never adopted
by the focused pane.

## Notes
Mechanism: `resolve_spawn_source(explicit_source, active_session)` in
`src/workspace/session_lifecycle.rs` is `explicit_source.or(active_session)`,
and the CLI never fills `explicit_source` from `HORIZON_SESSION_ID` — it
reads that variable only to resolve bare `--split`
(`crates/horizon-cli/src/cli.rs`). The fix direction is to carry the
caller's session id through the control-plane spawn request as the
explicit source (present ⇒ use it; absent ⇒ root), and drop the
active-session fallback for control-plane dispatches entirely — it can
stay for palette/UI launches, where "child of the current pane" is the
intended reading of the gesture.

Side effect to handle: an isolated agent worktree branches from its
spawn source's directory (issue 006's deliberate behavior, resolved via
`state.session_directory(source_id)` in agentd). A *terminal* parent is
unknown to agentd, so base derivation must fall back sanely (today an
unknown source resolves to `None` → root-spawn behavior; the terminal's
cwd would be the honest base if the wire ever carries it). Also means
focus-dependent parenting is a worktree-contamination vector today: the
child branches from whatever pane was focused, including its unpushed
commits.

Filed from the project session at the owner's direction (2026-07-30),
after the lineage tree surfaced wrong during dogfood integration.

## Resolution

Fixed in `5cebd55` (merged as `78d6276`). The CLI carries
`HORIZON_SESSION_ID` as an `"issuer"` key in the control-plane invoke
args (free-form `serde_json::Value`, so no protocol bump), and the shell
resolves a control-plane spawn's source as **explicit `--split` target >
issuer > none (root)** via a dedicated `control_plane_spawn_source`
(`src/workspace/session_lifecycle.rs`). The active-session fallback is
gone for control-plane dispatches and kept for palette/UI launches,
where "child of the current pane" is the intended reading of the
gesture. `resolve_spawn_source`'s second argument was renamed from
`active_session` to a neutral `fallback` so each caller decides what its
fallback means.

Per the owner's decision (2026-07-30), the worktree-base side effect is
left as-is: a terminal issuer is not a session `horizon-agentd` hosts,
so `session_directory` returns `None` and isolated-worktree creation
falls back to root-spawn behavior. Carrying the terminal's cwd on the
wire is deliberately a separate task; this change touches no wire type.

Tests: `resolve_spawn_source`'s existing test split into explicit-wins /
fallback / root, a new `control_plane_spawn_source` test covering all
three precedence cases, and CLI integration tests asserting the
`"issuer"` key reaches the wire (and is omitted when
`HORIZON_SESSION_ID` is unset or empty).
