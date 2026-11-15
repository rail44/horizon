# Horizon

**Terminals and AI agents, side by side, in one native window.**

<!-- screenshot: a tab split three ways — a terminal, an agent mid-turn, the board -->

Horizon is a tabbed, split-pane desktop app for Linux, written in Rust.
A pane holds a terminal, an agent, or a view; you drive it all from the
keyboard, and so can the agents.

- **Close a pane, keep the work.** Terminals and agents run in their own
  daemons. Close the pane, the tab, or the whole window — they keep going,
  and the layout is back when you reopen.
- **One key out, then every key is yours.** `ctrl+'` enters workspace
  mode: `hjkl` to move, `s` to split, `t` for a tab, `:` for the palette.
  Nothing else is taken from your shell or editor.
- **Agents that can use the workspace.** Everything the palette does, the
  `horizon` CLI does. An agent opens a terminal beside itself, starts
  another agent, or answers an approval the same way you would.
- **Your own model bill.** The agent runtime is Horizon's own and talks to
  any OpenAI-compatible or Anthropic endpoint. No account, no bundled plan.
- **Ask less, contain more.** Agent shell commands run in a sandbox.
  What stays inside runs without a prompt; what crosses the boundary is
  judged by a small model; what cannot be undone asks you.
- **A real terminal.** Kitty keyboard protocol, IME, scrollback, and box
  drawing that stays seamless at fractional scales.
- **A board both of you read.** Tasks, their threads, and the agent
  session working on each — in a pane, or `horizon board` from a shell.
- **Views are cheap.** Markdown, a project wiki, a file finder, live theme
  settings. New ones can be wasm components.

## Try it

```sh
scripts/install-duckdb-prefix.sh   # once; see AGENTS.md, "Build setup"
cargo build --workspace
./target/debug/horizon
```

Then `ctrl+'`, `:`. Set `OPENAI_API_KEY` and a model in
[`config.toml`](config.example.toml) for agents. There are no releases
yet, and macOS is untested on the current renderer.

## Built with

[Blitz](https://github.com/DioxusLabs/blitz) (Stylo · Taffy · Parley ·
Vello) · `alacritty_terminal` · `termwiz` ·
[Agent Client Protocol](https://agentclientprotocol.com) · `rig` ·
[DuckDB](https://duckdb.org) · `remoc` · `nono` · `wasmtime` · `tokio`

## More

[roadmap](docs/roadmap.md) · [UX principles](docs/ux-principles.md) ·
[workspace mode](docs/workspace-mode-design.md) ·
[CLI](docs/cli-control-plane-design.md) ·
[configuration](config.example.toml) · [for contributors and agents](AGENTS.md)
