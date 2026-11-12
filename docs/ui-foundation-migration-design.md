# UI Foundation Migration Design

Horizon's shell moves from GPUI to Blitz. This document describes the
target and the route. The decision is board #75; the survey of candidates
and the spike that backs each statement below are in
[the research record](research/ui-foundation-candidates-2026-11-11.md)
(addenda 1–16) and `spikes/ui-foundation/`.

## The stack

| Concern | Crate |
|---|---|
| Reactivity (signals, effects, ownership) | `reactive_graph` |
| Document, CSS, layout, text, hit testing, focus, events | `blitz-dom` (Stylo, Taffy, Parley) |
| Painting | `blitz-paint` over `anyrender`, Vello on the GPU |
| Window, input, input method | `blitz-shell` over winit |

A view function runs once and builds elements of the document. What
changes afterwards is an effect over signals that writes one thing: a text
node, an attribute, a list's children. There is no render pass over a view
and no notify.

Styles are CSS. Each view's rules sit under its root element's class
(CSS nesting), since a document has one style sheet for every view on it.

## The layer between signals and the document

Horizon owns a small layer (about 1,400 lines in the spike) that is the
only code naming `blitz-dom`'s document. A view sees:

- `El`: build and change elements (attributes, classes, one style
  property, text, children, a keyed list, a conditional child, handlers,
  scrolling, a text field's value, detaching and removing).
- `Page`: read the layout (a box's place and size, what is under a point,
  scroll position, a field's line count, focus). A view is handed a
  `Page` at the two times its answers are sound: right after an event
  (`dispatched`), and after a layout (`each_layout`).
- Capture-phase handlers (`on_capture`), which `blitz-dom` does not have:
  the layer walks an event's path inwards before the outward pass.
- `windowed`: a long list with only the items near the view built.
- For an element that takes text without being a text field (the
  terminal): the input method (`takes_text`) and pastes (`on_paste`).

A view does not reach the document itself. This is what a future
extension boundary needs of views, and it holds for the five views of the
spike. Two things a view still takes from Blitz directly: event payload
types, and for the terminal the paint widget.

## What carries over unchanged

- Every daemon and the crates behind them: `horizon-agentd`,
  `horizon-terminald`, `horizon-logd`, `horizon-agent`,
  `horizon-terminal-core`, `horizon-board`, `horizon-wiki`,
  `horizon-workspace`, `horizon-control`, `horizon-cli`, `horizon-config`.
- The agentd and terminald clients, their route tables, and the agent
  session model the agent routes carry: `crates/horizon-shell-runtime`.
  The GPUI shell's `src/runtime/` keeps the one part that is GPUI's own,
  the bridge from an attachment's channels into its tasks and entities.
- The theme's derivation: `crates/horizon-theme`, a scheme of packed
  colors with no UI framework's types. The GPUI shell's `src/theme/` is its
  projection onto gpui; the Blitz shell's `shell-blitz/theme` is its
  projection onto CSS variables.
- Display models written apart from their views (`src/agent/view/display.rs`,
  `chrome.rs`).

## Layout and coexistence

The new shell is a second binary in a workspace of its own
(`shell-blitz/`, an empty `[workspace]` table, path-depending on
`crates/*`), as `shell-gpui/` was beside the Floem shell
([the GPUI migration](gpui-migration-design.md)). Blitz is a beta and the
shell depends on a revision past its last release, so its dependency
graph stays out of the root lockfile. The GPUI shell keeps working until
the new one is at parity, and is then deleted.

`blitz-shell` is vendored in the repository (`shell-blitz/vendor/`) with
two changes: the input method is enabled with its cursor area, and the
shell hands each frame's layout to the layer (`set_frame`) instead of
resolving the document itself. `blitz-dom`, `blitz-paint` and
`blitz-traits` are pinned by revision. Nothing is posted upstream.

The workspace's crates:

- `shell-blitz/ui` (`horizon-ui`): the layer.
- `shell-blitz/theme` (`horizon-theme-css`): the theme and the type scale
  as CSS variables.
- `shell-blitz/app` (`horizon-blitz`): the shell's binary.

## Order

1. **Foundation.** The workspace, the layer as a crate (rebuilt from the
   spike without its shortcuts, listed below), the theme as CSS variables,
   a window.
2. **Terminal.** Over `horizon-terminald` through `src/runtime/`'s client,
   not the spike's in-process PTY. Scrollback windows, mouse reporting,
   the rest of the frame's styling.
3. **Shell and control surface.** `horizon-workspace`'s model, tabs,
   splits, workspace mode, the palette, the session manager, the view
   chooser, the control plane.
4. **Agent pane.** The adapter from a live session, approvals, the
   composer.
5. **Board, Markdown, wiki, theme settings, model picker.**
6. **Parity and retirement.** The GPUI shell, `gpui`/`gpui-component`,
   and the preview pane (`src/preview/`, `preview-plugin/`) are deleted.

The extension boundary comes after retirement.

## What the foundation changed from the spike

- A frame is the layer's: it lays out, runs the work that follows a
  layout, and lays out again while that work asks for it, with ordinary
  borrows of the document. The spike lent the document to effects through
  a raw pointer while the shell held it.
- What the layer keeps for an element (handlers, what takes text and
  pastes) is dropped with the element, and work registered to follow each
  layout ends with the owner it was registered under.
- The theme is read from `horizon-theme`. The spike read the GPUI shell's
  theme files in through a stand-in for gpui's color types.

Still written out twice: the type scale. `shell-blitz/theme` repeats the
table of `src/ui/spec.rs`, which is in gpui's units.

## Verification

Tests drive a view with the document's own input events and assert on the
document, with no window. Appearance is checked against the GPUI shell
side by side: an isolated instance on a virtual display (`Xvfb`), its
window captured, beside the new shell's own picture of the same content.
The new shell's picture is drawn offscreen; its window does not open on
`Xvfb` (Vello's window renderer finds no adapter that can present there),
so what only a window shows is looked at on a real desktop.

## Known gaps

- Text is drawn slightly lighter than GPUI draws it at the same font and
  size.
- Not yet exercised on this stack: selecting and copying text in a
  transcript, accessibility, desktop notifications, window decorations.
- macOS: the GPUI shell has macOS branches; the spike ran on Linux only.
- Blitz is a beta. Each problem met so far (thin borders, hit testing
  through a clip, the input method's position, focus) was worked around
  from Horizon's side.
