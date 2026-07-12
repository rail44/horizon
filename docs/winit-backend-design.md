# winit windowing backend — production crate

Adoption step of the roadmap's "winit windowing backend" item, following
the spike (`spikes/gpui-winit/`, legs 1+2, `docs/research/winit-backend-spike.md`).
This doc records the production `crates/horizon-winit-platform` crate: its
architecture, what differs behaviorally from gpui's own per-OS backends,
and (historically) what was open before the default flipped and before
every OS was unified on it.

**2026-07-12: unified every OS on `horizon-winit-platform`, `gpui_platform`
removed.** Owner decision: the per-OS windowing split (winit on Linux,
gpui's own `gpui_platform` — `gpui_macos`/`gpui_windows` — everywhere else)
was itself a weakness, not a stable end state — two backends means two
things to keep behaviorally consistent, twice the surface for platform
drift, and a permanent "did the macOS path bit-rot" question. Since winit
and `gpui_wgpu` were already cross-platform (only a handful of spots in
`crates/horizon-winit-platform` were genuinely Linux-only — see
"Architecture" below), unifying was a removal, not a rewrite:
`src/main.rs`'s `build_application` is now one unconditional
`Application::with_platform(horizon_winit_platform::platform())` call, the
root `Cargo.toml`'s `gpui_platform` dependency and target-gating on
`horizon-winit-platform` are both gone (see "No more per-OS backend
selection" below), and Horizon's own hand-drawn `TitleBar` is deleted
outright — winit now draws complete native chrome on every OS, so there is
no case left where Horizon needs to draw its own (see "TitleBar removed
entirely" below).

**What this trades on macOS**: `gpui_macos` is zed's own mature, long-used
backend; `horizon-winit-platform`'s macOS support is new code, written and
reviewed on this pass but **never built** (no macOS SDK on this host — see
"Verification limits" below) — the owner's next macOS build is the actual
verification gate, not this doc. The concrete gap this unification opens on
macOS: gpui_macos's own native `NSMenu`/`NSApplication` integration is
replaced by a hand-rolled `muda`-based menu (see "macOS: native app menu"
below) that covers exactly what Horizon sets today (one "Horizon" menu, one
"Quit Horizon" item) and nothing more — no accelerator labels, no dynamic
enable/disable, no Services menu, no dock menu.

Earlier history, kept for context: **2026-07-12, flipped to the only Linux
path.** The owner dogfooded the `"winit"` backend (decorations, IME,
mouse, and clipboard all confirmed working) and approved making it the
sole Linux windowing path; the `HORIZON_WINDOWING`/`[ui] windowing` opt-in
switch was removed then (superseded now — there is no switch of any kind
left, on any OS). The "Exit criteria for flipping the default" section
below is kept as a historical record of what was verified (and what
wasn't) going into that decision — it no longer describes open work.

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
  functionality out of scope (credentials, path prompts, screen capture) —
  see spike §8 for which stubs are actually load-bearing at runtime versus
  never hit. `set_menus`/`activate` are real (not stubs) on macOS — see
  "macOS: native app menu" below — and documented no-ops on Linux/Windows.
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
- `macos_menu.rs` (`#[cfg(target_os = "macos")]` only) — the `muda`-backed
  native app menu; see "macOS: native app menu" below.

Cross-platform: every module except `macos_menu.rs` builds on every OS
(`lib.rs` has no `#[cfg(target_os = "linux")]` gate at all any more). The
few genuinely OS-specific pieces are `#[cfg]`-gated *inside* their module
instead:

- `clipboard.rs`'s primary-selection methods (`read_primary`/
  `write_primary`) are `#[cfg(target_os = "linux")]`, no-ops elsewhere —
  X11/Wayland's separate middle-click-paste buffer has no equivalent on
  macOS/Windows, and `arboard` itself only exports the
  `GetExtLinux`/`SetExtLinux`/`LinuxClipboardKind` types this needs under
  `cfg(all(unix, not(macos/android/emscripten)))`. Plain
  read/write-clipboard (`get_text`/`set_text`) is unconditional — arboard
  supports it on every target Horizon builds for.
- `platform.rs`'s `set_menus`/`activate` branch on `target_os = "macos"`
  (see "macOS: native app menu" below); every other `Platform` method is
  identical on every OS.
- `macos_menu.rs` itself, and the one manifest dependency it needs
  (`muda`), are `#[cfg(target_os = "macos")]`-gated — see the crate's own
  `Cargo.toml` header comment.

`winit`/`gpui_wgpu`/`arboard`/`raw-window-handle`/`uuid` are plain
dependencies in the crate's own `Cargo.toml` (no target gate), and the
root crate's dependency on `horizon-winit-platform` is likewise plain —
`cargo build --workspace` builds this crate's full source on every OS now.

`gpui`/`gpui_wgpu` ride the same unpinned git source as the root crate's
own `gpui` (see the long comment on it in the root `Cargo.toml`) rather
than the spike's pinned rev — mixing a pinned and an unpinned source for
the same crate splits the dependency graph ("multiple different versions
of crate `gpui`"), so production code can't keep the spike's pin.

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

## TitleBar removed entirely

Originally (2026-07-12, the Linux-only flip) `gpui-component`'s `TitleBar`
(`crates/ui/src/title_bar.rs` in the pinned checkout) was still rendered on
non-Linux OSes — it draws its own minimize/maximize/close buttons
*unconditionally*, which only made sense back when `gpui_macos` relied on
Horizon's own hand-drawn titlebar for macOS's transparent-inset
traffic-light layout. Now that winit is the only backend and it draws
complete native chrome on every OS (sctk-adwaita CSD on Linux, native
decorations on macOS/Windows), there is no OS left where Horizon needs to
draw its own bar. `WorkspaceShell`'s `native_decorations: bool` field and
its `.when(!self.native_decorations, ...)` `TitleBar` child (`src/workspace.rs`)
are both deleted; `WorkspaceShell::new` dropped the corresponding
constructor parameter. `src/main.rs`'s `WindowOptions.titlebar` is now a
plain `TitlebarOptions { title: Some("Horizon".into()), appears_transparent:
false, traffic_light_position: None }` — `WinitPlatform::open_window` only
ever reads `titlebar.title` (see `platform.rs`), so the transparency/
traffic-light fields (gpui-component's own hand-drawn-titlebar concept)
are moot now and set to their plain defaults.

`gpui-component-assets`' `.with_assets` registration in `src/main.rs`
stays, despite `TitleBar` (the original reason it was added) being gone:
`List`, `Button`, and `TextView` — all still in active use
(`palette.rs`/`session_manager.rs`/`view_chooser.rs`'s `List`,
`agent/view.rs`'s `Button`/`TextView`) — resolve their own bundled icons
through the same registered asset source (confirmed by grepping
`gpui-component`'s `list/`, `button/`, and `text/` modules for
`IconName`/icon usage). Removing it would silently blank those icons.

## No more per-OS backend selection

`src/main.rs`'s `build_application` is one unconditional call:
`Application::with_platform(horizon_winit_platform::platform())` — no
`#[cfg]`, no tuple return, nothing OS-specific left at this call site. The
root `Cargo.toml` mirrors this: `horizon-winit-platform` is a plain
`[dependencies]` entry (was `[target.'cfg(target_os = "linux")'.dependencies]`);
the `gpui_platform` dependency (gpui's own platform backend, previously
used on non-Linux) is removed outright — nothing in this crate's own
source calls `gpui_platform::application()` any more.

This **does** shrink the dependency graph, correcting an assumption made
going into this task: the task brief expected `gpui-component` to still
pull `gpui_platform` in transitively regardless (its *workspace* root
`Cargo.toml` does depend on `gpui_platform`, for its own demo/story
binary), but the `gpui-component` *library* crate Horizon actually depends
on (`crates/ui` in the pinned checkout, published as the `gpui-component`
crate) does not. Confirmed directly: `git diff Cargo.lock` for this change
shows `gpui_linux`/`gpui_macos`/`gpui_platform`/`gpui_web`/`gpui_windows`
all disappearing (871 deleted lines against 59 inserted, dominated by
those five packages' own transitive trees — font-kit, cocoa/objc2,
windows-sys, wayland/x11 crates only `gpui_platform` needed) once this
crate's own `gpui_platform` dependency was removed; grepping the new
`Cargo.lock` for any of those five names returns nothing.

## macOS: native app menu

Winit itself draws no menus (it's a windowing-only crate) — `muda`
(`crates/horizon-winit-platform/src/macos_menu.rs`) is the standard winit
companion for this, evaluated and chosen because its README documents the
exact winit integration pattern this crate already uses elsewhere
(`muda::MenuEvent::set_event_handler` forwarding through an
`EventLoopProxy` as a user event, matching how `WinitDispatcher` already
wakes the loop for background-thread work — see `dispatcher.rs`).

- **`Platform::set_menus`** (`platform.rs`, gated `#[cfg(target_os = "macos")]`)
  hands gpui's `Vec<Menu>` tree to `MacosMenuState::set_menus`
  (`macos_menu.rs`), which walks it recursively
  (`Action`/`Separator`/`Submenu`; `SystemMenu` — macOS's OS-managed
  Services menu — is the one variant left unimplemented, since Horizon
  doesn't set one) into a `muda::Menu`, assigns each `Action` item a fresh
  `muda::MenuId`, stores `MenuId -> Box<dyn Action>` in a `RefCell<HashMap>`,
  and calls `muda::Menu::init_for_nsapp()`. Scope matches exactly what
  Horizon sets today (`src/main.rs`): one "Horizon" menu, one "Quit
  Horizon" action item. Menu items carry no accelerator (no
  `muda::accelerator::Accelerator` derived from gpui's `Keymap`) — the one
  shortcut Horizon binds today (`cmd-q` -> `Quit`) already works via
  gpui's own window-level keybinding dispatch regardless of what the OS
  menu displays; wiring `Keymap` -> `Accelerator` conversion is left for
  when Horizon actually needs menu-displayed shortcuts.
- **Click -> action dispatch.** A menu click fires `muda`'s process-global
  `MenuEvent` channel; `WinitPlatform::new` registers a handler once
  (`muda::MenuEvent::set_event_handler`) that forwards it through the
  winit event loop as a new `WinitUserEvent::MenuEvent` variant (gated
  `#[cfg(target_os = "macos")]` in `app_handler.rs`, which also dropped
  the enum's `Copy` derive since `muda::MenuEvent` isn't `Copy`).
  `WinitAppHandler::user_event` routes it to
  `WinitPlatform::dispatch_menu_action`, which looks the `MenuId` up in
  `MacosMenuState`'s map and invokes the `Box<dyn Action>` through
  whatever callback `Platform::on_app_menu_action` registered — gpui's own
  `init_app_menus` (called unconditionally from `App`'s constructor) wires
  that callback to `cx.dispatch_action`, so a click ends up going through
  exactly the same action-dispatch path `cmd-q` does. `on_app_menu_action`
  itself now actually stores the callback (`RefCell<Option<Box<dyn FnMut(&dyn
  Action)>>>` on `WinitPlatform`) instead of discarding it — the
  pre-unification stub silently dropped it, which would have made any
  macOS menu click a no-op even if a menu had been drawn.
- **`Platform::activate`** focuses every open window
  (`WinitWindowInner::window.focus_window()`) on macOS. There is no winit
  API for "activate the whole app" (as opposed to one window) post-launch;
  `WinitPlatform::new` also sets `ActivationPolicy::Regular` via
  `EventLoopBuilderExtMacOS::with_activation_policy` at event-loop-build
  time, which is what actually gives the process a normal Dock
  icon/menu-bar identity and gets it activated on launch (a bare `cargo
  run` binary has no bundle `Info.plist` to read a policy from
  otherwise). A real `NSApp.activate(ignoringOtherApps:)` call isn't
  exposed by winit 0.30's public API; this is the "precisely-documented
  fallback" for that gap, not a silent drop of the requirement.
- **Linux and Windows** keep `set_menus`/`activate` as documented no-ops
  (`#[cfg(not(target_os = "macos"))]` branches in `platform.rs`) — Linux
  never had this gap (sctk-adwaita's CSD carries no menu bar to begin
  with, and GNOME/Mutter's app-menu convention is a separate, unrelated
  mechanism Horizon doesn't target), and Windows menu-bar support is
  explicitly out of scope for this pass (see "Out of scope" below).

## What differs behaviorally from `gpui_macos`/`gpui_windows`

**Unbuilt on this host — see "Verification limits" below.** This section
records what the code is *intended* to do, reviewed by symmetry with the
Linux implementation and gpui's own `Platform` contract, not what's been
observed running.

- **Decorations.** Native macOS/Windows chrome via winit's own
  `WindowAttributes::with_decorations(true)` (unconditional, same call as
  Linux — see `platform.rs::open_window`) instead of `gpui_macos`'s
  transparent-titlebar-plus-traffic-lights setup or `gpui_windows`'s own
  chrome. `window_decorations()` always reports `Decorations::Server`
  (same rationale as Linux — see below).
- **App menu.** `muda`-backed, not `gpui_macos`'s native `NSMenu`
  integration — see "macOS: native app menu" above for exactly what's
  covered and what isn't (no accelerators, no dynamic enable/disable, no
  Services menu, no dock menu).
- **Activation.** `ActivationPolicy::Regular` + per-window
  `focus_window()`, not a direct `NSApp.activate(ignoringOtherApps:)` call
  — see "macOS: native app menu" above.
- **IME/keyboard/mouse/cursor/clipboard.** Same code paths as Linux
  (`app_handler.rs`/`input.rs`/`cursor.rs`/`clipboard.rs` have no
  Linux-specific gates beyond primary selection — see "Architecture"
  above), so whatever behavioral parity or gaps exist on Linux (documented
  below and in "Verification") apply equally to macOS/Windows, modulo
  actually being unbuilt there.
- **Window appearance.** Same `WindowAppearance::Dark` stub as Linux (see
  below) — macOS/Windows *do* have a real light/dark query winit could
  expose, unlike Linux's freedesktop-portal-only situation, but Horizon's
  theme is entirely config-driven and reads `WindowAppearance` nowhere, so
  this wasn't implemented for either OS.

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

## Out of scope

Multi-window, screen capture, drag&drop file opening, Windows menu-bar
support (`set_menus`/`activate` stay documented no-ops there — see "macOS:
native app menu" above), and — within the macOS menu itself — accelerator
labels, dynamic enable/disable, the Services menu, and the dock menu (see
the same section). Also carried over from the spike: native-Wayland
preedit *content* observation remains unverified (only confirmed via
winit's X11 fallback backend, spike §14.2-14.3 — same `winit::event::Ime`
code path either way, so this doesn't affect the code itself, only how
confidently the content was watched).

## Verification limits

This host is Linux-only: there is no macOS SDK, so the macOS-gated code
(`macos_menu.rs`, and the `#[cfg(target_os = "macos")]` branches in
`platform.rs`/`app_handler.rs`/`clipboard.rs`) has **never been compiled**,
only written and reviewed by symmetry with the Linux implementation and
against gpui's/muda's/winit's documented APIs (source read directly from
the pinned/registry checkouts, not from memory). **The owner's next macOS
build is the actual verification gate for all of it** — menu
construction, the click -> `Action` dispatch path, and activation. Windows
is in the same unbuilt position but carries less new code (menus/
activation stay no-ops there, same as before).

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
