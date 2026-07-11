# Horizon

Horizon is a GPUI-based desktop shell for tabbed and split-pane
applications: a keyboard-first command workspace where terminals, AI agent
sessions, and (future) plugin views run as sessions attached to panes.

## MVP Shape

- GPUI (Zed's UI framework, with gpui-component) owns the native window;
  Horizon's own N-ary layout tree drives tabs and split panes. Every
  operation runs through one command model, surfaced today by workspace mode's
  `:` palette (`docs/workspace-mode-design.md`) and by the `horizon` CLI
  control plane (`docs/cli-control-plane-design.md`).
- Built-in sessions provide the first two MVP surfaces:
  - Terminal: PTY-backed terminal core using `portable-pty`,
    `alacritty_terminal`, and `termwiz`.
  - AI Agent: provider-backed agent sessions using Horizon-owned
    command/event/frame contracts.
- Plugin views remain the future path for hot-reloadable pane development,
  including eventually developing Horizon from inside Horizon (the Floem-era
  WASM groundwork retired with that shell; see tag `floem-shell-final`).

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
the control surface (see `docs/workspace-mode-design.md`) — a Commands-only
palette now that session management has its own modal (see below). It
supports these manual smoke checks:

- `new tab`: opens the view chooser (`Enter` on `Terminal` opens another
  terminal tab; `Enter` on `Agent` or a role like `Configuration Agent`
  opens an agent tab).
- `split right`: opens the same view chooser, but the chosen view splits the
  active pane horizontally instead of opening a new tab.
- `split down`: same as `split right`, but splits vertically.
- `close active pane`: closes the active pane and leaves its session detached
  when another pane remains.
- `detached`: shows detached sessions such as `Terminal #2` and attaches the
  selected session back into the active tab as a split.
- `tab 1`, `tab 2`, ...: switches to the matching tab.
- `terminate active session`: terminates the active session.
- `manage sessions`: opens the session manager modal (see below).
- `reload agent runtime`: restarts `horizon-agentd` and reconnects every
  agent session — use after rebuilding the agent crates, or to recover from
  a lost connection.

Running the `manage sessions` command opens a separate modal listing every
session the workspace knows about: arrows move the selection, `Enter`
attaches a detached session as a split (or jumps to an attached session's
pane), `cmd+Enter` terminates the selected session (the modal stays open),
and `Esc` closes the modal.

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

For an automated headless check of the terminal pane:

```sh
scripts/check-gpui-terminal.sh
```

The script launches the freshly built binary with Horizon's built-in
headless taps (`HORIZON_GPUI_DUMP`/`HORIZON_GPUI_DRIVE`), types a marker
command, and asserts the frame dump recorded the marker plus 256-color and
truecolor spans. It refuses to run while another Horizon instance is
running unless `--force-kill` is passed.

## Next Integration Points

`docs/roadmap.md` is the source of truth for the current phase plan; this
section does not duplicate it.
