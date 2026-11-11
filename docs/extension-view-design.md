# Extension views

An extension view is a pane's view drawn by a WASI preview 2 component
instead of by code in `src/`. The Markdown view is the first one written
this way; it runs beside the built-in Markdown view, not in place of it.

## The parts

- **`gpui-wit-plugin`** (`rail44/gpui-wit-plugin`, pinned by revision in the
  root `Cargo.toml`) hosts the component. The component numbers the nodes of
  a tree and returns, from each call, the operations that change it; the
  host keeps the tree and turns it into GPUI elements. A node is a `div`,
  text, or a component registered by name, and its style is GPUI's
  `StyleRefinement` as JSON. The crate depends on `gpui`, `wasmtime` and
  `wasmtime-wasi` only.
- **`src/extension/`** is Horizon's side.
  - `mod.rs` — `ExtensionView`, what a pane holds: it loads the component,
    gives it its WASI context and host data, and carries out what the
    component asked of the application.
  - `host.rs` — the implementation of `extensions/wit/host.wit`.
  - `components.rs` — the parts an extension places by name (`markdown`,
    `button`, `chip`), drawn by Horizon's own `ui` and gpui-component code.
- **`extensions/wit/host.wit`** is the interface Horizon offers an
  extension's view: the colors and measurements to draw with, the pane's
  document, a file watch, opening a URL, and the Markdown view's front
  matter reader and link resolver.
- **`extensions/markdown/`** is the Markdown view as an extension, its own
  sub-workspace built for `wasm32-wasip2`. It is written with
  `gpui-wit-plugin-sdk` (same revision as the host), the way a GPUI view is
  written: a struct with a `render` that runs again after `cx.notify()`.

## Who does what, for the Markdown view

| | Extension | Horizon |
|---|---|---|
| The file shown, the way back, the notice | holds them | records the file on the pane (`DocumentChanged`) |
| Reading the file | `std::fs`, through WASI | preopens `/` read-only |
| Front matter, link resolution | calls `parse` / `resolve-link` | `markdown::document` |
| What a link or a key does | decides | delivers `link` and `key_down` |
| Drawing Markdown, the back button, chips | places the components | draws them |
| Scrolling | asks through a scroll handle | moves the element's `ScrollHandle` |
| File changes, theme and font size changes | re-reads on `file-changed`, re-asks on `look-changed` | watches the file, observes the theme |

## How a call reaches the application

A function of `host.wit` runs inside the component's own call, where only
the store's data (`HostData`) is in reach. Calls that answer from global
state (theme, type scale, the document reader) answer directly. Calls that
need the application (`set-document`, `watch`, `open-url`) leave a
`Request` in `HostData`; `gpui-wit-plugin` emits `Returned` after each call
into the component, and `ExtensionView::carry_out` runs the requests then.

Horizon tells a view something by dispatching an event to the view as a
whole (`open`, `file-changed`, `look-changed`).

## Running it

```sh
rustup target add wasm32-wasip2     # once
cd extensions/markdown && cargo build --release --target wasm32-wasip2
HORIZON_MARKDOWN_EXTENSION=$PWD/target/wasm32-wasip2/release/horizon_markdown_extension.wasm cargo run
```

With `HORIZON_MARKDOWN_EXTENSION` set, a Markdown pane is drawn by the
component it names; unset, or when the component does not load, by the
built-in view. The pane kind, its persistence and `horizon markdown <path>`
are the same either way.

`src/extension/tests.rs` builds the extension itself and holds it to what
the built-in view's tests hold that view to.

## What the other views use

Read from `src/board`, `src/wiki` and `src/theme_settings` as they are.

| | Used by | In `gpui-wit-plugin` |
|---|---|---|
| Hover and group-hover styles | board list, board thread | yes |
| Mouse down, positioning, opacity, truncation | board, wiki | yes |
| A component with state of its own | every one (text input) | the mechanism is there (`Window::use_keyed_state` by the node's id); no `input` component is registered |
| Work off the main thread, answered later | board (the store), wiki (thread log) | a request answered by an event, as `watch` and `file-changed`; no such call exists for a store |
| Drag and drop | board list (reordering) | no |
| Custom painting (`canvas`) | board thread, wiki | no |
| An animation | wiki | no |

## Not there

- The extension runs on the main thread, and reads its file there; a call
  into it has no time limit.
- There is no pane kind for extension views in general, and no loading of
  extensions at startup: the environment variable above is the only way in.
- `host.wit` is shaped by the Markdown view alone.
- The host draws a component's whole tree again on any change to it.
- Page scrolling moves by the scrolled element's height, where the built-in
  view uses the window's.
