# Horizon — Agent Guide

Horizon is a Floem-based desktop shell for tabbed and split-pane applications:
a keyboard-first command workspace where terminals, AI agent sessions, and
(future) WASM plugin views run as sessions attached to panes. The product
direction is recorded in `docs/ux-principles.md`; the implementation plan is
`docs/roadmap.md`, a living document — update it when decisions change.

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

## GUI Verification

Agents cannot see the GUI directly. Use the scripted harnesses:

- `scripts/check-terminal-visual.sh` — starts Horizon on an isolated Xvfb
  display, writes the terminal model to `terminal.txt`, captures the window to
  `screenshot.png`, and leaves artifacts under `/tmp/horizon-visual-*`.
  Driven by env vars: `HORIZON_TEST_TEXT`, `HORIZON_TEST_XDOTOOL`,
  `HORIZON_EXPECT_DUMP_CONTAINS`, `HORIZON_EXPECT_STATUS_CONTAINS`, etc.
  (read the script for the full set).
- `scripts/run-terminal-smoke.sh` — scenario suite (shell input, new-terminal
  focus, split-pane status, ANSI color, nvim screen) built on the script above.
  Artifacts land under `/tmp/horizon-terminal-smoke-*`, one directory per
  scenario with `terminal.txt`, `status.txt`, `screenshot.png`, and logs.

Required system deps: `Xvfb`, `xdotool`, `xwd`, ImageMagick (`magick` or
`convert`); `nvim` is optional (its scenario is skipped when missing).

Manual smoke after `cargo run`: `Ctrl+Shift+P` opens the control surface;
`Tab` toggles between Commands and Workspace modes. See README.md for the
manual command checklist (`new terminal`, `split`, `detached`, ...).

## Module Map (`src/`)

- `workspace/` — the core domain: tabs, panes, layout tree, session
  attachments, workspace operations and queries, pane input routing, and the
  workspace views (tab strip, pane chrome, agent controls, terminal output).
- `terminal/` — PTY-backed terminal session: `core/` (alacritty_terminal +
  termwiz emulation, events, rendering), `session/` (portable-pty runtime and
  contract), `types/` (frames, sizes, mouse), `view/` (Floem rendering, input,
  IME preedit, metrics).
- `agent/` — AI agent sessions: `contract.rs` (Horizon-owned provider
  command/event/frame contract), `live.rs` (running session), `providers/`
  (rig-based and mock providers), `tools/` (agent tool catalog/execution),
  `policy.rs`, `persistence/` (DuckDB-backed event log and projection),
  `view/` (transcript, markdown, styling).
- `session/` — shared session primitives: `SessionId`, session `Registry`,
  and `Frames` shared across session kinds.
- `app/` — composition root: app state and view, `commands.rs`
  (`CommandId`/`CommandSpec` definitions), `command_actions.rs`
  (`execute_command`), `keymap.rs`, input routing, `runtime/` (terminal/agent
  session spawning), status bar.
- `control_surface/` — the Ctrl+Shift+P surface: command palette and
  workspace overview (modes, query filtering, items, actions, views).
- `ui/` — cross-domain UI primitives only (fonts, theme, list rows,
  selectable list). Domain-specific views live next to their domains.
- `plugins/` — WASM plugin manifests and wasmtime validation; the future path
  for hot-reloadable panes. Not yet wired into the app shell.

`src/lib.rs` exposes only `app_view` and `SessionId`; everything else is
`pub(crate)`.

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

Check `docs/roadmap.md` for current phases. Phases 1–4 (command model,
palette MVP, toolbar de-scaffolding, close semantics) are done; Phase 5
(typed palette expansion) is partially done; Phase 6 (recursive layout
rendering), Phase 7 (plugin view MVP), and Phase 8 (agent session MVP
completion) are open. The README "Next Integration Points" section lists
nearer-term items such as workspace persistence and recursive split
rendering.
