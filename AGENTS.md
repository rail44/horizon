# Horizon — Agent Guide

Horizon is a Floem-based desktop shell for tabbed and split-pane applications:
a keyboard-first command workspace where terminals, AI agent sessions, and
(future) WASM plugin views run as sessions attached to panes. The product
direction is recorded in `docs/ux-principles.md`; the implementation plan is
`docs/roadmap.md`, a living document — update it when decisions change.

This file records only low-churn facts: commands, conventions, and pointers.
State the repo itself expresses — module contents, public API surfaces, phase
progress — is intentionally not duplicated here; read the source or the
pointed-to document instead.

## Commands

```sh
cargo check
cargo build --workspace
cargo test
cargo run
```

`cargo build --workspace` is the canonical build command: `cargo run` alone
only rebuilds the root `horizon` binary, and Horizon's agent sessions run
entirely inside `horizon-agentd` (`crates/horizon-agentd`), a separate
workspace member Horizon spawns on demand (see
`docs/agent-runtime-split-design.md`). If that binary was never built (or is
stale after an agent-side change), `cargo run` still starts Horizon but
agent panes fail to spawn a runtime — run `cargo build --workspace` first
(and again after touching `crates/horizon-agent`/`crates/horizon-agentd`),
or use `Reload Agent Runtime` from the command palette to retry after
rebuilding.

There is no CI. The local quality gate below is mandatory before finishing
any work — run it yourself and make sure all four are clean:

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
ast-grep scan --error -r .config/ast-grep/overtracking.yml src crates
```

`--workspace` is load-bearing: bare `cargo clippy`/`cargo nextest run`
from the repo root silently skip the `horizon-agentd`/`horizon-agent`
crates. nextest runs each test in its own process (no cross-test env
leakage) but does not run doctests; the workspace currently has none —
add `cargo test --doc` here if that changes.

The `ast-grep` step is the leg-2 static-analysis backstop against the
"over-tracking" reactive anti-pattern (raw `frame()` read + `.items` walk in
a `create_memo`/`create_effect` closure, no `untrack`) — see
`docs/agent-ui-performance-design.md`. It only catches the direct form (the
rule file documents the intraprocedural limitation); it is a backstop, not
the primary defense.

The same gate runs as a pre-commit hook (`hooks/pre-commit`), which hard-fails
if `ast-grep` isn't on `PATH` rather than skipping the check. One-time setup
per clone:

```sh
git config core.hooksPath hooks
npm install -g @ast-grep/cli   # or: cargo install ast-grep --locked
```

## Configuration

Horizon reads one optional TOML file: `$XDG_CONFIG_HOME/horizon/config.toml`
(falling back to `~/.config/horizon/config.toml`), overridable via
`HORIZON_CONFIG`. Precedence is env var > config file > built-in default;
existing env vars keep winning. Secrets (`OPENAI_API_KEY`) are
environment-only and never read from the file. Config is applied at startup
only, with one exception: `Reload Config` (palette / `reload-config`
keybinding id / CLI `horizon reload-config`) re-reads the file and applies
`[theme]` (chrome, `[theme.ansi]`, and the derived terminal colors) and
`[keybindings]` live; every other section still needs a restart. See
`config.example.toml` at the repo root for every knob, and `src/config/` for
the loader.

## GUI Verification

Agents cannot see the GUI directly. Two scripts drive it headlessly:
`scripts/check-terminal-visual.sh` runs a one-shot visual check (terminal
dump + screenshot), and `scripts/run-terminal-smoke.sh` runs the full
scenario suite on top of it. Authoritative details — env vars, artifact
paths, system deps, and caveats — live in the `gui-verify` skill
(`.claude/skills/gui-verify/SKILL.md`); read it before using either script.

Manual smoke after `cargo run`: press `ctrl+'` to enter workspace mode
(`docs/workspace-mode-design.md`), then `:` to open the control surface —
a Commands-only palette; session management (attach/terminate detached
sessions) is a separate modal opened via its "Manage Sessions" command. See
README.md for the manual command checklist (`new tab`, `split pane`,
`detached`, `manage sessions`, ...).

## Module Map (`src/`)

Domain responsibilities (stable); browse each directory for its current
contents:

- `workspace/` — the core domain: tabs, panes, layout tree, session
  attachments, operations/queries, pane input routing, and workspace views.
- `terminal/` — PTY-backed terminal sessions: the spawn layer (PTY
  ownership, threads, environment, the `HORIZON_PTY_TRACE` tap) and
  rendering/input/IME views. `TerminalCore`/emulation, the session
  command/update contract, and the byte-channel-driven session loop live in
  `crates/horizon-terminal-core` (a library crate, no floem dependency) —
  see `docs/session-daemon-design.md`. The kitty-keyboard-protocol
  conformance matrix (`KITTY_COMPLIANCE`,
  `crates/horizon-terminal-core/src/protocol/kitty_keyboard.rs`) is
  resident, code-adjacent documentation; print it with `cargo test -p
  horizon-terminal-core print_compliance_matrix -- --nocapture`.
- `agent/` — Horizon's seam onto AI agent sessions: the client/reconnect
  logic for `horizon-agentd` (`agentd_client.rs`, `agentd_runtime.rs`) and
  agent views. The provider contract, providers, tools, and persistence
  themselves live in `crates/horizon-agent` (a library crate, no floem
  dependency) and are hosted by `crates/horizon-agentd` (the daemon binary
  every agent session actually runs in) — see
  `docs/agent-runtime-split-design.md`.
