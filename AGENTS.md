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
cargo test
cargo run
```

There is no CI. The local quality gate below is mandatory before finishing
any work — run it yourself and make sure all three are clean:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

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

Manual smoke after `cargo run`: `Ctrl+Shift+P` opens the control surface;
`Tab` toggles between Commands and Workspace modes. See README.md for the
manual command checklist (`new terminal`, `split`, `detached`, ...).

## Module Map (`src/`)

Domain responsibilities (stable); browse each directory for its current
contents:

- `workspace/` — the core domain: tabs, panes, layout tree, session
  attachments, operations/queries, pane input routing, and workspace views.
- `terminal/` — PTY-backed terminal sessions (emulation core, session
  runtime, rendering/input/IME views).
- `agent/` — AI agent sessions: the Horizon-owned provider contract
  (`contract.rs`), providers, tools, persistence, and agent views.
- `session/` — shared session primitives (`SessionId`, `Registry`, `Frames`)
  used across session kinds.
- `app/` — composition root: the command model (`commands.rs` defines
  `CommandId`, `command_actions.rs` executes), keymap, session spawning,
  app-level state and view.
- `control_surface/` — the Ctrl+Shift+P surface: command palette and
  workspace overview.
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
