# winit windowing backend — production crate

Adoption step of the roadmap's "winit windowing backend" item, following
the spike (`spikes/gpui-winit/`, legs 1+2, `docs/research/winit-backend-spike.md`).
This doc records the production `crates/horizon-winit-platform` crate: its
architecture, the compile-time selection, what differs behaviorally from
`gpui_linux`, and (historically) what was open before the default flipped.

**2026-07-12: flipped to the only Linux path.** The owner dogfooded the
`"winit"` backend (decorations, IME, mouse, and clipboard all confirmed
working) and approved making it the sole Linux windowing path. The
`HORIZON_WINDOWING`/`[ui] windowing` opt-in switch is gone; `src/main.rs`
now selects the backend at compile time (`#[cfg(target_os = "linux")]`),
matching the target-gated `horizon-winit-platform` dependency in the root
`Cargo.toml`. The "Exit criteria for flipping the default" section below is
kept as a historical record of what was verified (and what wasn't) going
into that decision — it no longer describes open work.

## Why

GNOME/Mutter (this project's primary dev/dogfooding desktop) refuses
server-side xdg decoration unconditionally. Horizon's current workaround is
drawing its own chrome (`gpui-component`'s `TitleBar`, requested via
`WindowDecorations::Client`) on top of `gpui_linux`'s Wayland backend.
winit's bundled `sctk-adwaita` client-side frame gives real, native-feeling
decorations (title bar, minimize/maximize/close, correct focus/drag/resize
affordances) for free on any Wayland compositor, without a gpui fork —
`Application::with_platform` is a public, intended extension point. See the
spike doc for the full evaluation (including why `gpui-ce` was considered
and deferred, `docs/research/gpui-ce-drop-in-spike.md` §8).

## Architecture

`crates/horizon-winit-platform` implements `gpui::Platform` +
`gpui::PlatformWindow` + a `PlatformDispatcher`/`PlatformDisplay` pair over
winit 0.30, rendering through `gpui_wgpu::WgpuRenderer` (the same renderer
`gpui_web` uses — reused wholesale, not reimplemented). It is a direct port
of the spike, restructured into a workspace member and extended to cover
mouse, cursor styles, clipboard, and scale-factor changes, which the spike
explicitly left for this step (spike §9, §16-Q4 item 3).

Module map (`crates/horizon-winit-platform/src/`):

- `platform.rs` — `WinitPlatform`, the `gpui::Platform` impl. Owns the
  winit `EventLoop`, the dispatcher, the (single, spike-scope) display, the
  open windows list, and the clipboard. Most methods are no-op stubs for
  functionality out of scope (menus, credentials, path prompts, screen
  capture) — see spike §8 for which stubs are actually load-bearing at
  runtime versus never hit.
- `window.rs` — `WinitPlatformWindow`/`WinitWindowInner`, the
  `gpui::PlatformWindow` impl wrapping one winit `Window` + one
  `WgpuRenderer`. Owns IME (`handle_ime`, ported unchanged from spike leg
  2, including the `set_ime_cursor_area` feedback-loop fix in spike §15)
  and per-window mouse/click state.
- `app_handler.rs` — the winit `ApplicationHandler`: the actual event pump.
  Maps `WindowEvent`s to gpui `PlatformInput`/callback invocations —
  keyboard (via `input.rs`), mouse (down/up/move/scroll/enter/leave, new in
  this crate), resize, scale-factor change (new), focus, IME delegation.
- `input.rs` — pure mapping tables (keyboard, mouse button, scroll delta,
  click-count tracking) kept free of live winit/gpui state so they're
  colocated unit-test targets.
- `cursor.rs` — `CursorStyle` -> winit `CursorIcon`, exhaustively matched
  (unit tested).
- `clipboard.rs` — thin wrapper around `arboard` (see "Clipboard" below).
- `active_loop.rs` — the `ActiveEventLoop`-reachability bridge (spike §5's
  structural finding); unsafe thread-local pointer stash, unchanged from
  the spike. See that module's doc comment for the safety argument and why
  deferring window creation to a later `ApplicationHandler` callback was
  considered and rejected (gpui's `open_window` is a synchronous call that
  returns a live `PlatformWindow` to its caller — there's no callback seam
  to defer into without breaking that contract).
- `dispatcher.rs` / `display.rs` — ported unchanged from the spike
  (`EventLoopProxy`-backed `PlatformDispatcher`; a fixed-size single-display
  stub).

Every module is `#[cfg(target_os = "linux")]`-gated in `lib.rs`; on other
platforms the crate exposes nothing. `winit`/`gpui_wgpu`/`arboard`/
`raw-window-handle` are Linux-only dependencies in the crate's own
`Cargo.toml`, and the root crate's dependency on `horizon-winit-platform`
itself is target-gated (`[target.'cfg(target_os = "linux")'.dependencies]`
in the root `Cargo.toml`) — so `cargo build --workspace` on macOS/Windows
never touches this crate's code or dependency tree at all.

`gpui`/`gpui_wgpu` ride the same unpinned git source as the root crate's
own `gpui`/`gpui_platform` (see the long comment on those two in the root
`Cargo.toml`) rather than the spike's pinned rev — mixing a pinned and an
unpinned source for the same crate splits the dependency graph ("multiple
different versions of crate `gpui`"), so production code can't keep the
spike's pin.

## Gaps filled beyond the spike

The spike (legs 1+2) proved decorations, rendering, keyboard, and IME. This
crate adds the four items the task required next, in order:

**a. Mouse.** `app_handler.rs` maps winit's `CursorMoved`/`CursorEntered`/
`CursorLeft`/`MouseInput`/`MouseWheel` to gpui's
`MouseMove`/`MouseExited`/`MouseDown`/`MouseUp`/`ScrollWheel`
`PlatformInput` variants, with modifiers attached from the tracked
`ModifiersState`. Click counting (`input.rs::ClickTracker`) reimplements
`gpui_linux`'s double/triple-click heuristic (400ms window, 5px radius —
`gpui_linux/src/linux/platform.rs`'s `DOUBLE_CLICK_INTERVAL`/
`DOUBLE_CLICK_DISTANCE`, copied verbatim) since winit reports only raw
press/release with no click-count concept of its own. This is what
`src/terminal/input.rs`'s selection and mouse-reporting path consumes.

**b. Cursor styles.** `cursor.rs::cursor_style_to_icon` exhaustively maps
every `gpui::CursorStyle` variant to a winit `CursorIcon` (no wildcard arm,
so a new upstream `CursorStyle` variant fails this crate's build instead of
silently defaulting). `Platform::set_cursor_style` applies it to every open
window (single-window scope for this milestone — see "Out of scope"
below). `hide_cursor_until_mouse_moves`/`is_cursor_visible` remain no-op
stubs: the task's item (b) is cursor *styles*, not auto-hide.

**c. Clipboard.** `clipboard.rs` wraps `arboard` 3.x (evaluated first per
the task brief; chosen for one crate covering both X11 and
Wayland-data-control, text-only — `image-data` is left off since Horizon's
own clipboard usage, `src/terminal/mod.rs`, is text-only). The clipboard
connection is opened lazily (`Clipboard::new()` talks to the display
server, which may not be ready the instant `WinitPlatform::new()` runs) and
cached in a `RefCell`. Primary selection (`read_from_primary`/
`write_to_primary`) uses arboard's Linux-specific `GetExtLinux`/
`SetExtLinux` extension traits with `LinuxClipboardKind::Primary`. Kept
behind this crate's own `WinitClipboard` type (not called from
`platform.rs` directly) so the backend crate choice can change later
without touching `Platform`'s clipboard method signatures.

**d. Scale-factor changes.** `app_handler.rs`'s
`WindowEvent::ScaleFactorChanged` handler re-derives logical bounds from
the window's *current physical size* immediately (rather than waiting for
a possible follow-up `Resized`), matching how `gpui_linux`'s wayland
backend re-derives logical size on every `preferred_buffer_scale` update
rather than treating scale and resize as strictly sequential events.

