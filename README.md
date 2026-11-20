# Horizon

Horizon is a desktop shell for tabbed and split-pane applications, drawn
with Blitz: a keyboard-first command workspace where terminals, AI agent
sessions, and (future) plugin views run as sessions attached to panes.

## MVP Shape

- Blitz (an HTML and CSS engine: Stylo, Taffy, Parley, Vello) draws the
  window; Horizon's own N-ary layout tree drives tabs and split panes. Every
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
./scripts/extensions.sh build
cargo test
cargo run
```

`cargo run` alone only rebuilds the root `horizon` binary; terminal and agent
sessions run in `horizon-agentd` (`crates/horizon-agentd`), a separate
workspace member Horizon spawns on demand. Run `cargo build --workspace` at least once (and
again after touching the runtime crates) or panes will fail to find a runtime
to spawn. The Markdown view, the wiki and the theme settings are extensions, wasm
components the second build command above makes (`rustup target add
wasm32-wasip2` once); without them Horizon starts and has none of the three. `just dev` builds the whole
workspace and launches the freshly built binary directly (bypassing `cargo
run`'s environment leakage into Horizon and agentd); pass CLI subcommand
arguments after it, e.g. `just dev sessions`.

Agent provider access reads `OPENAI_API_KEY` from the environment. The
Horizon-owned `web_search` tool additionally needs environment-only
`EXA_API_KEY`; neither secret belongs in `config.toml`. `web_fetch` has no
vendor key and asks before contacting each exact destination host.

After `cargo run`, press `ctrl+'` to enter workspace mode, then `:` to open
the control surface (see `docs/workspace-mode-design.md`) — a Commands-only
palette now that session management has its own modal (see below). With
zero tabs open (e.g. after `terminate active session` closes the last one),
`:` opens the palette directly with no `ctrl+'` first — an empty workspace
is an implicit command surface, since there is no pane input left to
protect; `ctrl+'` is a harmless no-op in that state. It supports these
manual smoke checks:

- `new tab`: opens the view chooser (`Enter` on `Terminal` opens another
  terminal tab; `Enter` on `Agent` or a role like `Configuration Agent`
  opens an agent tab) for a tab in the tab group the cursor is in.
- `split right`, `split left`, `split down`, `split up`: open the same view
  chooser, but the chosen view goes beside what the cursor is on instead of
  in a new tab. In workspace mode, `k` from a group's top pane puts the
  cursor on the group's tab strip: a split from there goes beside the whole
  group, and stays on screen whatever tab the group shows
  (`docs/workspace-layout-design.md`).
- `close active pane`: closes the active pane and leaves its session detached
  when another pane remains.
- In workspace mode, `H` `J` `K` `L` (Shift and a direction, or Shift and an
  arrow) swap the pane the cursor is on -- or, on a tab strip, the whole tab
  group -- with its neighbor in that direction. `move to new tab` moves the
  pane the cursor is on into a new tab of its tab group.
- `apply layout`: puts the workspace in one of the layouts `config.toml`
  names (`[layouts.<name>]`, see `config.example.toml`): the list of
  sessions kept on the left of everything else, for one. `startup_layout`
  puts the workspace in one when Horizon starts.
- `reset extensions`: lets go of the extensions `horizon extension use
  <extension.wasm>` put in from their files (one still being written, tried
  in place of the one of its name); the configuration's and Horizon's own
  are loaded again.
- `detached`: shows detached sessions such as `Terminal #2` and attaches the
  selected session back as a split in the tab group that last had focus.
- `tab 1`, `tab 2`, ...: switches to the matching tab.
- `terminate active session`: terminates the active session.
- `manage sessions`: opens the list of sessions over the workspace (see
  below). It is a command of the sessions extension, and is not there
  when the extension is not built.
- `reload session runtime`: restarts `horizon-agentd`, closes terminal
  sessions, and reconnects persisted agent sessions — use after rebuilding
  the session runtime crates, or to recover from a lost connection.

Running the `manage sessions` command opens the list of sessions over
the workspace; `horizon new sessions` (or the view chooser) puts the same
list in a pane, where it stays. It is an extension (`plugins/sessions`,
built by `./scripts/extensions.sh build`). The sessions are under the
project each works in, a session under the one it was derived from; within
a project, what waits on you is first, then what finished and was not
looked at, then what works, then what rests. The keys are in the band
under the rows: `j`/`k` or the arrows move the selection, `n` goes to the
next session that waits, `/` searches, `Enter` goes to the session's pane
(a detached session is put in a split), `t` opens a terminal in the
session's directory, `x` terminates the session and `X` terminates it with
everything derived from it (each asks first, and `y` answers). In a pane
`Esc` takes a search back; over the workspace it closes the list, as going
to a session or opening a terminal does.

The same command model is also reachable from outside the GUI: `horizon
<subcommand>` (no arguments launches the GUI itself) is a thin client over a
Unix-socket control plane, useful for scripting or driving Horizon from an
agent. Panes get `HORIZON_SOCKET`/`HORIZON_SESSION_ID` in their environment,
so a subcommand run from inside a pane targets the enclosing instance and
`--split` (bare) resolves to "here" by default. Subcommands:
`new <plugin> [<argument>...]` (a new pane of a plugin, made from what the
plugin takes: `new terminal`, `new agent [--prompt <text>] [--role config]`,
`new markdown <path>`, `new wiki [<page>]`,
`new preview <extension.wasm> [<argument>...]` for an extension still being
written (`docs/preview-pane-design.md`), `new <extension's name> [<argument>...]`),
`<command>` (a command of the plugin the cursor's pane shows, by the name
the palette lists it under),
`attach <session-id>`,
`terminate-session <session-id>`, `terminate-all-detached`,
`approve`/`deny <session-id> <call-id> <occurrence-id>`, `cancel-turn <session-id>`,
`continue-turn <session-id>`,
`reload-agent-runtime`, `reload-terminal-runtime`, `reload-config`,
`sessions`, `state` (each takes
`--split`/`--active` where placement/focus applies). See
`docs/cli-control-plane-design.md` for the full contract.

The views are checked without a window, in tests (`cargo nextest run
--workspace`); see "GUI Verification" in `AGENTS.md`.

## Next Integration Points

`docs/roadmap.md` is the source of truth for the current phase plan; this
section does not duplicate it.
