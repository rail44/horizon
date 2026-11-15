# Horizon

**A native, keyboard-first workspace where terminals and AI agents are
equal first-class sessions.**

Horizon is a desktop application of tabs and split panes. Each pane shows
a session — a terminal, an agent, or a view such as the task board — and
every operation on the workspace is a command, open to a person through
keys and the palette and to an agent through the `horizon` CLI.

It is built from source and runs on Linux; see [Build and run](#build-and-run).

## Features

### Workspace

Tabs, and panes split to any depth with resizable weights. One chord,
`ctrl+'`, enters workspace mode, where every key belongs to Horizon;
outside it, every other key goes to the pane.

| Key | In workspace mode |
| --- | --- |
| `h` `j` `k` `l`, arrows | move the cursor between panes |
| `tab`, `shift+tab` | next, previous tab |
| `t` | new tab (asks which view) |
| `s` | split right (asks which view) |
| `x` | close the pane |
| `:` | the command palette |
| `enter` | leave the mode; the focus goes to the cursor's pane |
| `esc` | leave the mode; the focus stays where it was |

The palette lists every command. Any of them can be bound to a chord in
the config file, and a pane's own commands (a terminal's copy and paste,
an agent's approve and deny) are bound the same way.

### Sessions

A session does not belong to the pane that shows it. Terminals run in
`horizon-terminald` and agents in `horizon-agentd`, both started on
demand, so:

- closing a pane or a tab detaches its session, which keeps running;
- `Manage Sessions` lists every session as a tree of which one started
  which, and attaches a detached one back as a split;
- quitting Horizon leaves the sessions running, and the next start
  restores the tabs, splits and focus around them;
- ending a session is its own command, `Terminate Active Session`.

### Terminal

- The Kitty keyboard protocol's common paths; the conformance matrix is
  kept in the code and names the gaps.
- Scrollback, selection with copy and the primary selection, IME input.
- Box-drawing and block characters drawn as shapes, so they join without
  seams at any display scale.
- Font size changed live with `ctrl+=`, `ctrl+-`, `ctrl+0`.

### Agent

An agent runtime of Horizon's own, talking to any OpenAI-compatible
endpoint you configure.

- **Tools**: `bash`, `fs_read` / `fs_write` / `fs_edit` / `fs_glob` /
  `fs_grep`, `web_search`, `web_fetch`, `worktree_enter`, the board's
  tools, and `recall_sessions` over earlier sessions.
- **Approval in three tiers**: what stays inside the sandbox runs without
  asking, what crosses its boundary is judged by a small model, and what
  cannot be undone asks a person.
- **Sandbox**: `bash` runs under Landlock on Linux (Seatbelt on macOS),
  with network access through an allowlisting proxy. The file trees and
  hosts a project's agents may reach are grants in the config file.
- **Roles and skills**: a session starts in a role; skills are
  `SKILL.md` files loaded from the repository's `.horizon/skills/`.
- **Models**: separate models for conversation, titles and approval
  judgments, and a picker to switch a running session's model.
- **History**: each session is an append-only JSONL log, projected into
  DuckDB for recall and inspection.

The runtime speaks the Agent Client Protocol to the shell.

### CLI

`horizon` with no arguments opens the window. With a subcommand it is a
client of the running instance, and inside a pane it acts on that pane's
own instance and session — which is how an agent starts another agent
beside itself.

```sh
horizon new-terminal --split          # a terminal beside this pane
horizon new-agent --prompt "Review the last commit"
horizon sessions --json               # every session
horizon send <session-id> "…"         # a message to an agent session
horizon approve <session-id> <call-id> <occurrence-id>
horizon set-model <session-id> <provider> <model>
horizon markdown notes.md             # a file in a Markdown pane
```

### Board

A task board the owner and the agents share: a ranked list of tasks in
one pane and a task's thread in another. A task can carry the agent
session that works on it, and agents read and write the board with their
own tools. The same board from a shell:

```sh
horizon board add "Fix the scrollback jump"
horizon board list
horizon board comment <id> "…" --author <name>
```

### Other views

- **Markdown** — a file rendered in a pane, following the file as it
  changes.
- **Wiki** — pages describing the project's current state, with comment
  threads anchored in the text.
- **File finder** — a file found by typing, fuzzy-matched off the UI
  thread, opened in the view that fits it.
- **Theme settings** — the whole color scheme derived from a few seed
  colors, edited live.

### Extensions and preview

A view can be a wasm component: a package with a `plugin.toml` naming the
component, its commands, and what it may read, listed under `extensions`
in the config file (`plugins/markdown` is one). A view still being
written is shown in a pane of the running Horizon with
`horizon preview <plugin.wasm>`, and reloaded from disk on a command.

## Stack

| For | Horizon uses |
| --- | --- |
| Drawing | [Blitz](https://github.com/DioxusLabs/blitz) (Stylo, Taffy, Parley, Vello) on winit |
| View state | `reactive_graph` signals |
| Views' CSS, checked at compile time | `lightningcss` |
| Terminal emulation and PTYs | `alacritty_terminal`, `termwiz`, `portable-pty` |
| Shell ↔ agent daemon | [Agent Client Protocol](https://agentclientprotocol.com) |
| Shell ↔ terminal daemon, board log | `remoc` |
| Model providers | `rig-core`, `reqwest` |
| Session history and recall | [DuckDB](https://duckdb.org) |
| Sandbox | `nono` (Landlock, Seatbelt), `hudsucker` for the network proxy |
| Extensions and preview | `wasmtime`, wasm components |
| Markdown | `markdown`, `pulldown-cmark` |
| File finder | `nucleo`, `ignore` |
| Async runtime | `tokio` |

Everything is Rust, in one Cargo workspace: the shell at the root and the
rest under `crates/`.

## Build and run

There are no releases; Horizon is built from source. It runs daily on
Linux. The current shell has not been run on macOS yet.

Requirements: a recent stable Rust toolchain, and DuckDB 2.0 as a shared
library — `scripts/install-duckdb-prefix.sh` installs it under
`~/opt/duckdb` without root, and "Build setup" in [`AGENTS.md`](AGENTS.md)
has the `~/.cargo/config.toml` entries that point the build at it.

```sh
cargo build --workspace     # the shell and both daemons
./target/debug/horizon
```

`cargo run` alone rebuilds only the shell, not the daemons it starts.
`just dev` runs the two lines above; `just dev-release` does the same
with optimizations.

Agent sessions need a provider: put `OPENAI_API_KEY` in the environment
and name the models in the config file. `web_search` also reads
`EXA_API_KEY`.

## Configuration

One optional file, `~/.config/horizon/config.toml`: models and providers,
fonts, key bindings, the theme's seed colors, grants, and extensions.
[`config.example.toml`](config.example.toml) documents every key, and
[`docs/provider-configuration.md`](docs/provider-configuration.md) the
provider entries. `Reload Config` applies the theme, key bindings and
providers without a restart.

## Documentation

- [`docs/roadmap.md`](docs/roadmap.md) — where Horizon stands and what is
  open, as an index into the design documents under `docs/`
- [`docs/ux-principles.md`](docs/ux-principles.md) — the product direction
- [`docs/workspace-mode-design.md`](docs/workspace-mode-design.md),
  [`docs/cli-control-plane-design.md`](docs/cli-control-plane-design.md) —
  the keys and the CLI in full
- [`AGENTS.md`](AGENTS.md) — the module map, conventions, and the quality
  gate for changes to this repository