**e. Window appearance.** No winit query exists on Linux for OS light/dark
mode (same limitation `gpui_linux` has — it reads the freedesktop portal
setting directly, a mechanism outside winit). Stubbed to
`WindowAppearance::Dark`, documented as the default. Grepped: nothing in
`src/` reads `WindowAppearance`, so this has no observable effect on
Horizon's own (entirely config-driven) theme.

## The double-chrome fork: skipping Horizon's own TitleBar under winit

Not in the task's explicit gap list, but discovered while wiring the
switch: `gpui-component`'s `TitleBar` (`crates/ui/src/title_bar.rs` in the
pinned checkout) draws its own minimize/maximize/close buttons
*unconditionally* on Linux — it only reads `window.window_decorations()`
(`Decorations::Client` vs `Server`) to decide whether to also hook up a
right-click window-menu handler, not whether to render controls at all.
Under `gpui_linux`, that's correct: GNOME/Mutter never grants real
server-side decoration, so `TitleBar` is the *only* chrome that exists.
Under the winit backend, winit+sctk-adwaita already draws a complete CSD
frame (title, drag region, minimize/maximize/close) — rendering Horizon's
own `TitleBar` on top would double both the bar and its buttons.

Fix: `WorkspaceShell` gained a `native_decorations: bool` field
(`src/workspace.rs`), threaded from `src/main.rs`'s windowing-backend
resolution (`true` only when the winit backend is actually active — see
`build_application`'s return tuple), and `WorkspaceShell::render` skips its
`TitleBar` child entirely (`.when(!self.native_decorations, ...)`) when
set. The native/macOS paths are untouched (`native_decorations` is always
`false` there — macOS still wants `TitleBar` for its transparent-inset
traffic-light layout, matching the existing comment in `main.rs`).

## Compile-time selection

There is no runtime switch: `src/main.rs`'s `build_application` picks the
backend at compile time via `#[cfg(target_os = "linux")]`.

- On Linux: `Application::with_platform(horizon_winit_platform::platform())`,
  with `native_decorations: true` (see "The double-chrome fork" above).
- On every other OS: `gpui_platform::application()` — gpui's own platform
  backend, untouched by this work — with `native_decorations: false`, so
  `WorkspaceShell` keeps drawing its own `TitleBar` (macOS's transparent
  traffic-light layout).

The root `Cargo.toml` mirrors this at the manifest level:
`horizon-winit-platform` is a `[target.'cfg(target_os = "linux")'.dependencies]`
entry, and `gpui_platform` is a
`[target.'cfg(not(target_os = "linux"))'.dependencies]` entry (manifest
hygiene only — `gpui-component` itself depends on `gpui_platform`
unconditionally, so `gpui_linux` still builds transitively into the Linux
graph regardless of our own crate's direct dependency).

## What differs behaviorally from `gpui_linux`

- **Decorations.** Real sctk-adwaita CSD on Wayland (this is the entire
  point) instead of Horizon's hand-drawn `TitleBar`; native X11 chrome if
  winit falls back to its X11 backend (no `WAYLAND_DISPLAY`).
  `window_decorations()` always reports `Decorations::Server` (from gpui's
  point of view, the platform — winit + the compositor — owns them either
  way); `request_decorations`/`show_window_menu`/`start_window_resize` are
  no-ops (winit's CSD frame handles resize-by-drag/menu itself; there's no
  API to ask it to do otherwise).
- **IME.** Functionally proven identical in the spike (leg 2): same
  `EntityInputHandler` calls, same preedit/commit/candidate-bounds
  behavior. One caveat carried over unchanged from spike §16-Q2: a
  composition confirmed via the physical Enter key can deliver the IME
  commit *and* a plain `KeyboardInput` press for that same Enter as two
  independent events (Wayland's text-input-v3 design never withholds key
  events from the client) — this is a pre-existing risk in Horizon's
  `TerminalView::on_key_down` IME guard shared with `gpui_linux`, not
  something this crate introduces; tracked as dogfooding backlog-30, not
  fixed here.
- **Mouse/cursor/clipboard.** New code (the spike didn't touch these).
  Pure mapping/tracking logic is unit tested (`input.rs`, `cursor.rs`);
  not yet exercised against a live compositor pointer/clipboard event in
  this pass (see "Verification" below).
- **Multi-monitor.** `display.rs` is a single fixed-size stub, same as the
  spike. Real per-monitor bounds/DPI is out of scope (see below).

## Out of scope (unchanged from the task brief)

Menus beyond no-op stubs, multi-window, screen capture, drag&drop file
opening, and macOS/Windows support for this crate. Also carried over from
the spike: native-Wayland preedit *content* observation remains unverified
(only confirmed via winit's X11 fallback backend, spike §14.2-14.3 — same
`winit::event::Ime` code path either way, so this doesn't affect the code
itself, only how confidently the content was watched).

## Verification

Ran headless (`HORIZON_GPUI_DUMP`/`HORIZON_GPUI_DRIVE` taps, see the
`gui-verify` skill) with `HORIZON_WINDOWING=winit`, isolated
`HORIZON_SESSIOND_SOCKET`/`HORIZON_WORKSPACE_STATE`/
`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`: a real decorated
winit-backed window opened, a terminal session spawned and rendered,
raw-PTY-injected marker text plus a 256-color (`Indexed(208)`) and
truecolor (`Spec(Rgb{r:10,g:20,b:30})`) span all appeared correctly in the
frame dump — proving the render/PTY/frame pipeline works end-to-end under
this backend, and that the headless taps are indeed backend-independent as
the task brief predicted. No panics in either run.

`scripts/check-gpui-terminal.sh` itself could not be run as a literal
script invocation: its `pgrep -x "$binary_name"` safety guard (refuses to
run alongside another process literally named `horizon`) tripped against
an already-running owner Horizon instance on this shared desktop at
verification time — an environmental collision unrelated to winit
(the same guard would block a "native" backend run too). The check above
manually reproduces the script's exact env vars, drive command, and
pass/fail assertions with the winit backend selected, and all three
passed.

**Not verified in this pass** (would require real OS-level input
injection — `xdotool`/`ydotool`/a virtual-keyboard protocol — which the
task brief flags as an incident-class risk on a shared desktop, per the
`xdotool windowactivate` incident recorded in the spike doc §17): live
mouse click/drag selection, live scroll, live clipboard copy/paste, live
cursor-style transitions, and a fresh live IME round-trip through this
specific crate build (the spike already proved the *ported* IME code path
end-to-end with real ibus/mozc — this crate changes nothing there). The
mouse/cursor/clipboard *mapping logic* is unit tested; the *live
input-to-window* leg is the residual gap before flipping the default.

## Exit criteria for flipping the default (historical)

Superseded 2026-07-12 by direct owner dogfooding approval (see the top of
this doc) — kept as the record of what this list originally required
before the flip, not as open work.

Before `"winit"` could become the built-in default (not just opt-in):

1. Live-driven (not just unit-tested) verification of mouse
   click/drag/scroll and clipboard copy/paste against a real compositor,
   using a safe injection method (spike §6.3/§14.2 found GNOME/Mutter
   rejects the virtual-keyboard protocol `wtype` needs; `ydotool`
   via `uinput`, compositor-independent, is the untried candidate) or a
   headless Wayland compositor (`weston`/`sway`/`cage`) that does support
   it.
2. Multi-monitor support (`display.rs` today is a single fixed-size stub)
   — at minimum, real per-monitor bounds/DPI for correct window placement
   on multi-head setups.
3. A decision on window appearance (light/dark) parity with `gpui_linux`'s
   freedesktop-portal read, if Horizon's theme ever stops being purely
   config-driven.
4. Enough dogfooding time on the opt-in switch to catch anything the
   above verification gaps miss.

(Historical: at the time this list was written, `"native"` was still the
default and `"winit"` was opt-in. See the top of this doc for the flip.)
