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
any work — run it yourself and make sure all three are clean:

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

`--workspace` is load-bearing: bare `cargo clippy`/`cargo nextest run`
from the repo root silently skip the `horizon-agentd`/`horizon-agent`
crates. nextest runs each test in its own process (no cross-test env
leakage) but does not run doctests; the workspace currently has none —
add `cargo test --doc` here if that changes.

The same gate runs as a pre-commit hook (`hooks/pre-commit`). One-time setup
per clone:

```sh
git config core.hooksPath hooks
```

## Configuration

Horizon reads one optional TOML file: `$XDG_CONFIG_HOME/horizon/config.toml`
(falling back to `~/.config/horizon/config.toml`), overridable via
`HORIZON_CONFIG`. Precedence is env var > config file > built-in default;
existing env vars keep winning. Secrets (`OPENAI_API_KEY`) are
environment-only and never read from the file. Config is applied at startup
only — restart Horizon after editing it. See `config.example.toml` at the
repo root for every knob, and `src/config/` for the loader.

## GUI Verification

Agents cannot see the GUI directly. Two scripts drive it headlessly:
`scripts/check-terminal-visual.sh` runs a one-shot visual check (terminal
dump + screenshot), and `scripts/run-terminal-smoke.sh` runs the full
scenario suite on top of it. Authoritative details — env vars, artifact
paths, system deps, and caveats — live in the `gui-verify` skill
(`.claude/skills/gui-verify/SKILL.md`); read it before using either script.

Manual smoke after `cargo run`: press `ctrl+'` to enter workspace mode
(`docs/workspace-mode-design.md`), then `:` to open the control surface;
`Tab` toggles between Commands and Workspace modes. See README.md for the
manual command checklist (`new terminal`, `split`, `detached`, ...).

## Module Map (`src/`)

Domain responsibilities (stable); browse each directory for its current
contents:

- `workspace/` — the core domain: tabs, panes, layout tree, session
  attachments, operations/queries, pane input routing, and workspace views.
- `terminal/` — PTY-backed terminal sessions (emulation core, session
  runtime, rendering/input/IME views).
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
- `control_surface/` — the command palette and workspace overview,
  opened with `:` from workspace mode (see
  `docs/workspace-mode-design.md`).
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

## Open Work

Check `docs/roadmap.md` for current phases and their status, and the README
"Next Integration Points" section for nearer-term items. Phase status is not
duplicated here.
