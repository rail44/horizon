<h1 align="center">Horizon</h1>

<p align="center">
  A native workspace for terminals and AI agents.
  <br />
  <a href="#about">About</a>
  ·
  <a href="#what-works-today">What works today</a>
  ·
  <a href="#stack">Stack</a>
  ·
  <a href="#building">Building</a>
  ·
  <a href="docs/roadmap.md">Roadmap</a>
</p>

<!-- screenshot: a tab split three ways — a terminal, an agent mid-turn, the board -->

## About

Horizon is a desktop application in which a terminal and an AI agent are
the same kind of thing: a session, shown in a pane, in a tab, in a native
window. You split, move, close and reattach them with the same keys, and
neither is a guest inside the other.

Agent tools today mostly live in one of two places. Some run inside a
terminal, where they compete with the shell for keys and cannot easily
see or drive what is in the next split. Others are full editors or
hosted products, with the model and its price chosen for you. Horizon is
a third arrangement: a GUI that owns its panes, its terminal emulation
and its agent runtime, and exposes all of it through one set of commands
— so the palette, the key bindings, the `horizon` CLI, and the agents
themselves operate the workspace in the same way.

Sessions run in daemons separate from the window. Closing a pane detaches
a session instead of ending it, and the workspace is restored around the
sessions that are still alive when the window comes back.

## What works today

| | | |
| --- | --- | --- |
| Tabs, nested splits, workspace mode, command palette | ✅ | `ctrl+'`, then `hjkl`, `s`, `t`, `x`, `:` |
| Sessions that survive the pane and the window | ✅ | detach, attach, session manager, layout restore |
| Terminal | ✅ | Kitty keyboard protocol (common paths), IME, scrollback, selection |
| Agent sessions | ✅ | OpenAI-compatible and Anthropic endpoints; roles, skills, model switching |
| Agent tools | ✅ | shell, files, web search and fetch, worktrees, recall of past sessions |
| Sandbox and tiered approval | ✅ | Linux (Landlock); contained commands run unprompted |
| `horizon` CLI | ✅ | sessions, panes, approvals, the board — also usable by agents |
| Task board | ✅ | list and thread panes; tasks own agent sessions |
| Markdown view, project wiki, theme settings | ✅ | |
| File finder | 🚧 | new |
| Extensions as wasm components | 🚧 | one extension so far (`plugins/markdown`) |
| macOS | ❌ | not yet run on the current renderer |
| Releases and packages | ❌ | build from source |

Details for each row are in [`docs/roadmap.md`](docs/roadmap.md), which
indexes a design document per decision.

## Stack

Horizon is Rust throughout.

- **Rendering** — [Blitz](https://github.com/DioxusLabs/blitz): Stylo for
  CSS, Taffy for layout, Parley for text, Vello for painting, on winit.
  Views are functions over `reactive_graph` signals.
- **Terminal** — `alacritty_terminal` and `termwiz` for emulation,
  `portable-pty` for PTYs, and Horizon's own painting of the grid.
- **Agents** — the [Agent Client Protocol](https://agentclientprotocol.com)
  between the window and the agent daemon; `rig-core` for providers; an
  append-only JSONL log per session, projected into
  [DuckDB](https://duckdb.org).
- **Isolation** — `nono` (Landlock on Linux, Seatbelt on macOS) and a
  `hudsucker` proxy for agent commands; `wasmtime` for extension views.
- **Between processes** — `remoc` to the terminal daemon and the board
  log, a Unix socket for the CLI.

## Building

```sh
scripts/install-duckdb-prefix.sh   # DuckDB 2.0 into ~/opt/duckdb, no root
cargo build --workspace            # the window and its daemons
./target/debug/horizon
```

"Build setup" in [`AGENTS.md`](AGENTS.md) covers pointing Cargo at
DuckDB. Agents need `OPENAI_API_KEY` in the environment and models named
in the config file; [`config.example.toml`](config.example.toml) lists
every key.

## Contributing and developing

[`AGENTS.md`](AGENTS.md) is the guide for people and agents alike: the
module map, the conventions, and the quality gate.
