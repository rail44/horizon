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

## Known failure mode: lost main-thread wakeup / configure stall (fixed)

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
confirming concurrent background-thread posts are never dropped. Why the existing headless checks (`scripts/check-gpui-terminal.sh`,
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

## Investigation: total keyboard-input death on the owner's daily driver (2026-07-12)

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

**Conclusion: no code fix landed.** The investigation did not find a
`main`-introduced code regression in the key-dispatch pipeline; it found
and precisely reproduced one real IME-related failure mode (X11/XIM) that
is a plausible sibling of, but not proven identical to, whatever the owner
is hitting on native Wayland. The fastest next diagnostic for the owner:
switch input source / restart the input method (or log out and back in)
and see whether that alone restores keyboard input in the *existing*
running Horizon — if it does, this is IME-daemon state, not a Horizon bug,
and no code change is needed here.

**Masking question, answered.** `HORIZON_GPUI_DRIVE`
(`src/terminal/session.rs`) does **not** exercise the winit→gpui key path
at all: it spawns a thread that sends `TerminalCommand::Input`/`::Key`
directly on the session's command channel, entirely bypassing
`WindowEvent::KeyboardInput`, `PlatformInput::KeyDown`, gpui's focus/dispatch
tree, and `TerminalView::handle_key`. `scripts/check-gpui-terminal.sh`
(which drives this tap) could not have caught this regression class even
if one existed — same masking pattern as the configure-stall bug above,
where the frame-dump tap sits below the render pipeline; here the drive
tap sits below the *input* pipeline. No cheap fix is proposed: a real
synthetic-injection check would need either a safe native-Wayland
injection story (none available on this host) or a permanently-running
`Xvfb`+`xdotool` smoke gated behind an explicit opt-in env var, which is a
new piece of test infrastructure, not a one-line addition — left for a
follow-up if the owner wants it.

### Follow-up, same day: the real mechanism was a dedup bug, not IME/XIM state

The owner narrowed the symptom further after the investigation above:
keys are lost **only** while the Japanese IME is in direct/ASCII input
mode; composition mode works. That inverts the earlier framing — this is
a `main`-code bug, not (only) IME-daemon state, and it's now fixed.

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
*total* silence found in the investigation above (zero `KeyboardInput`,
zero `Ime` events at all) is consistent with a genuinely stuck XIM/ibus
session state — the owner ran `ibus restart` while narrowing this down,
and after that this session's real desktop showed normal
`Preedit`/`Commit` traffic again, exactly where it had shown nothing
before. That's IME-daemon state, unrelated to any Horizon commit, and
plausibly wasn't present (or wasn't yet stuck) during the earlier
dogfooding session. Second, and separately, the dedup bug itself is a
`main`-code issue that predates this investigation — the `!was_composing
&& keys_as_escape_codes` guard has looked like this since kitty-mode
reporting was added, so it's plausible the owner's dogfooding session
simply never happened to have the IME in direct mode while typing in a
kitty-mode shell for long enough to notice a single dropped keystroke
(easy to miss; a stuck *ibus session* silences everything, which is not).
Both explanations are consistent with the evidence; distinguishing them
further would need the owner's own memory of which IME mode they were in
during that session, which isn't recoverable from logs.

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
