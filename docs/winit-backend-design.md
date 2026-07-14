# winit windowing backend — production crate

Adoption step of the roadmap's "winit windowing backend" item, following
the spike (`spikes/gpui-winit/`, legs 1+2, `docs/research/winit-backend-spike.md`).
This doc records the production `crates/horizon-winit-platform` crate: its
architecture, what differs behaviorally from gpui's own per-OS backends,
and (historically) what was open before the default flipped and before
every OS was unified on it. Investigations and bugs found and fixed along
the way are consolidated in "Resolved incidents" at the end, in the order
they happened, rather than interleaved with the architecture description.

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

**Update 2026-07-14: that verification gate has now run.** The first
macOS build surfaced three real gaps (all fixed same-session) and then
passed the full quality gate plus the headless GUI check on the owner's
Apple Silicon machine — see "macOS bring-up" under "Resolved incidents"
for what broke and the decisions made fixing it.

Earlier history, kept for context: **2026-07-12, flipped to the only Linux
path.** The owner dogfooded the `"winit"` backend (decorations, IME,
mouse, and clipboard all confirmed working) and approved making it the
sole Linux windowing path; the `HORIZON_WINDOWING`/`[ui] windowing` opt-in
switch was removed then (superseded now — there is no switch of any kind
left, on any OS). The "Exit criteria for flipping the default" section
below (now folded into "Resolved incidents") is kept as a historical
record of what was verified (and what wasn't) going into that decision —
it no longer describes open work.

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
- `input_trace.rs` — the winit-side half of the permanent
  `HORIZON_INPUT_TRACE` diagnostic facility (see AGENTS.md's GUI
  Verification section); `src/input_trace.rs` carries the gpui-side half.
  Duplicated deliberately rather than shared, since the two crates have no
  other reason to depend on each other.

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
its `.when(!self.native_decorations, ...)` `TitleBar` child (`src/workspace.rs`,
both since deleted) are both gone; `WorkspaceShell::new` dropped the
corresponding constructor parameter. `src/main.rs`'s `WindowOptions.titlebar`
is now a plain `TitlebarOptions { title: Some("Horizon".into()),
appears_transparent: false, traffic_light_position: None }` —
`WinitPlatform::open_window` only ever reads `titlebar.title` (see
`platform.rs`), so the transparency/traffic-light fields (gpui-component's
own hand-drawn-titlebar concept) are moot now and set to their plain
defaults.

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

**macOS verified 2026-07-14** (build + gate + headless GUI check on the
owner's machine — see "macOS bring-up" under "Resolved incidents");
Windows remains unbuilt, recorded here as *intended* behavior reviewed by
symmetry with the Linux implementation and gpui's own `Platform`
contract, not as observed behavior.

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
  behavior. See "Resolved incidents" below for the physical-key/IME-commit
  double-processing risk this carried over from `gpui_linux` (backlog-30)
  and how it was fixed.
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

**Update 2026-07-14**: the macOS half of this debt is paid — the crate
now compiles and runs on the owner's Apple Silicon machine, the full
quality gate passes, and `scripts/check-gpui-terminal.sh` passes all
assertions (marker, 256-color, truecolor). The section above is kept as
the honest record of what "reviewed but never compiled" bought: three
real gaps slipped through (see "macOS bring-up" under "Resolved
incidents"). Windows remains in the never-compiled position.

## Verification

Ran headless (`HORIZON_GPUI_DUMP`/`HORIZON_GPUI_DRIVE` taps, see the
`gui-verify` skill) with `HORIZON_WINDOWING=winit` (historical — this env
var no longer exists; see "Resolved incidents" / the top of this doc for
the unification), isolated `HORIZON_SESSIOND_SOCKET`/`HORIZON_WORKSPACE_STATE`/
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

## Resolved incidents

Bugs found and fixed during this crate's development and dogfooding, kept
in chronological order for context on why the code (guards, the text
fallback, `completed_frame()`'s no-op) looks the way it does. All are
resolved; nothing here describes open work.

### Configure stall: lost main-thread wakeup on Wayland (fixed 2026-07-12)

Symptom (owner's daily driver, captive-reproducible on GNOME/Wayland):
Horizon freezes on its last-painted frame forever — most visibly stuck on
"Restoring session..." — while the app model keeps progressing (the control
plane still answers, background threads keep running); only the window
stops repainting. `WAYLAND_DEBUG=1` traces showed the content surface
committing exactly once, ever, then going completely silent (client and
compositor) for the rest of the process's life, with no correlation to
resize/focus events that followed.

Root cause: `PlatformWindow::draw` (`window.rs`) performs the *actual* GPU
present (`gpui_wgpu::WgpuRenderer::draw`'s internal `frame.present()`,
which issues the real `wl_surface.commit()`), and only *afterward* does
gpui call `completed_frame()` — which used to forward to
`winit::Window::pre_present_notify()`. On Wayland that arms a
`wl_surface.frame` request (per the protocol, "the frame request takes
effect on the next `wl_surface.commit`"), but by the time it fired the
matching commit had already gone out *without* it attached — so the
request sat queued, orphaned, associated with no commit the compositor
would ever see. winit's own Wayland backend refuses to deliver any further
`WindowEvent::RedrawRequested` while a frame-callback request it sent is
still outstanding (`frame_callback_state == Requested`, gating
`RedrawRequested` in `winit`'s `wayland/event_loop/mod.rs`) — so once one
request got orphaned, no future redraw could ever be requested again,
including the one a resize's `WindowEvent::Resized` handler asks for via
`request_redraw()`. This reproduced on **every** run under a plain
`timeout 12 ./target/debug/horizon` (isolated env, real desktop) — not
merely "sometimes" — because it happens on literally the first frame's
`completed_frame()` call; the intermittent *user-visible* symptom the
owner saw was just whether some other legitimate redraw happened to sneak
past before the orphaned request's turn came up.

Fix: `WinitPlatformWindow::completed_frame()` no longer calls
`pre_present_notify()` at all — it's a deliberate no-op. Pacing doesn't
regress: `WgpuSurfaceConfig::preferred_present_mode` is `None`, so
`gpui_wgpu` configures the surface Fifo, and the *blocking*
`get_current_texture`/`present` calls inside `draw` already provide real
vsync pacing while focused (this was already documented — see
`app_handler.rs`'s module doc); the ~30fps inactive-window cap is gpui's
own wall-clock `min_frame_interval` throttle, independent of any platform
hook. The invariant this fix enforces: **every `configure`/resize
deterministically leads to a fresh commit** — `Resized`/`ScaleFactorChanged`
call `request_redraw()`, and since winit's Wayland gate can no longer get
stuck (it never arms a frame-callback request to begin with), that
`RedrawRequested` is guaranteed to be delivered.

This is a Wayland protocol-ordering bug, not a dispatcher-level lost
wakeup — `dispatcher.rs`'s `dispatch_on_main_thread` (`EventLoopProxy`-based)
was audited and has a regression test
(`dispatcher::tests::concurrent_main_thread_posts_all_get_processed`)
confirming concurrent background-thread posts are never dropped. Why the
existing headless checks (`scripts/check-gpui-terminal.sh`,
`scripts/check-workspace-restore.sh`) never caught this, confirmed by
running both against the pre-fix binary (still pass, 3/3 and 1/1):
`HORIZON_GPUI_DUMP` (`src/terminal/session.rs`) writes its frame dump
straight from the terminal session model on every session update,
entirely independent of the `gpui`/`gpui_wgpu`/winit paint pipeline this
bug lives in — so both scripts fully verify terminal *content* correctness
without the GPU ever actually presenting a single frame. Neither drives a
real resize either. There's no cheap hook to extend here: the gap is
structural (the taps sit below the whole rendering pipeline, by design —
they need to work headless in CI-less environments with no GPU/compositor
guarantees), so closing it needs a *different* kind of check, which is
what this fix's own validation added instead: a real-desktop, real-compositor
repeated-run protocol smoke (see the review request / worker report for
this item's exact numbers).

### Keyboard input pipeline: a three-stage investigation (2026-07-12)

The owner reported total keyboard-input loss on their daily driver, and
the eventual fix (a missing text-input fallback) went through two wrong
hypotheses first. Kept in order because each stage's negative result
narrowed the next, and the code today (`ImeCommitGuard`, `KeyTextDedup`,
and `crates/horizon-winit-platform`'s text-input fallback) is the sum of
all three, not just the last one.

#### Stage 1: no code regression found; a real but unproven X11/XIM sibling issue

Symptom reported: neither terminal nor agent panes accept *any* keyboard
input on `main` (`7ab2cc1`), while mouse interaction (click, scroll, drag)
keeps working. Input had worked earlier the same day, both in the leg 1/2
spike and in the owner's opt-in dogfooding of this crate at merge
`c952649`.

**Code path ruled out.** `git diff c952649 7ab2cc1` across every file on
the winit→gpui→pane key path — `app_handler.rs`'s `WindowEvent::KeyboardInput`
arm and `dispatch_input`, `window.rs`'s callback wiring and
`set_ime_allowed(true)`, `input.rs`'s keystroke mapping, `workspace.rs`'s
focus wiring (`focus_active`, `track_focus`), and `terminal/mod.rs`'s
`handle_key`/`ime_marked_text` guard — shows no functional change; the only
diffs in that span are macOS-only additions (`macos_menu.rs`, muda wiring)
and the unrelated `completed_frame()` no-op documented above. Confirmed
live: with temporary `eprintln!` instrumentation at each hop (raw winit
`KeyEvent`, the mapped `Keystroke`, `dispatch_input`'s callback result, and
`TerminalView::handle_key`'s entry), a real end-to-end keypress — injected
via `xdotool key --window <XID>` against an isolated build (own
`HORIZON_SESSIOND_SOCKET`/`HORIZON_WORKSPACE_STATE`/etc., never touching the
owner's live instance) under a throwaway `Xvfb :77` display — flows
correctly through every hop with the current, *unmodified* code:
`WindowEvent::KeyboardInput` → `Keystroke` → `PlatformInput::KeyDown` →
`TerminalView::handle_key`. The pipeline itself is intact.

**A real, reproducible key-eating mechanism was found, but on the X11
fallback path, not proven on native Wayland.** The same isolated build,
run instead against the real desktop's XWayland `DISPLAY=:0` (forcing
winit's X11 backend per the spike's technique — unset `WAYLAND_DISPLAY`/
`WAYLAND_SOCKET`), delivered **zero** `WindowEvent::KeyboardInput` events
for real, properly-focused keypresses (confirmed via `xdotool
getactivewindow` reporting the test window active, and real `MouseDown`/
`MouseUp` delivering correctly for a click at the same moment) — every
single injected key vanished, repeatably, across several attempts and
several key values. Toggling one line — `window.set_ime_allowed(true)` →
`false` in `WinitWindowInner::new` (`window.rs`), a temporary diagnostic
edit, reverted after the experiment — immediately restored delivery on the
identical window, identical desktop, identical ibus/mozc session. Root
cause, confirmed by reading winit 0.30.13's own source
(`platform_impl/linux/x11/event_processor.rs:131-179`): with IME allowed,
*every* X11 event is routed through `XFilterEvent` before winit ever sees
it; if the IME (ibus's XIM server here — `ibus-daemon --xim`, running
continuously since before this session) claims an event via
`XFilterEvent`, winit discards it silently and never emits
`WindowEvent::KeyboardInput`. An XIM implementation that's gotten into a
bad state can therefore make a client believe it's receiving *zero* key
events forever, with no error, no crash, and mouse unaffected — matching
the reported symptom exactly. `window.set_ime_allowed(true)` is untouched
in every commit since this crate's introduction (`56f28ba`), including the
dogfooded merge, so this specific line is not itself a code regression;
whatever is different is the ibus/XIM session's own state, not Horizon's
code.

**Why this doesn't fully explain the report.** The owner's daily driver
runs on native Wayland (the sole path now — no `HORIZON_WINDOWING`
override exists to force X11), and winit's Wayland keyboard path
(`platform_impl/linux/wayland/seat/keyboard/mod.rs`) has no equivalent
gate: `wl_keyboard` events reach the client unconditionally, independent
of `zwp_text_input_v3` state — confirmed by grepping that file for any
IME/text-input reference (none). So the XIM mechanism above cannot be
*the* mechanism on Wayland, even though the same physically-stuck IME
daemon is a live suspect for an analogous but different Wayland-side
failure (e.g. GNOME Shell's own compositor-level input-method integration
getting stuck, which is outside winit's or Horizon's code and not
reproducible without safe access to the owner's real compositor). Testing
this directly was not attempted: native Wayland has no safe, targeted
synthetic-injection path available here (`wtype` uses
`zwp_virtual_keyboard_manager_v1`, which has no per-window targeting and
would land on whatever surface the *real* compositor currently treats as
focused — an unacceptable risk on the owner's live shared desktop), and
restarting the shared `ibus-daemon` to test the recovery hypothesis was
correctly declined as another live-desktop side effect.

**Conclusion of stage 1: no code fix landed.** The investigation did not
find a `main`-introduced code regression in the key-dispatch pipeline; it
found and precisely reproduced one real IME-related failure mode (X11/XIM)
that is a plausible sibling of, but not proven identical to, whatever the
owner was hitting on native Wayland.

**Masking question, answered.** `HORIZON_GPUI_DRIVE`
(`src/terminal/session.rs`) does **not** exercise the winit→gpui key path
at all: it spawns a thread that sends `TerminalCommand::Input`/`::Key`
directly on the session's command channel, entirely bypassing
`WindowEvent::KeyboardInput`, `PlatformInput::KeyDown`, gpui's focus/dispatch
tree, and `TerminalView::handle_key`. `scripts/check-gpui-terminal.sh`
(which drives this tap) could not have caught this regression class even
if one existed — same masking pattern as the configure-stall bug above,
where the frame-dump tap sits below the render pipeline; here the drive
tap sits below the *input* pipeline. This is what motivated adding
`HORIZON_INPUT_TRACE` (see below): a permanent, always-available trace of
the real pipeline the owner could capture from their own daily-driver
session, rather than needing another one-off `eprintln!` instrumentation
pass each time.

#### Stage 2: the real mechanism was a `KeyTextDedup` bug, not IME/XIM state

The owner narrowed the symptom further after stage 1: keys are lost
**only** while the Japanese IME is in direct/ASCII input mode; composition
mode works. That inverts the earlier framing — this is a `main`-code bug,
not (only) IME-daemon state.

**Root cause.** `TerminalView::replace_text_in_range`
(`src/terminal/mod.rs`) had a dedup branch for kitty "report all keys"
mode: an ordinary printable keystroke is sent via the Key path
(`handle_key` → `TerminalCommand::Key`) *and* independently echoed by the
platform's text-input pipeline (`replace_text_in_range`) — the second copy
has to be dropped or the terminal double-feeds. The old guard was
`if !was_composing && keys_as_escape_codes { return; }` — it assumed *every*
non-composing commit under kitty mode was one of these echoes. That's true
for ordinary typing, but false for an IME "direct"/ASCII input mode: ibus
can consume the physical key itself (never reaching winit as
`WindowEvent::KeyboardInput`, so `handle_key` never runs) and deliver
*only* `commit_string` — which arrives here as `Ime::Commit` →
`replace_text_in_range` with `was_composing == false` (there was no
preceding marked/preedit text — direct mode has no composition at all).
The old guard couldn't tell that apart from the ordinary case and dropped
the *only* copy — total, silent input loss, exactly reproducing "neither
terminal nor agent panes accept any keyboard input" whenever the owner's
daily-driver fish shell (which sets `TERM=xterm-kitty` and requests kitty
keyboard modes, so this branch is live) had the IME in direct mode.

**Fix.** `KeyTextDedup` (`src/terminal/mod.rs`, next to the pre-existing
`ImeCommitGuard`) replaces the blanket assumption with an actual match:
`handle_key` records the text of every plain-char Key-path send;
`replace_text_in_range` only drops its copy if that record matches the
commit's text within a short window (50ms — generous vs. the same-burst
gap between the two paths, comfortably below human-typing intervals).
An unmatched commit (no recent key, or a mismatched one — the direct-mode
case) now passes through instead of being silently swallowed. Composition
commits are unaffected (`was_composing` still short-circuits before this
check ever runs). Six colocated pure-decision tests
(`src/terminal/tests.rs`) cover the three-case matrix directly on
`KeyTextDedup`: ordinary kitty-mode typing still dedups
(`kitty_mode_typing_drops_the_duplicate_echo`), a composition commit with
no key send is never treated as a duplicate
(`composition_commit_with_no_key_send_is_never_a_duplicate`), and a
direct-mode commit with no prior key passes through
(`direct_mode_commit_with_no_prior_key_is_not_a_duplicate`) — plus
mismatch, one-shot-consumption, and stale-window edge cases.

**Verification attempted, partially inconclusive.** The generic mechanism
— `Ime::Commit` arriving with no matching `WindowEvent::KeyboardInput` —
is directly confirmed structurally (winit's X11 backend can swallow a key
via `XFilterEvent` before ever surfacing `KeyboardInput`, per
`platform_impl/linux/x11/event_processor.rs`) and was observed live:
retesting the real desktop with `handle_ime` instrumented showed genuine
`Ime::Preedit`/commit-cycle traffic with no matching `KeyboardInput` for
the same keystroke while composing. Reproducing *mozc's own* direct-mode
behavior specifically (rather than the generic mechanism) was attempted
in two safe environments — a fully isolated `Xvfb`+private-D-Bus+private
`ibus-daemon` instance (never touching the shared session), and a careful,
minimal real-desktop retest — but the real desktop's ibus was in
composition mode throughout (not direct mode) at the times tested, and
mozc's own mode-switch hotkeys (`Hiragana_Katakana`) didn't register on
the isolated instance (likely missing the `ibus-x11` helper process and/or
per-user keymap config that a full desktop session provides). Forcing
mozc into the owner's exact "just switched to direct mode" state safely,
without touching the owner's live session, was not achieved within this
investigation's time budget. The fix itself does not depend on
reproducing that exact state: it's correct for *any* mechanism that
produces a `replace_text_in_range` call with no matching physical key,
which is the structural class of bug the owner described, and the pure
unit tests exercise that decision directly.

**Why did this work during the pre-unify dogfooding, then?** Two
independent things bit at once and are easy to conflate. First, the
*total* silence found in stage 1 (zero `KeyboardInput`, zero `Ime` events
at all) is consistent with a genuinely stuck XIM/ibus session state — the
owner ran `ibus restart` while narrowing this down, and after that this
session's real desktop showed normal `Preedit`/`Commit` traffic again,
exactly where it had shown nothing before. That's IME-daemon state,
unrelated to any Horizon commit, and plausibly wasn't present (or wasn't
yet stuck) during the earlier dogfooding session. Second, and separately,
the dedup bug itself is a `main`-code issue that predates this
investigation — the `!was_composing && keys_as_escape_codes` guard has
looked like this since kitty-mode reporting was added, so it's plausible
the owner's dogfooding session simply never happened to have the IME in
direct mode while typing in a kitty-mode shell for long enough to notice a
single dropped keystroke (easy to miss; a stuck *ibus session* silences
everything, which is not). Both explanations are consistent with the
evidence; distinguishing them further would need the owner's own memory of
which IME mode they were in during that session, which isn't recoverable
from logs.

#### Stage 3: the confirmed root cause — a missing text-input fallback

The owner captured a `HORIZON_INPUT_TRACE` trace of their own daily-driver
typing (the diagnostic added in stage 1's aftermath) and it pinpointed the
bug precisely, invalidating both prior hypotheses (X11/XIM state, and "IME
direct mode delivers a commit-only event"):

```
winit KeyboardInput physical_key=Code(KeyA) state=Pressed
handle_key entry key="a"
handle_key key="a" dropped: unmapped (keys_as_escape_codes=false)
```

— and nothing else. No `Ime::Commit` ever arrives for a direct-ASCII-mode
character on this desktop; ibus passes the physical key straight through
as an ordinary `wl_keyboard`/`WindowEvent::KeyboardInput` event, exactly
as it should. Composition mode's own trace (`Preedit('あ')` → `Commit('あ')`
→ `replace_text_in_range` → sent) confirmed that path was never the
problem either.

**Root cause.** `TerminalView::handle_key` (`src/terminal/mod.rs`)
deliberately does nothing for a plain printable key when NOT in kitty
"report all keys" mode — `term_key_code` returns `None` for a bare letter
unless `keys_as_escape_codes` or Ctrl is set, by design: printable text
outside kitty mode is supposed to arrive through the text-input pipeline
(`replace_text_in_range`) instead, matching the Linux convention
`src/terminal/mod.rs`'s own module doc already documented
("plain printable text on Linux arrives only through the
`EntityInputHandler` pipeline, matching gpui_linux"). But
`crates/horizon-winit-platform` never implemented the half of that
convention that makes it true: gpui_linux's own wayland and x11 backends
(`WaylandWindowState::handle_input` /
`X11WindowState::handle_input` in the pinned checkout,
`~/.cargo/git/checkouts/zed-a70e2ad075855582/5f8a741/crates/gpui_linux/src/linux/{wayland,x11}/window.rs`)
both do this after dispatching a `KeyDown`: if gpui's own callback left
`propagate == true` (nothing consumed it) and the keystroke carries
`key_char`, feed that text straight to the active `PlatformInputHandler`
via `replace_text_in_range(None, key_char)` — there's no separate IME
event coming for it. `horizon-winit-platform` had no equivalent, so an
unhandled plain key was silently dropped with nowhere left to go. This
reproduces on every OS `horizon-winit-platform` targets, not just this
one desktop; it just happens that kitty mode (which routes printables
through the Key path instead) and active IME composition (which routes
them through `Ime::Commit`) both mask it, so it only ever surfaced as
"direct-ASCII IME mode eats keys."

**Fix.** `WinitWindowInner::maybe_feed_unhandled_key_as_text` (`window.rs`)
mirrors gpui_linux's fallback: `app_handler.rs`'s `KeyboardInput` handler
now captures `dispatch_input`'s `DispatchEventResult` and, for `Pressed`
events, calls it with the keystroke and `result.propagate`. The decision
itself (`text_fallback_decision`, a free function so it's unit-testable
without a live window) feeds the text only when all of: nothing else
already handled the key (`propagate`), no IME composition is in progress
(a new `WinitWindowState::ime_composing: bool`, set from `Ime::Preedit`/
`Commit`/`Disabled`, mirroring gpui_linux's own `composing: bool` on its
wayland client state — this is the one thing the pinned gpui_linux
snippet doesn't show inline, because on Wayland it lives one layer up
from `handle_input`, and on X11 it's implicit since XIM already swallows
composing keys before they ever reach `KeyboardInput` at all), the
modifiers are plain-or-Shift-only (mirrors gpui_linux's own
`is_subset_of(&Modifiers::shift())` gate — Ctrl/Alt/Cmd combos are never
text), and the keystroke actually carries `key_char`.

**A second bug found and fixed along the way, via live testing (not the
owner's report — this one never reached them):** implementing the
fallback and testing it live (isolated `Xvfb`, `HORIZON_INPUT_TRACE=1`)
immediately surfaced a double-Enter: `handle_key` already sends Enter via
`TerminalCommand::Key` (Enter is in `term_key_code`'s unconditional named-key
list, handled regardless of kitty mode), but since `TerminalView`'s
`on_key_down` never calls `cx.stop_propagation()` anywhere,
`result.propagate` stays `true` even for a key `handle_key` fully handled
— and winit's own `KeyEvent::text` is documented (and confirmed live) to
be `Some("\r")` for Enter, so the new fallback fired too, sending a second
`\r` via `replace_text_in_range`. Root cause:
`winit_key_event_to_keystroke` (`crates/horizon-winit-platform/src/input.rs`)
was copying winit's raw `event.text` into `Keystroke::key_char`
unconditionally, including for every `Key::Named` variant — but
`Keystroke::key_char`'s own documented contract is "the character that
could have been typed," and gpui_linux's `keystroke_from_xkb` never sets
it for named keys either. Fixed at the source: `key_char` is now `None`
for every `Key::Named` variant except `Space` (the one named key
`term_key_code` has *no* unconditional case for — like an ordinary letter,
it only maps under kitty mode or Ctrl, so direct-ASCII-mode delivery
depends on the fallback the same way a letter does; nulling its
`key_char` too silently broke the space bar, caught by the same live
`Xvfb` retest before landing). Verified live end-to-end after both fixes:
`hi there` typed under a direct-ASCII (non-kitty) bash prompt appears
exactly once in the terminal frame, and Enter executes it exactly once
(`handle_key ... sent: TerminalCommand::Key` followed by
`text-fallback skip: SkipNoText`, not a second send).

**Watch-interaction analysis** (from the task brief, resolved via the
`propagate`-never-set-false discovery above rather than needing separate
handling for each):

1. *Kitty mode, both paths could fire.* Since `TerminalView` never calls
   `stop_propagation`, `propagate` stays `true` even when `handle_key`
   already sent a kitty-mode printable via the Key path — so the fallback
   *does* still fire for those too, landing on `KeyTextDedup`
   (`src/terminal/mod.rs`, from stage 2 above) as the actual line
   of defense against a double-feed, exactly as anticipated. Verified live
   (`printf` piped through a working `bash` prompt to flip on kitty
   reporting via `CSI > 1 u`, then typed 'a'): the trace shows `handle_key
   ... sent: TerminalCommand::Key` immediately followed by
   `text-fallback fire` → `replace_text_in_range ... dropped: duplicate of
   a key-path send` — composes correctly, no double character. This is
   also why `KeyTextDedup` is not dead code today even though its
   original 2026-07-12 motivation (guarding against an IME's own kitty-mode
   echo) turned out narrower than believed: the text fallback is now a
   *second*, always-firing source of the same echo whenever propagation
   isn't stopped, which is unconditionally the case in this view.
2. *IME composition must not trigger the fallback.* `ime_composing` gates
   this unconditionally, checked before the modifiers/key_char checks —
   see `text_fallback_decision`'s `SkipComposing` case and its priority
   test (`composing_gate_wins_even_when_also_unhandled_and_has_text`).
3. *A key consumed by a gpui action/keybinding (Tab, List navigation, ...)
   must not text-fallback.* These are all named keys with no `key_char`
   (Tab, arrows, Enter, Escape, ...), so `SkipNoText` excludes them
   regardless of whether anything set `propagate = false` — the same
   `is_named`/`Space`-exception fix above is what keeps this true; before
   it, Tab/Enter/etc. all carried a spurious `key_char` from winit's raw
   `text` field and would have needed `propagate` to actually go false to
   stay excluded, which (per point 1) it doesn't in this codebase today.

**Tests.** `text_fallback_decision` has nine colocated unit tests
(`crates/horizon-winit-platform/src/window.rs`) covering `Feed`, each
`Skip*` reason independently, the composing-wins-over-everything priority
case, and Shift-allowed-but-Ctrl/Alt/Cmd-excluded modifiers. Full
correctness (the fallback firing/not-firing correctly *and* composing
with `KeyTextDedup`) was additionally verified live end-to-end rather than
only at the unit-test seam, since the interaction with the rest of the
input pipeline is exactly what the two live-testing bugs above were.

Backlog #33 files a narrower, still-open follow-up question from this same
investigation (whether a resumed/respawned session's kitty-mode flag
survives correctly) — deliberately not resolved here, per the task brief's
"don't block on it."

### `ImeCommitGuard`: phantom Enter after an IME commit (fixed 2026-07-12, backlog-30)

Found while implementing IME for the winit backend spike (leg 2), then
confirmed live against `TerminalView` itself: Wayland's text-input-v3
protocol (unlike X11's XIM) never lets the compositor consume keys on the
client's behalf, so a physical Enter that confirms an IME conversion still
arrives as an independent `KeyDownEvent`, *after* the `replace_text_in_range`
call that confirmed the composition already cleared `ime_marked_text`. A
naive `ime_marked_text.is_some()` check can't tell that keydown apart from
an ordinary, unrelated keystroke, so the phantom Enter fell through to
normal key handling and sent an extra `\r` to the PTY.

Fixed with a pure `ImeCommitGuard` (`src/terminal/mod.rs`): armed by
`replace_text_in_range` on `was_composing`, consumed unconditionally by
the next `handle_key` call, suppressing only when that key is Enter *and*
it arrived within a 100ms window of the commit — review feedback caught
that a composition committed by mouse click on the candidate window
produces no phantom key at all, so an unbounded guard would swallow a
later genuine Enter (e.g. compose → click candidate → press Enter to send
the line). Covered by unit tests in `src/terminal/tests.rs` for the
single-suppression, rapid-typing, Space/candidate-commit,
consecutive-composition, and within-window/after-window cases. Live repro
with a real IME was out of scope for that pass (native Wayland blocks key
injection); final visual confirmation was left to owner dogfooding, which
has since happened without a reported regression. The agent composer
(`src/agent/view.rs`) uses gpui-component's `Input`/`InputState` widget
rather than a hand-rolled `EntityInputHandler`, so this guard doesn't
apply there — left as-is. Known residual, not handled speculatively: an
IME configured to auto-commit on a punctuation key would deliver that
punctuation as its own phantom key within the window, which this guard
intentionally passes through (only Enter/Return is treated as a plausible
confirming key).

This guard is unrelated to, and unaffected by, stage 3's text-input
fallback above: named keys (including Enter) never carry `key_char` after
that stage's fix, so the fallback never fires for Enter at all —
`ImeCommitGuard` is still the only mechanism suppressing this specific
phantom `KeyDownEvent`.

### Idle CPU: the redraw loop never actually stopped (fixed 2026-07-13)

Symptom (owner-reported, matching an independent dogfooding complaint
"horizon using a lot of CPU"): an isolated, completely idle instance (no
interaction, one fresh terminal pane) burned meaningfully more CPU than a
truly idle GUI app should — measured climbing over the first ~20s of a
`ps -o pcpu` sample series on the reporting environment.

**Instrumented first, per the task brief, rather than guessing.** Added a
permanent per-second counter (`FrameLoopStats`, `app_handler.rs`) tracing
how many `WindowEvent::RedrawRequested` cycles actually ran, via the
existing `HORIZON_INPUT_TRACE` sink. On this investigation's own test host
the counter immediately showed the real mechanism: a **flat, rock-steady
60fps** `RedrawRequested` rate — not accumulating, not multiplying, just
never stopping — for the entire duration of an idle run (60+ seconds
observed). `ps -o pcpu`'s own cumulative-average computation naturally
*ramps toward* a new steady state over its first several samples after a
process jumps from near-zero to sustained load at launch, which is
consistent with — though not conclusively identical to — the "climbing"
shape reported; a companion `/proc/[pid]/stat`-delta measurement (true
per-interval CPU, immune to that ramp artifact) showed a flat ~14–15% the
entire time on this host, not a climb. Either way, the redraw-loop finding
explains sustained elevated idle CPU regardless of which curve shape a
given `ps` sampling protocol shows for it.

**Root cause.** `app_handler.rs`'s `WindowEvent::RedrawRequested` handler
unconditionally called `inner.window.request_redraw()` again at the end of
every cycle — a free-running loop "matching gpui_web's rAF pattern" per
the (now outdated) module doc, intentional at the time. Two compounding
effects made this expensive rather than merely wasteful-but-cheap:

1. gpui's own `on_request_frame` closure (`gpui/src/window.rs`, the
   pinned checkout) only skips a frame's real GPU cost when
   `!request_frame_options.require_presentation` *and* nothing else marks
   `needs_present` — but our handler always passed
   `require_presentation: true`, unconditionally forcing a real
   `window.present()` call every single cycle regardless of
   `invalidator.is_dirty()`. A present at native display refresh rate,
   forever, is not free even when the frame content never changes.
2. Nothing coalesced the request: winit's `request_redraw`/
   `RedrawRequested` contract gives platforms no "is there more to draw"
   signal back from gpui's callback (unlike, say, a real Wayland
   `wl_surface.frame` protocol callback, which only fires again after an
   actual commit — see `gpui_linux`'s wayland backend, which relies on
   exactly that self-terminating chain and passes
   `require_presentation: false` via `RequestFrameOptions::default()`).
   Blindly re-requesting is the only *safe* default with no such signal,
   which is presumably why gpui_web does the same thing — but a browser
   tab's `requestAnimationFrame` gets throttled/paused by the browser
   runtime itself when nothing is visibly changing, a mechanism native
   winit has no equivalent of. Porting the pattern without that safety
   net is what turned "some CPU" into "a continuous 60fps present loop,
   forever, even fully idle."

**Fix: coalesce with an explicit dirty flag, consumed once per
iteration.** `WinitWindowInner::needs_redraw` (`window.rs`, a
`Cell<bool>`) replaces every direct `window.request_redraw()` call outside
the one-time bootstrap in `resumed()`. Every `WindowEvent` that could make
something dirty (`Resized`, `ScaleFactorChanged`, `Focused`,
`KeyboardInput`, `Ime`, `CursorMoved`, `CursorLeft`, `ModifiersChanged`,
`MouseInput`, `MouseWheel`) marks it via `mark_needs_redraw()` instead of
requesting a redraw directly; `CursorMoved`/`CursorLeft`/`ModifiersChanged`
gained an *explicit* mark for the first time (previously they relied
entirely on the free-running loop eventually picking up any resulting
hover/cursor-icon change — a reliance that no longer holds). `user_event`'s
`Wake` case — posted by `WinitDispatcher::dispatch_on_main_thread`/
`dispatch_after` for any gpui main-thread work, which covers everything a
raw `WindowEvent` can't see: animation timers, a background thread's
`cx.notify()` reaching the main thread through `Entity::update` (the
terminal session's own async frame-update loop is exactly this case) —
conservatively marks *every* window dirty, since there's no way to tell
which one (if any) actually needs it from here. `about_to_wait` is the
sole remaining place that calls `winit::window::Window::request_redraw`:
once per event-loop iteration, only for windows whose flag is set,
consuming it (`Cell::take`) as it goes — so a burst of several events in
one iteration still yields exactly one redraw request, and a genuinely
idle window (nothing marks the flag) means `about_to_wait` requests
nothing at all. `RedrawRequested` itself no longer re-arms anything.

**Result.** The same 60s idle measurement now shows near-0% CPU
(`/proc/[pid]/stat`-delta reading 0.0% on essentially every sample; `ps
-o pcpu` decaying from a few percent of pure launch-cost dilution down
toward ~1%) and the frame-loop counter shows single-digit total redraws
over 90+ seconds of idle, instead of thousands. Verified interaction
still repaints promptly: typed characters land in the terminal frame
immediately (checked via `HORIZON_GPUI_DUMP`), and mouse movement alone
(no keyboard/resize activity) still triggers redraws via the new
`CursorMoved` mark.

**No return of the configure stall.** `completed_frame()` — the
Wayland-protocol-specific mechanism the original stall lived in (arming
`wl_surface.frame` via `pre_present_notify`, since fixed by making it a
no-op) — is untouched by this change entirely; this fix only touches
*when* `request_redraw` gets called, never `completed_frame`. The
"every configure/resize deterministically leads to a fresh commit"
invariant the stall fix required still holds: `Resized`/
`ScaleFactorChanged` still unconditionally mark the window dirty, and
`about_to_wait` runs immediately after every batch of `WindowEvent`s, so a
resize still reliably produces a `RedrawRequested` on the very next
iteration. Direct empirical re-verification of the original 15-run
protocol wasn't possible as originally shaped: it depended on
`WAYLAND_DEBUG=1` commit *volume* (hundreds/run) as the "still alive"
signal, which assumed the old free-running ~60fps loop — under the fix,
few-total-commits is now the *correct*, expected idle behavior, so
volume alone can't distinguish "healthy and mostly idle" from "stalled".
It's also not runnable at all as originally scripted: native Wayland
surfaces have no X11 window ID, so there's no `xdotool` handle to script
a resize against without touching the owner's live compositor. Instead,
verified the specific invariant directly — `WindowEvent::Resized`
handling is identical code on every backend winit supports (the platform
that produced the event doesn't change how `app_handler.rs` reacts to
it) — via 5 runs on the X11 backend under an isolated `Xvfb` display,
using this fix's own frame-loop counter as the "did a redraw actually
happen" signal in place of `WAYLAND_DEBUG` commits: 3 resizes each
followed by a check that the redraw counter advanced, then one more
unrelated key input to confirm the window was still alive and repainting
afterward (not stuck) — 5/5 passed.

### Composer IME double-insert: `handle_ime` clearing the marked range before `Commit` (fixed 2026-07-13)

Symptom (owner dogfooding report): confirming a Japanese IME composition
in the **agent pane's composer** inserted the composed string **twice**.
The terminal did not double for the same action — the recent text-input
fallback and dedup work (this section's earlier "Keyboard input pipeline"
entry) had already been verified working there.

**Root cause.** `handle_ime`'s `Ime::Preedit` arm called
`input_handler.unmark_text()` whenever winit reported an *empty* preedit
string. winit consistently emits an empty `Preedit` immediately before
the `Commit` that finalizes a composition — already documented, and
reproduced directly, in `docs/research/winit-backend-spike.md` §16 Q2
("the order `Preedit("", None)` -> `Commit(text)` was consistent" across
every log the leg-2 spike captured). Both `EntityInputHandler`
implementations this crate drives resolve `replace_text_in_range`'s
`None` range to the *current marked range* if one is set, falling back to
the plain text-cursor position only when nothing is marked — confirmed by
reading each implementation directly:

- **gpui-component's `InputState`** (behind the agent composer; pinned
  checkout `crates/ui/src/input/state.rs`): `replace_text_in_range`'s
  range resolves to `range_utf16` if given, else `self.ime_marked_range`
  if set, else `self.selected_range`. `unmark_text` is a *trivial* clear
  (`self.ime_marked_range = None;`) — it does **not** touch `self.text`,
  which already has the preedit content inserted from the earlier
  `replace_and_mark_text_in_range` call. So: unmark (clears
  `ime_marked_range`, leaves the already-inserted preedit text in the
  buffer) → `Commit`'s `replace_text_in_range(None, text)` falls through
  to `self.selected_range` (sitting right after that still-present
  preedit text) instead of the marked range → the same text gets inserted
  a second time, right next to the first.
- **This crate's own terminal** (`src/terminal/mod.rs`) never has this
  *symptom* — its `ime_marked_text` is a client-side-only overlay string,
  never actually written into the PTY buffer during `Preedit`, so there's
  no "already-present text" for a duplicate insert to land next to. But
  the same premature-unmark ordering *was* silently miscomputing
  `was_composing` there too: `replace_text_in_range`'s
  `self.ime_marked_text.take().is_some()` read `false` for a genuine
  composition commit (already cleared by the empty-`Preedit`'s
  `unmark_text` moments earlier), when it should read `true`. This
  happened to stay harmless only because `KeyTextDedup`'s dedup lookup
  never matches a multi-character composed string against a recorded
  single-keypress send — but it also meant `ImeCommitGuard::note_commit`
  was being armed with the wrong value on every real composition commit
  (see "Verification" below for why this needs confirming, not assuming,
  post-fix).
- **gpui_linux's own reference implementation** (pinned checkout,
  `crates/gpui_linux/src/linux/wayland/window.rs::handle_ime`) confirms
  the fix direction: its `ImeInput::InsertText` arm — the wayland client's
  `CommitString` handler routes multi-character commits here — is exactly
  `input_handler.replace_text_in_range(None, &text)`, nothing else. It
  never calls `unmark_text` from a commit path at all.

**Fix.** `handle_ime`'s empty-`Preedit` arm no longer calls
`unmark_text()`. `Commit`'s own `replace_text_in_range` already clears
the marked range as part of doing the replacement in both
implementations (gpui-component: `self.ime_marked_range.take();` at the
end of `replace_text_in_range`; the terminal:
`self.ime_marked_text.take()` at the top), so there's no leak on the
normal compose → commit path — matching gpui_linux's own behavior.
`Ime::Disabled` still explicitly unmarks (a composition genuinely
interrupted by the IME turning off mid-way is an unambiguous signal, unlike
a bare empty `Preedit`). A composition cancelled with no `Commit` and no
`Disabled` (e.g. some IMEs on Escape) can leave a stale marked range until
the next real edit — which naturally overwrites/consumes it via the same
None-range-targets-marked-range convention any subsequent
`EntityInputHandler` call already uses — narrow, self-healing visual
staleness, clearly preferable to silently duplicating committed text.
Traced via a new `input-trace:` line on the empty-Preedit no-op path, for
symmetry with every other `handle_ime` branch.

**Verification.** Confirmed via live testing that this change doesn't
regress the two paths already covered by the prior investigation:

- Composer plain (non-IME) typing: unaffected — this fix only touches the
  empty-`Preedit` branch, which never fires for ordinary keystrokes routed
  through the text-input fallback; confirmed live (isolated instance, real
  desktop, X11-forced), typed characters land in the composer via the
  fallback exactly as before.
- Terminal direct-ASCII: unaffected, confirmed live (same setup) — a
  plain, non-composing key still flows `handle_key` (unmapped, direct
  mode) → text-fallback → `replace_text_in_range` → sent, unchanged.
- `KeyTextDedup`/`ImeCommitGuard`'s own colocated unit tests (already
  covering "given `was_composing=true`, the dedup/suppression logic
  behaves correctly") continue to pass unmodified — those pure structs
  were already correct; only the *integration* (`handle_ime` now actually
  delivering `was_composing=true` to them for a real composition) needed
  fixing, and that's not a decision-level seam this codebase's testing
  convention can isolate without a live `gpui`/window context (no test
  here uses `TestAppContext`/`#[gpui::test]`, matching every prior
  investigation in this doc).

**Not achieved: a full live round-trip of a real multi-character Japanese
composition through either the composer or the terminal**, despite
substantial effort across two approaches:

1. A fully isolated private D-Bus + `ibus-daemon` (+`ibus-x11` helper) on
   a throwaway `Xvfb` display, mirroring the technique used successfully
   for the `ime-direct-ascii` investigation's mozc testing — this time
   the daemon/helper repeatedly failed to establish a working IPC
   connection to each other (`ibus-x11`: "Can not connect to ibus
   daemon"; separately, the daemon's own address-file keying picked up a
   stale `WAYLAND_DISPLAY` from the environment instead of the intended
   isolated `DISPLAY`), and mozc's own mode defaults to direct/ASCII on a
   fresh engine selection with no reliable programmatic way found to force
   Hiragana composition without a working keybinding path.
2. The real desktop's own already-configured ibus/mozc session
   (X11-forced, as used successfully elsewhere in this investigation
   chain) — repeatedly landed in direct-ASCII mode rather than
   composition mode at test time, and the composition that *did* start a
   couple of times (`Preedit('あ')`, `Preedit('い')`) reverted to empty
   before a follow-up `xdotool windowfocus` call (needed to work around
   this same desktop's focus-delivery flakiness, documented throughout
   this doc's other entries) could be avoided — X11 focus-out is a
   plausible, common IME policy trigger for "cancel the current
   composition." One test attempt to try `super+space` (GNOME's default
   input-source-switch shortcut) as a workaround **changed the real,
   system-wide active input source** (a global compositor-level shortcut,
   not scoped to the test window) — caught and reverted immediately
   (`ibus engine mozc-jp`) once noticed, but recorded here as a caution:
   that specific shortcut is not safe to script against a shared desktop.

Given this, the fix is backed by direct, thorough source-code
verification of the exact mechanism in both `EntityInputHandler`
implementations it touches (not inference — the actual
`replace_text_in_range`/`unmark_text` bodies were read line-by-line) and
by this crate's own prior research record of the event ordering it
depends on, rather than by a fresh empirical capture of the full
compose-then-commit round trip. If the owner's next dogfooding session
still shows doubling in either pane, the `HORIZON_INPUT_TRACE`
`winit Ime ...` lines around the `Commit` — specifically whether an
`empty Preedit: not unmarking` line appears immediately before it — are
the first thing to check.

### macOS bring-up: first build and runtime verification (2026-07-14)

The verification gate the 2026-07-12 unification deliberately deferred
("the owner's next macOS build") ran on the owner's Apple Silicon
machine. Three real gaps surfaced, all in code paths a Linux build can
never exercise, all fixed same-session:

1. **gpui's platform-gated API surface.** `gpui::PriorityQueueReceiver`/
   `PriorityQueueSender` are only compiled and re-exported on
   Windows/Linux/wasm (gpui's macOS platform dispatches through Grand
   Central Dispatch and never needs them), so `dispatcher.rs` could not
   import them. Fixed by vendoring the queue into this crate
   (`queue.rs`, verbatim algorithm from zed @ 5f8a741 including the
   loaded-die weighted pop, minus the `spin_*`/`try_iter` variants we
   never call), used on **every** OS — one code path rather than a
   cfg-forked import. Routing Linux through the vendored copy (rather
   than keeping it on gpui's own, which works there) was challenged and
   then ratified by the owner 2026-07-14, on two grounds: a mac-only
   cfg-split would make every future gpui bump that touches the queue
   API cost double (absorb the upstream change on the Linux side while
   keeping the vendored side call-site compatible on mac), and would
   let an upstream semantic change (weights, fairness) quietly split
   dispatcher scheduling behavior between OSes. The accepted trade is
   that upstream queue fixes no longer flow in automatically — both
   OSes are frozen on the vendored copy, identical by construction.
   Note the Linux-gated dispatcher regression test could not run on
   the macOS machine that made this change; the first Linux gate run
   after the merge is its validation (backlog 39).
   Similarly, the `Platform` trait's clipboard surface is cfg-split
   upstream: `read/write_from_primary` exist only on Linux/FreeBSD,
   while macOS instead *requires* `read/write_from_find_pasteboard` —
   `platform.rs` now mirrors the trait's gates exactly, with the find
   pasteboard deliberately stubbed (backlog 38).
2. **`gpui_wgpu` hardcodes VULKAN|GL.** `WgpuContext::instance` (the
   lazy first-window path inside `WgpuRenderer::new`) creates its
   instance with `Backends::VULKAN | Backends::GL` — an empty backend
   set on macOS, so the very first surface creation failed
   ("Failed to create surface for any enabled backend: {}"). Upstream
   main is unchanged as of this writing, and won't feel the gap:
   gpui_wgpu is zed's *Linux* renderer (zed PR #46758); zed itself
   renders macOS through gpui_macos's native Metal path. Fixed at our
   seam: `window.rs` seeds the shared `GpuContext` cell from a
   `Backends::METAL` instance (macOS-gated, so Linux/Windows keep the
   upstream first-window path bit-for-bit) using a temporary surface
   for adapter selection, dropped before the renderer creates its own —
   sequential, never simultaneous, which is the WebGPU-sanctioned shape
   (two live surfaces on one window are UB). Upstreaming a one-line
   backends fix to zed would let us delete the seed (backlog 37).
3. **macOS-only test-harness physics.** The sessiond cwd-inheritance
   e2e test hung 120s on macOS for two stacked reasons: `temp_dir()`
   lives under `/var` (a symlink to `/private/var`) so the shell's
   `$PWD`/sysinfo's sampled cwd (both physical paths) never equal the
   expected string, and the canonicalized path is long enough to wrap
   an 80-column PTY, splitting the needle across a `'\n'` in
   `frame.text`. Fixed in the test: canonicalize the expected path and
   widen the spec to 200 columns.

Also caught: two clippy `unnecessary_to_owned` lints in
`macos_menu.rs` — the first time that file was ever compiled — and one
operational gotcha now recorded in the `gui-verify` skill: the check
script's window takes focus, so owner keystrokes during the ~10s run
land in the checked terminal ahead of `HORIZON_GPUI_DRIVE` and corrupt
the driven command.

Verified green after the fixes: `cargo build --workspace`, the full
gate (fmt / clippy `-D warnings` / nextest, 803 passed 4 skipped), and
`scripts/check-gpui-terminal.sh` (marker + `Indexed(208)` + `Spec(Rgb`
spans all present). Build setup for this host (Homebrew DuckDB 1.5.4 +
`DUCKDB_LIB_DIR`/`DUCKDB_INCLUDE_DIR` via `.envrc.local`) is recorded
in AGENTS.md "Build setup".

### Exit criteria for flipping the default (historical, superseded)

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