- `session/` — shared session primitives (`SessionId`, `Registry`, `Frames`)
  used across session kinds.
- `app/` — composition root: the command model (`commands.rs` defines
  `CommandId`, `command_actions.rs` executes), keymap, session spawning,
  app-level state and view.
- `control_surface/` — the command palette (opened with `:` from workspace
  mode, see `docs/workspace-mode-design.md`) and the session manager modal
  (attach/terminate any session, opened via its "Manage Sessions" command).
- `control_plane/` — the CLI control-plane listener: a fixed well-known
  Unix socket, one thread per connection, bridged onto the UI thread so
  commands still execute through the command model. The contract lives
  in `crates/horizon-control`; the client is `horizon <subcommand>`
  itself (client code in the lib-only `crates/horizon-ctl`). Panes get
  `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment. See
  `docs/cli-control-plane-design.md`.
- `ui/` — cross-domain UI primitives only. Domain-specific views live next
  to their domains.
- `plugins/` — WASM plugin groundwork; the future path for hot-reloadable
  panes. Not yet wired into the app shell.

## Conventions

- **Code and comments are English.** Do not quote task instructions
  (often written in Japanese) verbatim into code comments — restate the
  intent in English. Japanese as *test data* (IME, multibyte-boundary
  cases) is fine and deliberate; docs under `docs/research/` are
  Japanese by choice.
- **Keep module internals crate-local.** Default to `pub(crate)` (or private)
  and re-export a narrow surface from each `mod.rs`. See the long run of
  "Keep ... crate-local" commits for the pattern.
- **Split modules by responsibility.** Domain directories hold small focused
  files (e.g. `terminal/core/{events,input,render}.rs`) rather than large
  monolith modules; see the "Split ... by responsibility" commits.
- **Operations go through the command model.** User-visible operations are
  `CommandId` variants executed via `execute_command`
  (`src/app/command_actions.rs`). Buttons, keyboard shortcuts, and the palette
  are bindings to commands — do not add ad-hoc behavior in UI handlers.
- **Close vs. terminate are distinct.** Closing a pane/tab detaches sessions;
  `Terminate Active Session` is the explicit destructive command. Preserve
  this separation (see `docs/ux-principles.md`).
- **Tests are colocated** in `tests.rs` modules next to the code
  (`src/workspace/tests.rs`, `src/terminal/tests.rs`, `src/agent/tests.rs`,
  ...), declared as `#[cfg(test)] mod tests;`.
- **Design decisions are recorded under `docs/`** (e.g.
  `agent-pane-design.md`, `agent-provider-contract.md`,
  `agent-duckdb-state-design.md`). Add or update a doc when making a
  non-obvious architectural decision.

## Branch and Integration Flow

Development is organized as one **project session** (long-horizon
milestones, application-wide decisions, integration) plus per-domain
work sessions. If you are a domain or task session: implement on a
branch, never commit to or push `main` directly, and hand your branch
back through the review queue below. Subagent workers already follow
the same shape (worktree branch handoff, `.claude/agents/worker.md`);
this section extends it to every session working in this repository.
Before editing anything, confirm `git rev-parse --show-toplevel` points at
your own worktree, not the shared main checkout — a domain session has
edited the main checkout directly by mistake before, and that is exactly
the collision this whole flow exists to prevent.

Direction comes from `docs/roadmap.md`: a domain session picks an
item there and makes the concrete design decisions with the owner
in-session — there is no separate plans layer; **the review request is
the scope record**. The project session does not relitigate in-session
decisions at merge — its review covers the gate, cross-domain
integration, and coherence with the roadmap and architecture docs,
returning non-blocking concerns as notes — and reflects merges back
into the roadmap. Live session state is deliberately not tracked in
git.

**Review queue** (`.claude/review-queue/`, untracked): when a branch
is ready, write `<slug>.request.md` containing the branch name, commit
ref, the roadmap item it implements, the key design decisions made
in-session, a short summary, and the tail of your gate run. An
optional `## Observations` section (same two categories as worker
reports: out-of-scope findings, friction) is triaged at review time by
the project session into the backlog or the roadmap; mid-work findings
can also ride your branch as edits to `docs/tasks/backlog.md`. Don't
wait synchronously — the project session is notified, reviews in an
isolated worktree, merges and pushes on green, and writes
`<slug>.result` (`merged <hash>` / `rejected: <reason>` + notes) next
to your request. After writing a request, don't go idle blind: arm a
background watcher on your own `<slug>.result` (e.g. an `until
[ -e ... ]; do sleep 15; done` background task) so the verdict wakes
you — the owner should never need to prod a session back to life.
The pre-commit hook refuses commits on `main` unless
`HORIZON_INTEGRATION=1` is set; only the project session sets it.

## Open Work

Check `docs/roadmap.md` — it is the single direction document (in
flight / next / later). Status is not duplicated here.

Owner-filed dogfooding issues ride a **separate, faster lifecycle** from
the roadmap: they are written one-file-each under `docs/issues/` (see
`docs/issues/README.md`) by an issue-filing session, and the project
session triages them by priority and merge-conflict and dispatches the
chosen ones to workers through the same review queue. Filing an issue is
not a request to fix it now.
