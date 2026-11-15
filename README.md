# Horizon

**A native, keyboard-first workspace where terminals and AI agents are
equal first-class sessions.**

Horizon is a desktop shell of tabs and split panes. What a pane shows is a
session — a terminal, an agent, or a view such as the task board — and
sessions live in daemons of their own, so closing a pane, or the window,
does not end the work in it. Everything a person can do to the workspace
is a command, and the same commands are open to agents through the
`horizon` CLI.

## Status

Horizon is a personal project, used daily by its author to develop Horizon
itself. There are no releases and no packages: it is built from source,
and anything — config keys, on-disk formats, the protocols between the
shell and its daemons — may change without notice.

- **Linux** is where it runs every day.
- **macOS** was supported by the previous shell; the current one (Blitz)
  has not been run there yet.

## What is in it

- **Sessions outlive what shows them.** `horizon-terminald` owns every PTY
  and `horizon-agentd` hosts every agent session. Closing a pane detaches
  its session; terminating one is a separate, explicit command. The
  workspace — tabs, splits, focus, attachments — comes back after a
  restart.
- **One stolen key.** `ctrl+'` enters workspace mode, where every key
  belongs to Horizon; outside it, every key but that one goes to the pane.
- **One command model.** The palette, the key bindings and the `horizon`
  CLI are bindings to the same commands, so an agent in a pane can split,
  spawn, attach and approve the way a person does.
- **An agent runtime of its own.** Agent sessions talk to any
  OpenAI-compatible endpoint you configure, with roles, skills, sandboxed
  shell execution, approvals, and recall over a DuckDB projection of the
  session log. The runtime speaks ACP to the shell and does not need
  Horizon as its frontend.
- **A terminal that speaks the Kitty keyboard protocol**: the common
  paths are implemented, and the conformance matrix kept in the code
  names the gaps.
- **Views beside the sessions**: a task board shared by the owner and the
  agents, a project wiki with comment threads, a Markdown view, a file
  finder, and live theme settings.
- **Views from wasm.** An extension is a view built as a wasm component;
  a view still being written can be shown in a pane of the running
  Horizon with `horizon preview`.

## Build and run

Requirements: a recent stable Rust toolchain, and DuckDB 2.0 as a shared
library — `scripts/install-duckdb-prefix.sh` installs it under
`~/opt/duckdb` without root, and `AGENTS.md` ("Build setup") has the
`~/.cargo/config.toml` entries that point the build at it.

```sh
cargo build --workspace     # the shell and both daemons
./target/debug/horizon
```

`cargo run` alone rebuilds only the shell, not the daemons it spawns;
`just dev` does both steps above, and `just dev-release` the same
optimized.

Agent sessions need a provider: set `OPENAI_API_KEY` in the environment
and choose models in the config file (below). The `web_search` tool also
reads `EXA_API_KEY`. Secrets are read from the environment only.

## First steps

| Key | In workspace mode |
| --- | --- |
| `ctrl+'` | enter or leave workspace mode |
| `h` `j` `k` `l`, arrows | move the cursor between panes |
| `tab`, `shift+tab` | next, previous tab |
| `t` | new tab (asks which view) |
| `s` | split right (asks which view) |
| `x` | close the pane; its session is detached |
| `:` | the command palette |
| `enter` | leave the mode; the focus goes to the cursor's pane |
| `esc` | leave the mode; the focus stays where it was |

With no tab open, `:` opens the palette directly. The palette lists every
command, among them `Manage Sessions` (attach or terminate what is
detached), `Open Board`, and the two runtime reloads.
See [the workspace mode design](docs/workspace-mode-design.md).

## The CLI

`horizon` with no arguments opens the window; with a subcommand it is a
client of the running instance. A pane's environment names that instance
and its own session, so a command run inside a pane acts where it is.

```sh
horizon new-terminal --split
horizon new-agent --prompt "Review the last commit"
horizon sessions
horizon send <session-id> "…"
horizon approve <session-id> <call-id> <occurrence-id>
horizon board add "…"
horizon markdown notes.md
```

See [the control plane design](docs/cli-control-plane-design.md) for the
whole contract.

## Configuration

One optional file, `~/.config/horizon/config.toml`: models and providers,
font, key bindings, the theme's seed colors, the grants agents work
within, and extensions. [`config.example.toml`](config.example.toml)
documents every key; [provider configuration](docs/provider-configuration.md)
covers the provider entries. `Reload Config` applies most of it live.

## How it is built

The window is drawn with [Blitz](https://github.com/DioxusLabs/blitz): a
view is a function that builds a CSS document once and follows signals
afterwards. The shell holds no session itself — it is a client of the two
daemons, and either can be reloaded without disturbing the other.

- [`docs/ux-principles.md`](docs/ux-principles.md) — what Horizon is and
  is not
- [`docs/roadmap.md`](docs/roadmap.md) — where it stands and what is
  open, as an index into the design docs under `docs/`
- [`docs/trust-boundaries.md`](docs/trust-boundaries.md) — what runs
  where, and why
- [`AGENTS.md`](AGENTS.md) — the module map, conventions, and the quality
  gate to run before finishing a change. If you are an AI agent working
  on this repository, read it first.
