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
only rebuilds the root `horizon` binary, and Horizon's terminal and agent
sessions run inside `horizon-sessiond` (`crates/horizon-sessiond`), a separate
workspace member Horizon spawns on demand (see
`docs/agent-runtime-split-design.md`). If that binary was never built (or is
stale after an agent-side change), `cargo run` still starts Horizon but
agent panes fail to spawn a runtime — run `cargo build --workspace` first
(and again after touching `crates/horizon-agent`/`crates/horizon-sessiond`),
or use `Reload Session Runtime` from the command palette to retry after
rebuilding.

There is no CI. The local quality gate below is mandatory before finishing
any work — run it yourself and make sure all three are clean:

```sh
cargo fmt
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
```

`--workspace` is load-bearing: bare `cargo clippy`/`cargo nextest run`
from the repo root silently skip the `horizon-sessiond`/`horizon-agent`
crates. nextest runs each test in its own process (no cross-test env
leakage) but does not run doctests; the workspace currently has none —
add `cargo test --doc` here if that changes.

The same gate runs as a pre-commit hook (`hooks/pre-commit`). One-time
setup per clone:

```sh
git config core.hooksPath hooks
```

## Build setup

`crates/horizon-agent` links DuckDB dynamically by default (non-bundled):
install a system libduckdb before building. On Void Linux:
`sudo xbps-install duckdb-devel`; elsewhere, install your distro's DuckDB
dev package or the equivalent (macOS: `brew install duckdb`). No env vars
are needed once the lib/headers are on the default system paths (Linux:
`/usr/lib` + `/usr/include`, auto-discovered — verify with `ldd` on a
built binary if unsure). For a manually-placed prebuilt lib elsewhere, set
`DUCKDB_LIB_DIR`/`DUCKDB_INCLUDE_DIR` (libduckdb-sys's standard discovery).
Version skew note: libduckdb-sys 1.10504.0 (the pin as of this writing)
encodes an expected DuckDB 1.5.4; Void's `duckdb-devel` currently ships
1.5.0 — confirmed ABI-compatible (full `horizon-agent` suite green against
it), but re-verify on any libduckdb-sys bump or when Void catches up.
Machines without a system libduckdb can build with
`cargo build --workspace --features horizon-agent/bundled-duckdb`, which
compiles DuckDB from source instead (slow — it's the single largest
compile unit in this workspace).

Every worktree of this repo (main checkout, `git worktree` clones,
`.claude/worktrees/agent-*` workers) shares one build cache: the tracked
`.cargo/config.toml` sets `build.build-dir` to a path under `CARGO_HOME`
(`{cargo-cache-home}/horizon-build-dir`), so intermediate build artifacts
for crates.io/git dependencies (the bulk of a from-scratch build) are
built once and reused by every worktree, while final artifacts (the
binaries) stay in each worktree's own `target/`. No manual setup needed —
the config is checked in. Concurrent builds across worktrees are safe
(cargo's own advisory lock serializes overlapping writers) but will queue
behind each other on a cold cache; this is an accepted tradeoff for the
disk/CPU savings. This makes the old worker convention of reflinking the
main checkout's `target/` into a fresh worktree mostly redundant for the
heavy artifacts (only the now-small per-worktree `target/` benefits).

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
`config.example.toml` at the repo root for every knob, and
`crates/horizon-config` for the loader.

## GUI Verification

Agents cannot see the GUI directly. The shell has built-in headless taps
(`HORIZON_GPUI_DUMP=<path>` mirrors every terminal frame plus a span
color table to a file; `HORIZON_GPUI_DRIVE=<bytes>` types input into the
first session shortly after startup), and `scripts/check-gpui-terminal.sh`
drives them as a one-shot check (marker text plus 256-color and truecolor
span assertions). Details live in the `gui-verify` skill
(`.claude/skills/gui-verify/SKILL.md`). `HORIZON_INPUT_TRACE=1` (or a file
path) traces every hop of the real winit→gpui→PTY key/IME pipeline —
distinct from `HORIZON_GPUI_DRIVE`, which bypasses that pipeline entirely
— key names and event kinds only, never typed text (see
`src/input_trace.rs`/`crates/horizon-winit-platform/src/input_trace.rs`).

`scripts/check-workspace-restore.sh` is the isolated UI-restart recovery
check: it creates two terminal tabs plus a split, restarts the UI against the
same sessiond and persisted workspace, and verifies stable session ids,
layout, and a restored terminal frame.

Manual smoke after `cargo run`: press `ctrl+'` to enter workspace mode
(`docs/workspace-mode-design.md`), then `:` to open the control surface —
a Commands-only palette; session management (attach/terminate detached
sessions) is a separate modal opened via its "Manage Sessions" command. See
README.md for the manual command checklist (`new tab`, `split pane`,
`detached`, `manage sessions`, ...).

## Module Map (`src/`)

Domain responsibilities (stable); browse each directory for its current
contents:

The shell is GPUI-based (the Floem shell retired at tag
`floem-shell-final`; the migration record is
`docs/gpui-migration-design.md`). Domain logic lives in shared crates;
`src/` holds only the view projections and wiring:

- `workspace.rs` — the shell root (`WorkspaceShell`): renders the shared
  workspace model (tab strip, recursive splits on gpui-component's
  resizable primitives), owns the session stores (terminal + agent, the
  close-vs-detach seam), workspace mode, command dispatch (`execute`),
  and the modals' lifecycles. The model itself — tabs, panes, layout
  tree, session attachments, operations/queries, mode state, spatial
  navigation, the pure command model, and the `workspace.snapshot`
  payload — is `crates/horizon-workspace`.
- `sessiond/` — the shell's one eager shared client runtime: non-blocking
  connect/spawn, raw agent/terminal routing, and explicit drain.
- `terminal/` — the terminal pane: the daemon-backed per-session model entity
  (`session.rs`) and the view (grid painting, key/mouse/IME handling,
  `input.rs` mapping). PTY ownership lives in `crates/horizon-sessiond`;
  emulation and the session loop live in `crates/horizon-terminal-core` — see
  `docs/session-daemon-design.md`;
  print the kitty conformance matrix with `cargo test -p
  horizon-terminal-core print_compliance_matrix -- --nocapture`.
- `agent/` — the agent pane: per-session model entities (`session.rs`, folding
  events through the shared `LiveState`), and the view (Markdown transcript, composer,
  approvals). Contract/providers/tools/persistence live in
  `crates/horizon-agent`, hosted by `crates/horizon-sessiond` — see
  `docs/agent-runtime-split-design.md`.
- `palette.rs` / `session_manager.rs` / `view_chooser.rs` — the control
  surface modals, all delegates over gpui-component's searchable List.
- `control_plane.rs` — the GPUI-side bridge and dispatcher for the CLI
  control plane; the transport is shared in `horizon-control::host`, the
  client is `horizon <subcommand>` itself (`crates/horizon-ctl`). Panes
  get `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment. See
  `docs/cli-control-plane-design.md`.
- `keymap.rs` — `[keybindings]` chord/command translation;
  `theme.rs` — config-driven color scheme; `terminal_focus.rs` — the
  focus-reporting decision; `main.rs` — CLI-vs-GUI entry point.
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
Codex sessions additionally follow `.codex/delegation-workflow.md` for
model-tier guidance, delegation boundaries, and root/worker responsibilities.
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
