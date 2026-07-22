# Native GPUI platform

Status: adopted on every OS (2026-07-22).

## Decision

Horizon constructs its application with `gpui_platform::application()` and
does not provide its own `gpui::Platform` implementation. The selected Zed
backend owns the complete platform boundary for the current OS: window and
event-loop integration, input and IME delivery, clipboard, renderer
presentation, and frame scheduling.

The retired `horizon-winit-platform` crate is not kept as a fallback. Keeping
two implementations would retain the behavioral-skew and maintenance problem
this decision removes. Its architecture and incident history remain in
`docs/winit-backend-design.md` and git history.

## Why the boundary is this large

winit successfully normalizes window creation and OS events, but GPUI's
native backend optimization is not a renderer adapter that can be placed
under an otherwise independent winit loop. A `gpui::PlatformWindow` also owns
the request-frame callback, presentation timing, input-method state, focus,
and window state. Horizon's custom implementation therefore had to infer
when GPUI wanted another frame and translate that into winit redraw requests.

That inference repeatedly became product behavior: a free-running idle loop,
redraw bursts around IME and scroll, and later an animation-continuity repair
that needed yet another private scheduling state. Each local repair could be
tested, but Horizon still owned a parallel implementation of semantics that
the native GPUI backends already maintain. The project does not benefit enough
from window-toolkit interchangeability to justify that ownership.

## Window chrome

GPUI's native Linux backend does not supply GNOME/Mutter client-side chrome.
Horizon therefore requests `WindowDecorations::Client`, configures the window
with `TitleBar::title_bar_options()`, and renders
`gpui_component::TitleBar` as the shell's first child. This is application UI,
not a replacement platform backend. The same setup preserves the native
traffic-light inset on macOS and avoids a separate per-OS construction path.

## Performance and regression boundary

Horizon may optimize its own view work: transcript virtualization, terminal
shape caching, session update coalescing, and avoiding notifications when
state did not change. It must not add an application-owned redraw scheduler or
infer presentation demand from view renders. Frame callback and presentation
policy stay in `gpui_platform`.

When investigating CPU regressions, separate these layers:

1. Count Horizon entity notifications and expensive view renders to find
   application work.
2. Profile the process to distinguish layout/scene construction from backend
   presentation.
3. Reproduce native platform behavior upstream before adding a Horizon-side
   scheduling workaround.

This boundary prevents a local performance fix from silently becoming a new
window backend.

## Verification

The adoption branch verifies:

- the existing pinned GPUI revision remains unchanged;
- Linux compilation through both Wayland and X11-enabled `gpui_platform`;
- the isolated terminal smoke (PTY spawn, scripted input, indexed color, and
  truecolor frame dump);
- a same-host, same-five-second-window idle comparison: the running custom
  winit build stayed at roughly 111–115% of one CPU core while the isolated
  native GPUI build stayed at 0–2%; this is diagnostic evidence, not a stable
  benchmark, but it directly reproduces the frame-loop failure that motivated
  the migration;
- the complete workspace quality gate.

The owner's next macOS run is the remaining external check for titlebar,
native menu/activation, IME, and clipboard behavior. No custom macOS code is
introduced by this migration; it returns to Zed's maintained `gpui_macos`
backend.
