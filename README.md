# Horizon

Horizon is a Floem-based desktop shell for tabbed and split-pane
applications: a keyboard-first command workspace where terminals, AI agent
sessions, and (future) WASM plugin views run as sessions attached to panes.

## MVP Shape

- Floem owns the native window, tab actions, and split-pane layout. Every
  operation runs through one command model, surfaced today by workspace mode's
  `:` palette (`docs/workspace-mode-design.md`) and by the `horizon` CLI
  control plane (`docs/cli-control-plane-design.md`).
- Built-in sessions provide the first two MVP surfaces:
  - Terminal: PTY-backed terminal core using `portable-pty`,
    `alacritty_terminal`, and `termwiz`.
  - AI Agent: provider-backed agent sessions using Horizon-owned
    command/event/frame contracts.
- WASM plugin manifests and validation are retained as the future path for
  hot-reloadable pane development, including eventually developing Horizon
  from inside Horizon.

## Commands

```sh
cargo check
cargo build --workspace
cargo test
cargo run
```

`cargo run` alone only rebuilds the root `horizon` binary; agent sessions run
in `horizon-agentd` (`crates/horizon-agentd`), a separate workspace member
Horizon spawns on demand. Run `cargo build --workspace` at least once (and
again after touching `crates/horizon-agent`/`crates/horizon-agentd`) or agent
panes will fail to find a runtime to spawn. `just dev` builds the whole
workspace and launches the freshly built binary directly (bypassing `cargo
run`'s environment leakage into Horizon and agentd); pass CLI subcommand
arguments after it, e.g. `just dev sessions`.

After `cargo run`, press `ctrl+'` to enter workspace mode, then `:` to open
the control surface (see `docs/workspace-mode-design.md`). Commands mode
supports these manual smoke checks:

- `new terminal`: opens another terminal tab.
- `new agent`: opens an agent tab.
- `split`: splits the active pane.
- `close active pane`: closes the active pane and leaves its session detached
  when another pane remains.
- `detached`: shows detached sessions such as `Terminal #2` and attaches the
  selected session back into the active tab as a split.
- `tab 1`, `tab 2`, ...: switches to the matching tab.
- `terminate active session`: terminates the active session.
- `reload agent runtime`: restarts `horizon-agentd` and reconnects every
  agent session — use after rebuilding the agent crates, or to recover from
  a lost connection.

Use `Tab` while the control surface is open to switch between Commands and
Workspace. The Workspace mode lists open tabs, panes inside split tabs, and
detached sessions; Enter switches to the selected tab or pane, or attaches the
selected detached session as a split.

The same command model is also reachable from outside the GUI: `horizon
<subcommand>` (no arguments launches the GUI itself) is a thin client over a
Unix-socket control plane, useful for scripting or driving Horizon from an
agent. Panes get `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment,
so a subcommand run from inside a pane targets the enclosing instance and
`--split` (bare) resolves to "here" by default. Subcommands: `new-terminal`,
`new-agent [--prompt <text>]`, `attach <session-id>`,
`terminate-session <session-id>`, `terminate-all-detached`,
`approve`/`deny <session-id> <call-id>`, `cancel-turn <session-id>`,
`reload-agent-runtime`, `sessions`, `state` (each takes `--split`/`--active`
where placement/focus applies). See `docs/cli-control-plane-design.md` for
the full contract.

For automated visual inspection of the terminal pane:

```sh
scripts/check-terminal-visual.sh
```

The script starts Horizon on an isolated Xvfb display, writes the terminal model
to `terminal.txt`, captures the Horizon window to `screenshot.png`, and leaves
all artifacts under `/tmp/horizon-visual-*` by default. It expects `Xvfb`,
`xdotool`, `xwd`, and ImageMagick to be installed.

To run the current terminal compatibility smoke suite:

```sh
scripts/run-terminal-smoke.sh
```

The suite runs shell input, new-terminal focus, split-pane status, ANSI color,
and nvim screen scenarios. The nvim scenario is skipped when `nvim` is not
installed. Artifacts are grouped under `/tmp/horizon-terminal-smoke-*` by
default, with each scenario containing `terminal.txt`, `status.txt`,
`screenshot.png`, and logs.

## Next Integration Points

`docs/roadmap.md` is the source of truth for the current phase plan; this
section does not duplicate it. Wave 1 in flight: a session-manager modal, a
redesigned agent transcript UI, and a minimal agent-roles mechanism.
