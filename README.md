# Horizon

Horizon is a Floem desktop shell for tabbed and split-pane applications.

## MVP Shape

- Floem owns the native window, toolbar, tab actions, and split-pane layout.
- Each pane is backed by a plugin-shaped `PluginFrame`.
- Built-in plugins provide the first two MVP surfaces:
  - Terminal: PTY-backed terminal core using `portable-pty`, `alacritty_terminal`, and `termwiz`.
  - AI Agent: planned local-agent pane using the same command/frame contract.
- WASM plugins are represented by manifests and validated through `wasmtime`.

## Commands

```sh
cargo check
cargo test
cargo run
```

After `cargo run`, use `Ctrl+Shift+P` to open the command palette. Useful
manual smoke checks:

- `new terminal`: opens another terminal tab.
- `new agent`: opens an agent tab.
- `split`: splits the active pane.
- `close active pane`: closes the active pane and leaves its session detached
  when another pane remains.
- `detached`: shows detached sessions such as `Terminal #2` and attaches the
  selected session back into the active tab as a split.
- `tab 1`, `tab 2`, ...: switches to the matching tab.
- `terminate active session`: terminates the active session.

Use `Ctrl+Shift+O` to open the workspace overview. It lists open tabs and
detached sessions; Enter switches to the selected tab or attaches the selected
detached session as a split.

For automated visual inspection of the terminal pane:

```sh
scripts/check-terminal-visual.sh
```

The script starts Horizon on an isolated Xvfb display, writes the terminal model
to `terminal.txt`, captures the Horizon window to `screenshot.png`, and leaves
all artifacts under `/tmp/horizon-visual-*` by default. It expects `Xvfb`,
`xdotool`, `xwd`, and ImageMagick to be installed.

## Next Integration Points

1. Bind `TerminalSession` updates into the Floem pane event loop.
2. Convert Floem/winit keyboard events into `termwiz::input::KeyCode` values.
3. Add a JSON-RPC or stdio bridge for the AI agent pane.
4. Define the guest WASM ABI as WIT or a small exported JSON command function.
5. Persist workspace state so tabs and splits survive restart.
