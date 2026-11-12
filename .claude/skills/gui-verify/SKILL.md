---
name: gui-verify
description: Use when verifying Horizon GUI changes end-to-end - checking a view without a window, painting it to pixels, or running the shell over real daemons in a test. Trigger words: verify GUI, screenshot, smoke test, visual check, run the app.
---

# GUI Verification

Agents cannot see the window, and must not open one on a desktop a person
is using. Horizon's window also does not open on a virtual display (`Xvfb`):
Vello's window renderer finds no adapter that can present there. So a view
is verified without a window, in a test.

## A view through its document

The tests under `tests/` build a view into a document with
`horizon_ui::App::new(width, height, css, || view)`, feed it the document's
own input events, and assert on the document.

- Keys, the input method, a paste: `app.dispatch(UiEvent::KeyDown(..))`,
  `UiEvent::Ime(BlitzImeEvent::Commit(..))`, `UiEvent::Paste(..)`.
- The mouse: `horizon_ui::testing::{pointer, click_at, move_to, select}`.
- After a change made outside a handler (a signal set from the test, a
  command run through `Shell::execute`): `app.run_effects()` then
  `app.resolve()`. `app.resolve()` alone lays out and takes in what
  arrived from other threads.
- What is on the page: walk `app.document()` by class, or ask a node's
  `text_content()` and `get_client_bounding_rect`.

Each view has a file to start from: `tests/terminal.rs`, `tests/agent/`,
`tests/board.rs`, `tests/board_thread.rs`, `tests/markdown.rs`,
`tests/wiki.rs`, `tests/parts.rs`, and `tests/workspace.rs` for the whole
shell.

## What is drawn

A test about pixels paints the page on the CPU and reads them:

```rust
let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
let mut pixels = Vec::new();
renderer.render_to_vec(
    |scene| blitz_paint::paint_scene(scene, &mut document.borrow_mut(), scale, width, height, 0, 0),
    &mut pixels,
);
```

At a display scale other than one, set the document's viewport to that
scale first (`document.set_viewport(Viewport::new(width, height, scale, ..))`)
and paint at it: edges that are on whole pixels at one are not at 1.25.

To look at a picture yourself, write the pixels out from the test (the
terminal and workspace tests do when `HORIZON_TEST_DUMP=<path>` is set, as
raw RGBA), convert it to PNG, and read the PNG.

## Over real daemons

`tests/terminald.rs`, `tests/agentd.rs` and `tests/board_logd.rs` start
`horizon-terminald`, `horizon-agentd` and `horizon-logd` on sockets and a
data home of the test's own, never the ones in daily use. The agent's
provider is `scripts/mock-openai-provider.py`. Build the daemons first
(`cargo build --workspace`). A test that fails may leave its daemon
running: find it by its socket under `/tmp/hzb*` and stop that process
only.

## What a person has to look at

The window itself: text weight, a real display's scale, the input
method's candidates, the clipboard and the primary selection, desktop
notifications. Say plainly in the report what was verified in a test and
what was not looked at.
