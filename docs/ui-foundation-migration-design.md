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
- `src/runtime/` (the agentd and terminald clients): one file of it names
  gpui.
- The theme's derivation (`src/theme/scheme.rs`, `palette.rs`,
  `oklab.rs`): pure over packed colors. It names three gpui color types,
  which need a home that is not gpui.
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

`blitz-shell` is vendored in the repository with Horizon's changes (three
in the spike: the input method's cursor area, thin borders at fractional
scales, a hook after each resolve). `blitz-dom`, `blitz-paint` and
`blitz-traits` are pinned by revision. Nothing is posted upstream.

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

## The spike's shortcuts, to be removed in the foundation

- The document is lent to effects through a raw pointer while the shell
  holds it (`unsafe`), so that a layout can be redone before a paint.
- Handlers registered for a node are not dropped with the node.
- `each_layout` work is never unregistered.
- The type scale is `src/ui/spec.rs`'s table written out again; the theme
  files are read in through a stand-in for gpui's color types.

## Verification

Tests drive a view with the document's own input events and assert on the
document, with no window. Appearance is checked against the GPUI shell
side by side: an isolated instance on a virtual display (`Xvfb`), its
window captured, beside the new shell's own picture of the same content.

## Known gaps

- Text is drawn slightly lighter than GPUI draws it at the same font and
  size.
- Not yet exercised on this stack: selecting and copying text in a
  transcript, accessibility, desktop notifications, window decorations.
- macOS: the GPUI shell has macOS branches; the spike ran on Linux only.
- Blitz is a beta. Each problem met so far (thin borders, hit testing
  through a clip, the input method's position, focus) was worked around
  from Horizon's side.
