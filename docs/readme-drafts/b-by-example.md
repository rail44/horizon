# Horizon

A native desktop workspace for terminals and AI agents, written in Rust.

<!-- screenshot: a tab split three ways — a terminal, an agent mid-turn, the board -->

## Highlights

- Terminals and agents are both **sessions**: put either in any tab or
  split, close the pane without ending it, attach it again later.
- **Keyboard-first**, with a single reserved chord.
- **Scriptable** from the `horizon` CLI — by you, or by an agent running
  inside a pane.
- An **agent runtime of its own**: OpenAI-compatible and Anthropic
  endpoints, sandboxed tools, approvals that mostly do not need you.
- A **task board**, a Markdown view, a project wiki and a file finder as
  panes next to the sessions.
- Drawn with [Blitz](https://github.com/DioxusLabs/blitz); see
  [Stack](#stack).

## Features

### Splits and tabs

Press `ctrl+'` to enter workspace mode, then:

```
s        split the pane to the right — pick Terminal or Agent
t        open a new tab
h j k l  move between panes
tab      next tab
x        close the pane
:        open the command palette
enter    back to the pane
```

Outside workspace mode every key except `ctrl+'` reaches the terminal or
agent in the pane. Any command can also be bound to a chord:

```toml
[keybindings]
"ctrl+shift+t" = "new-tab"
"ctrl+shift+p" = "open-palette"
```

### Sessions that outlive panes

Close a pane with `x` and its terminal or agent keeps running, detached.
Bring it back from the palette:

```
:manage sessions     every session, as a tree of which started which
enter                attach the selected one as a split
```

Quit Horizon and start it again: the tabs, splits and focus return, with
the same sessions in them. Sessions end only when you say so, with
`Terminate Active Session`.

### Driving it from a shell

Inside a pane, `horizon` talks to the window that pane is in:

```sh
horizon new-terminal --split                 # a terminal beside this one
horizon new-agent --prompt "Run the tests and fix what fails"
horizon sessions --json
horizon send <session-id> "Also update the docs"
horizon markdown docs/roadmap.md             # open a file in a Markdown pane
```

Agents have the same CLI, so an agent can hand part of its work to a new
agent in the next split and read back what it did.

### Agents

Point Horizon at a provider and choose models:

```toml
default_model  = { provider = "default", model = "…" }   # conversation
small_model    = { provider = "default", model = "…" }   # titles
decision_model = { provider = "default", model = "…" }   # approval judgments

[[providers]]
name = "default"
base_url = "https://…/v1"
api_key_env = "OPENAI_API_KEY"

[[providers]]
name = "claude"
kind = "anthropic"
```

A running session switches models from a picker that lists what each
provider offers.

An agent session has these tools:

| | |
| --- | --- |
| Shell | `bash` (sandboxed), `bash_output` |
| Files | `fs_read`, `fs_write`, `fs_edit`, `fs_glob`, `fs_grep` |
| Web | `web_search`, `web_fetch` |
| Work | `worktree_enter`, `board_read`, `board_update`, `board_comment` |
| Memory | `recall_sessions`, `knowledge_read`, `knowledge_write` |

Approvals come in three tiers. Commands that stay inside the sandbox just
run. Commands that would cross it are judged by the decision model.
Irreversible ones wait for you:

```sh
horizon approve <session-id> <call-id> <occurrence-id>
horizon deny    <session-id> <call-id> <occurrence-id>
```

Which directories and hosts a project's agents may reach is configured
under `[grants]`.

### The board

```sh
horizon board add "Fix the scrollback jump"
horizon board list
horizon board show <id>
horizon board comment <id> "Reproduced on a fractional scale" --author me
```

`Open Board` shows the same tasks as a ranked list in one pane and a
task's thread in another. A task can own an agent session; the agent
posts to the thread with its board tools.

### Terminal

- Kitty keyboard protocol (the conformance matrix is in the code: `cargo
  test -p horizon-terminal-core print_compliance_matrix -- --nocapture`)
- IME input, selection, clipboard and primary selection, scrollback
- `ctrl+=` / `ctrl+-` / `ctrl+0` change the font size live

### Theme

The whole scheme is derived from a few seed colors:

```toml
[theme]
surface_base = "#…"
accent = "#…"
```

`Reload Config` applies it without a restart, and the theme settings pane
edits it with sliders and a color picker.

### Extensions

A view can be a wasm component, described by a `plugin.toml`:

```toml
[plugin]
name = "markdown-extension"
title = "Markdown"
component = "markdown.wasm"

[access]
read = ["/"]
```

```sh
horizon preview target/wasm32-wasip2/debug/my_view.wasm   # show a view in progress
```

## Stack

| For | Horizon uses |
| --- | --- |
| Drawing | [Blitz](https://github.com/DioxusLabs/blitz) (Stylo, Taffy, Parley, Vello) on winit |
| View state | `reactive_graph` signals |
| Terminal | `alacritty_terminal`, `termwiz`, `portable-pty` |
| Shell ↔ agent daemon | [Agent Client Protocol](https://agentclientprotocol.com) |
| Shell ↔ terminal daemon | `remoc` |
| Providers | `rig-core`, `reqwest` |
| Agent history and recall | JSONL, [DuckDB](https://duckdb.org) |
| Sandbox | `nono` (Landlock, Seatbelt), `hudsucker` |
| Extensions | `wasmtime` |
| Markdown | `markdown`, `pulldown-cmark` |
| File finder | `nucleo`, `ignore` |

## Installation

Horizon is built from source; there are no releases yet. It is developed
and used daily on Linux. macOS has not been run on the current renderer.

```sh
scripts/install-duckdb-prefix.sh   # DuckDB 2.0 into ~/opt/duckdb, no root
cargo build --workspace            # the app and its daemons
./target/debug/horizon
```

The build needs to find DuckDB; "Build setup" in [`AGENTS.md`](AGENTS.md)
has the two `~/.cargo/config.toml` entries.

## Documentation

- [`config.example.toml`](config.example.toml) — every configuration key
- [`docs/roadmap.md`](docs/roadmap.md) — current state and open work
- [`docs/`](docs) — one design document per decision
- [`AGENTS.md`](AGENTS.md) — conventions and the quality gate
