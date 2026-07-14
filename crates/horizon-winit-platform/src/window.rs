//! `PlatformWindow` backed by a single winit `Window` + gpui_wgpu's
//! `WgpuRenderer`. Reuses gpui_wgpu's renderer wholesale — none of the
//! rendering pipeline is reimplemented here, only the glue that feeds it a
//! winit-owned surface and drains winit events into gpui's callbacks.
//!
//! Multi-window and window decoration/state control (minimize, zoom,
//! move-by-drag, ...) beyond what's wired below are out of scope for this
//! crate — see docs/winit-backend-design.md. IME (`handle_ime`) mirrors the
//! `zwp_text_input_v3::Event` -> `PlatformInputHandler` dispatch
//! `gpui_linux`'s wayland backend does (`WaylandClientStatePtr`'s
//! `Dispatch` impl in the pinned gpui checkout), just driven by winit's
//! `Ime` enum instead of raw Wayland protocol events — ported unchanged
//! from the spike (docs/research/winit-backend-spike.md §13-15, including
//! the `set_ime_cursor_area` feedback-loop fix in §15).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    px, AnyWindowHandle, Bounds, Capslock, Decorations, DevicePixels, DispatchEventResult,
    GpuSpecs, Modifiers, MouseButton, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, Point, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene,
    Size, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowControls, WindowDecorations, WindowParams,
};
#[cfg(target_os = "macos")]
use gpui_wgpu::{wgpu, WgpuContext};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

use crate::input::ClickTracker;
use crate::input_trace::input_trace;

/// `input_trace!`'s redacted rendering of a winit `Ime` event — variant
/// name plus (for `Preedit`/`Commit`) `input_trace::describe_text`'s
/// first-char+length summary, never the composed/committed text itself.
fn describe_ime(ime: &winit::event::Ime) -> String {
    match ime {
        winit::event::Ime::Enabled => "Enabled".to_string(),
        winit::event::Ime::Disabled => "Disabled".to_string(),
        winit::event::Ime::Preedit(text, _cursor_range) => {
            format!("Preedit({})", crate::input_trace::describe_text(text))
        }
        winit::event::Ime::Commit(text) => {
            format!("Commit({})", crate::input_trace::describe_text(text))
        }
    }
}

/// The pure decision behind
/// [`WinitWindowInner::maybe_feed_unhandled_key_as_text`] — see that
/// method's doc for the *why*. Kept as a free function (rather than inline
/// in the method) so it's testable without a real `WinitWindowInner`
/// (`RefCell`-backed state, a live `PlatformInputHandler`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextFallbackDecision {
    /// Nothing else handled this key, no composition is in progress, and
    /// it carries text — the platform layer is this keystroke's only
    /// remaining chance to reach the active input handler.
    Feed,
    /// A downstream listener (an `on_key_down` handler, a bound action,
    /// gpui's own dispatch, ...) already stopped propagation — mirrors
    /// gpui_linux's `if !result.propagate { return }`.
    SkipHandled,
    /// An IME composition is in progress (a non-empty `Preedit` arrived
    /// with no `Commit`/`Disabled` since) — the composed result arrives
    /// via `Ime::Commit` on its own; feeding the raw romaji/kana keystrokes
    /// here too would insert them as literal text alongside it.
    SkipComposing,
    /// Anything beyond a plain keystroke or Shift (Ctrl/Alt/Cmd combos,
    /// function keys held with a modifier) is not text input — mirrors
    /// gpui_linux's `modifiers.is_subset_of(&Modifiers::shift())` gate on
    /// both its wayland and x11 backends.
    SkipModifiers,
    /// The keystroke carries no text (arrows, Tab, Escape, a bare
    /// modifier, ...) — nothing to feed.
    SkipNoText,
}

fn text_fallback_decision(
    propagate: bool,
    ime_composing: bool,
    modifiers: Modifiers,
    key_char: Option<&str>,
) -> TextFallbackDecision {
    if !propagate {
        TextFallbackDecision::SkipHandled
    } else if ime_composing {
        TextFallbackDecision::SkipComposing
    } else if !modifiers.is_subset_of(&Modifiers::shift()) {
        TextFallbackDecision::SkipModifiers
    } else if key_char.is_none() {
        TextFallbackDecision::SkipNoText
    } else {
        TextFallbackDecision::Feed
    }
}

// `PlatformWindow`'s callback setters take these exact closure shapes
// (mirroring gpui_web/gpui_linux's own window callback structs, which have
// the same complexity); factoring each into a named `type` would only
// indirect through the trait's own signatures without reducing anything.
#[derive(Default)]
#[allow(clippy::type_complexity)]
pub(crate) struct WinitWindowCallbacks {
    pub(crate) request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    pub(crate) input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    pub(crate) resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    pub(crate) active_status_change: Option<Box<dyn FnMut(bool)>>,
    pub(crate) should_close: Option<Box<dyn FnMut() -> bool>>,
    pub(crate) close: Option<Box<dyn FnOnce()>>,
    pub(crate) appearance_changed: Option<Box<dyn FnMut()>>,
}

pub(crate) struct WinitWindowState {
    pub(crate) renderer: WgpuRenderer,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) title: String,
    pub(crate) input_handler: Option<PlatformInputHandler>,
    pub(crate) is_active: bool,
    pub(crate) modifiers: Modifiers,
    pub(crate) mouse_position: Point<Pixels>,
    /// `Some` between a `MouseDown` and its matching `MouseUp` — winit
    /// reports button state per-event, not "which buttons are currently
    /// held", but gpui's `MouseMoveEvent`/`MouseExitEvent` want the latter
    /// (drag detection). Distinct from `click_tracker`, which counts
    /// clicks-in-a-row and is never cleared on release.
    pub(crate) pressed_button: Option<MouseButton>,
    pub(crate) click_tracker: ClickTracker,
    /// True from a non-empty `Ime::Preedit` until the matching
    /// `Ime::Commit`/empty-`Preedit`/`Disabled` — mirrors gpui_linux's
    /// wayland client's own `composing: bool` (`WaylandClientStatePtr`'s
    /// `Dispatch` impl in the pinned checkout). Gates
    /// `maybe_feed_unhandled_key_as_text`: the romaji/kana keys a user
    /// presses *while* composing must never be independently fed to the
    /// input handler as literal text — the composed result already
    /// arrives via `Ime::Commit` when composition ends.
    pub(crate) ime_composing: bool,
}

/// Shared with the winit `ApplicationHandler`, which drives `state` and
/// `callbacks` from `WindowEvent`s it receives for this window's id.
pub(crate) struct WinitWindowInner {
    pub(crate) window: Arc<winit::window::Window>,
    pub(crate) state: RefCell<WinitWindowState>,
    pub(crate) callbacks: RefCell<WinitWindowCallbacks>,
    /// Coalesces redraw requests into "is a repaint owed at all", set by
    /// any `WindowEvent` handler that could have made something dirty
    /// (input, resize, focus, IME) and by `WinitAppHandler::user_event`'s
    /// `Wake` case (main-thread gpui work — animation timers, a
    /// background thread's `cx.notify()` — both route through
    /// `WinitDispatcher::dispatch_on_main_thread`/`dispatch_after`, which
    /// wake the loop the same way). Consumed exactly once per event-loop
    /// iteration by `about_to_wait`, the only place that actually calls
    /// `winit::window::Window::request_redraw` after the bootstrap frame in
    /// `resumed` — see docs/winit-backend-design.md's "idle CPU" section
    /// for why `RedrawRequested` no longer re-arms itself unconditionally.
    pub(crate) needs_redraw: Cell<bool>,
}

impl WinitWindowInner {
    /// Marks this window as owing a repaint on the next event-loop
    /// iteration — see the field doc on [`WinitWindowInner::needs_redraw`].
    pub(crate) fn mark_needs_redraw(&self) {
        self.needs_redraw.set(true);
    }

    /// Drives a winit `Ime` event into gpui's `EntityInputHandler` pipeline
    /// through the same calls gpui_linux's wayland backend makes from
    /// `zwp_text_input_v3::Event` (`replace_and_mark_text_in_range` for
    /// preedit, `replace_text_in_range` for commit) — see module docs.
    /// `unmark_text` is *not* called from a `Commit`, on either backend:
    /// gpui_linux's own `ImeInput::InsertText` handler
    /// (`window.rs::handle_ime` in the pinned checkout) is exactly
    /// `replace_text_in_range(None, &text)`, nothing else — see the
    /// `Ime::Preedit` arm below for why unmarking there (rather than
    /// letting `Commit`'s own `replace_text_in_range` consume the marked
    /// range) causes a double-insert.
    ///
    /// Candidate-window positioning (`set_ime_cursor_area`) only fires for
    /// a *non-empty* `Preedit` — i.e. only while a composition is genuinely
    /// in progress. An earlier version (in the spike) called it
    /// unconditionally (including from empty `Preedit("", None)` acks) and
    /// hit a feedback loop: GNOME's text-input-v3 implementation answers
    /// `set_cursor_rectangle` + `commit()` with another `Done` (surfaced by
    /// winit as another empty `Preedit`), which triggered another
    /// `set_ime_cursor_area`, forever — tens of thousands of events/sec,
    /// observed directly in the spike
    /// (docs/research/winit-backend-spike.md §15). gpui_linux avoids the
    /// same trap with a `serial_tracker` check before its own `commit()`;
    /// winit doesn't expose wayland serials through its `Ime` enum, so
    /// "only reposition on real composition content" is the available
    /// equivalent guard here.
    pub(crate) fn handle_ime(&self, ime: winit::event::Ime) {
        input_trace!("winit Ime {}", describe_ime(&ime));
        let mut state = self.state.borrow_mut();
        let Some(mut input_handler) = state.input_handler.take() else {
            // No logger is initialized in production, so `log::warn!` is
            // normally silent here — if this is where events die, the
            // trace line is the only way to see it.
            log::warn!("Ime event with no active input handler: {ime:?}");
            input_trace!("winit Ime dropped: no active input handler");
            return;
        };
        drop(state);

        let mut reposition_candidate_window = false;
        match ime {
            winit::event::Ime::Enabled => {}
            winit::event::Ime::Preedit(text, _cursor_range) => {
                // winit's `Preedit` cursor_range (byte offsets into `text`)
                // is richer than what gpui_linux actually reads off
                // `zwp_text_input_v3::Event::PreeditString` — that handler
                // destructures `{ text, .. }` and drops the protocol's own
                // cursor_begin/cursor_end fields. We match that omission
                // here (`replace_and_mark_text_in_range`'s
                // `new_selected_range` goes to `None`, same as gpui_linux)
                // rather than inventing a richer contract gpui's Linux
                // backend doesn't itself provide.
                self.state.borrow_mut().ime_composing = !text.is_empty();
                if text.is_empty() {
                    // Deliberately does *not* call `unmark_text()` here.
                    // winit consistently emits an empty `Preedit`
                    // immediately before the `Commit` that finalizes a
                    // composition (documented and reproduced directly in
                    // docs/research/winit-backend-spike.md §16 Q2: "the
                    // order Preedit("", None) -> Commit(text) was
                    // consistent" across every observed log). Unmarking
                    // here would clear the active input handler's marked
                    // range before `Commit`'s own `replace_text_in_range`
                    // gets a chance to use it as *its* replacement target.
                    // Both `EntityInputHandler` implementations this
                    // crate drives (this crate's own terminal, and
                    // gpui-component's `InputState` behind the agent
                    // composer — verified directly against the pinned
                    // checkout's `crates/ui/src/input/state.rs`) resolve a
                    // `None` range to the *current marked range* if one is
                    // set, falling back to the plain text-cursor position
                    // only when nothing is marked. If we unmark first,
                    // `Commit`'s `replace_text_in_range(None, text)` falls
                    // through to that plain-cursor fallback instead — which
                    // sits right *after* the already-inserted preedit
                    // content (gpui-component's `unmark_text` clears the
                    // marked-range bookkeeping but never touches the
                    // buffer text it already inserted during `Preedit`),
                    // so the same text gets inserted a second time right
                    // next to the first. `Commit`'s own
                    // `replace_text_in_range` already clears the marked
                    // range as part of doing the replacement in both
                    // implementations, so there's no leak on the normal
                    // compose -> commit path. `Ime::Disabled` below still
                    // explicitly unmarks for "IME turned off mid-
                    // composition"; a composition cancelled with no commit
                    // and no `Disabled` (e.g. Escape, depending on the
                    // IME) can leave a stale marked range until the next
                    // real edit, which naturally overwrites/consumes it
                    // via the same None-range-targets-marked-range
                    // convention — narrow, self-healing visual staleness,
                    // clearly preferable to silently duplicating committed
                    // text.
                    input_trace!(
                        "winit Ime empty Preedit: not unmarking (left for Commit/Disabled to resolve)"
                    );
                } else {
                    input_handler.replace_and_mark_text_in_range(None, &text, None);
                    reposition_candidate_window = true;
                }
            }
            winit::event::Ime::Commit(text) => {
                self.state.borrow_mut().ime_composing = false;
                input_handler.replace_text_in_range(None, &text);
            }
            winit::event::Ime::Disabled => {
                self.state.borrow_mut().ime_composing = false;
                if let Some(marked) = input_handler.marked_text_range() {
                    input_handler.replace_text_in_range(Some(marked), "");
                }
                input_handler.unmark_text();
            }
        }

        if reposition_candidate_window {
            if let Some(bounds) = input_handler.ime_candidate_bounds() {
                self.set_ime_cursor_area(bounds);
            }
        }

        self.state.borrow_mut().input_handler = Some(input_handler);
        self.mark_needs_redraw();
    }

    /// Mirrors gpui_linux's own text-input fallback — wayland's
    /// `WaylandWindowState::handle_input` and x11's `X11WindowState::handle_input`
    /// in the pinned checkout both do this same thing after dispatching a
    /// `PlatformInput::KeyDown`: if nothing downstream consumed it
    /// (`propagate` still true) and the keystroke carries text, feed that
    /// text to the active input handler directly, since no separate IME
    /// event is coming for it. `horizon-winit-platform` had no equivalent —
    /// a plain printable key that nothing else handles (the common case
    /// outside kitty "report all keys" mode, see `src/terminal/mod.rs`'s
    /// module doc) was silently dropped instead of ever reaching the
    /// terminal. See docs/winit-backend-design.md's "Resolved incidents" ("Keyboard input pipeline", Stage 3)
    /// section for how this was diagnosed (a permanent `input-trace:` trace
    /// facility, driven by the owner's own daily-driver capture) and why
    /// composition-mode input already worked without it (a real
    /// `Ime::Commit` covers that path; `ime_composing` here is what keeps
    /// this fallback from also firing on the raw keys typed *while*
    /// composing).
    ///
    /// `propagate`: the `DispatchEventResult` from dispatching this
    /// `KeyDown` through gpui's own callback — `true` means nothing
    /// downstream (an `on_key_down` listener, a bound action, ...) stopped
    /// propagation, matching gpui_linux's exact gate. Only ever called for
    /// `ElementState::Pressed` — gpui_linux's own fallback only looks at
    /// `PlatformInput::KeyDown`, never `KeyUp`.
    pub(crate) fn maybe_feed_unhandled_key_as_text(
        &self,
        keystroke: &gpui::Keystroke,
        propagate: bool,
    ) {
        let ime_composing = self.state.borrow().ime_composing;
        let decision = text_fallback_decision(
            propagate,
            ime_composing,
            keystroke.modifiers,
            keystroke.key_char.as_deref(),
        );
        let TextFallbackDecision::Feed = decision else {
            input_trace!("winit text-fallback skip: {decision:?}");
            return;
        };
        // `Feed` only reaches here when `key_char` is `Some` (see
        // `text_fallback_decision`).
        let key_char = keystroke.key_char.as_deref().unwrap();
        let mut state = self.state.borrow_mut();
        let Some(mut input_handler) = state.input_handler.take() else {
            input_trace!("winit text-fallback skip: no active input handler");
            return;
        };
        drop(state);
        input_trace!(
            "winit text-fallback fire: feeding {} to input handler",
            crate::input_trace::describe_text(key_char)
        );
        input_handler.replace_text_in_range(None, key_char);
        self.state.borrow_mut().input_handler = Some(input_handler);
        self.mark_needs_redraw();
    }

    /// Shared by `handle_ime` (while composing) and
    /// `WinitPlatformWindow::update_ime_position` (the out-of-composition
    /// caret-moved hook gpui calls via `Window::invalidate_character_coordinates`).
    pub(crate) fn set_ime_cursor_area(&self, bounds: Bounds<Pixels>) {
        self.window.set_ime_cursor_area(
            winit::dpi::LogicalPosition::new(
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
            ),
            winit::dpi::LogicalSize::new(
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ),
        );
    }

    pub(crate) fn set_cursor_icon(&self, icon: winit::window::CursorIcon) {
        self.window.set_cursor(icon);
    }
}

pub(crate) struct WinitPlatformWindow {
    pub(crate) inner: Rc<WinitWindowInner>,
    display: Rc<dyn PlatformDisplay>,
    #[allow(dead_code)]
    handle: AnyWindowHandle,
}

impl WinitPlatformWindow {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        params: WindowParams,
        window: winit::window::Window,
        gpu_context: GpuContext,
        display: Rc<dyn PlatformDisplay>,
    ) -> anyhow::Result<Self> {
        let window = Arc::new(window);
        let physical_size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;

        // On Wayland, GNOME/Mutter refuses server-side xdg decorations, so
        // winit falls back to its bundled sctk-adwaita client-side frame —
        // see docs/research/winit-backend-spike.md §2 for the decoration
        // evidence this log line was originally used to gather.
        log::debug!(
            "window decoration: is_decorated={} inner_size={:?} outer_size={:?}",
            window.is_decorated(),
            window.inner_size(),
            window.outer_size(),
        );

        // Opt the window into IME. Without this winit never asks the
        // platform's text-input mechanism (zwp_text_input_v3 on Wayland,
        // XIM on X11) to attach to this surface, so no `WindowEvent::Ime`
        // ever arrives — and, per `src/terminal/mod.rs`'s design (plain
        // printable text on Linux arrives only through the
        // EntityInputHandler pipeline, matching gpui_linux), typing
        // wouldn't work at all, not just IME composition.
        window.set_ime_allowed(true);

        let renderer_config = WgpuSurfaceConfig {
            size: Size {
                width: DevicePixels(physical_size.width as i32),
                height: DevicePixels(physical_size.height as i32),
            },
            transparent: false,
            preferred_present_mode: None,
        };

        // gpui_wgpu's own lazy first-window path (`WgpuRenderer::new` with
        // an empty `GpuContext` cell) builds its instance via
        // `WgpuContext::instance`, which hardcodes VULKAN|GL — fine on the
        // OSes zed itself exercises that crate on, but an empty backend set
        // on macOS, so surface creation fails before adapter selection even
        // runs. Seed the shared cell from a Metal instance here (using a
        // temporary surface for adapter selection); the renderer then
        // reuses it (and its instance) and never reaches the hardcoded
        // fallback. macOS-gated so Linux/Windows keep the upstream
        // first-window path untouched.
        #[cfg(target_os = "macos")]
        if gpu_context.borrow().is_none() {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::METAL,
                flags: wgpu::InstanceFlags::default(),
                backend_options: wgpu::BackendOptions::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: Some(Box::new(Arc::clone(&window))),
            });
            let surface = instance.create_surface(Arc::clone(&window))?;
            let context = WgpuContext::new(instance, &surface, None)?;
            gpu_context.borrow_mut().replace(context);
        }
        let renderer = WgpuRenderer::new(gpu_context, &window, renderer_config, None)?;

        let logical_size = physical_size.to_logical::<f32>(scale_factor as f64);
        let bounds = Bounds {
            origin: params.bounds.origin,
            size: Size {
                width: px(logical_size.width),
                height: px(logical_size.height),
            },
        };

        let state = WinitWindowState {
            renderer,
            bounds,
            scale_factor,
            title: String::new(),
            input_handler: None,
            is_active: true,
            modifiers: Modifiers::default(),
            mouse_position: Point::default(),
            pressed_button: None,
            click_tracker: ClickTracker::new(),
            ime_composing: false,
        };

        let inner = Rc::new(WinitWindowInner {
            window,
            state: RefCell::new(state),
            callbacks: RefCell::new(WinitWindowCallbacks::default()),
            needs_redraw: Cell::new(false),
        });

        Ok(Self {
            inner,
            display,
            handle,
        })
    }
}

impl raw_window_handle::HasWindowHandle for WinitPlatformWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.inner.window.window_handle()
    }
}

impl raw_window_handle::HasDisplayHandle for WinitPlatformWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.inner.window.display_handle()
    }
}

impl gpui::PlatformWindow for WinitPlatformWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.inner.state.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        self.inner.window.is_maximized()
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Windowed(self.bounds())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.inner.state.borrow().bounds.size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let scale_factor = self.inner.state.borrow().scale_factor as f64;
        let _ = self.inner.window.request_inner_size(
            winit::dpi::LogicalSize::new(f32::from(size.width), f32::from(size.height))
                .to_physical::<u32>(scale_factor),
        );
    }

    fn scale_factor(&self) -> f32 {
        self.inner.state.borrow().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        // winit exposes no light/dark query on Linux; gpui_linux itself
        // reads the freedesktop portal setting instead of asking winit's
        // equivalent (which doesn't exist here). Horizon's own theme is
        // entirely config-driven and never reads `WindowAppearance`
        // (grepped: no caller in src/), so this stub has no observable
        // effect today — documented default per the task brief's item 2e.
        WindowAppearance::Dark
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.display.clone())
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.inner.state.borrow().mouse_position
    }

    fn modifiers(&self) -> Modifiers {
        self.inner.state.borrow().modifiers
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.inner.state.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.inner.state.borrow_mut().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        self.inner.window.focus_window();
    }

    fn is_active(&self) -> bool {
        self.inner.state.borrow().is_active
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        self.inner.state.borrow_mut().title = title.to_owned();
        self.inner.window.set_title(title);
    }

    fn set_background_appearance(&self, _background: WindowBackgroundAppearance) {}

    fn minimize(&self) {
        self.inner.window.set_minimized(true);
    }

    fn zoom(&self) {
        let maximized = self.inner.window.is_maximized();
        self.inner.window.set_maximized(!maximized);
    }

    fn toggle_fullscreen(&self) {
        let fullscreen = self.inner.window.fullscreen();
        self.inner.window.set_fullscreen(if fullscreen.is_some() {
            None
        } else {
            Some(winit::window::Fullscreen::Borderless(None))
        });
    }

    fn is_fullscreen(&self) -> bool {
        self.inner.window.fullscreen().is_some()
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.inner.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.inner.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.inner.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, _callback: Box<dyn FnMut(bool)>) {}

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.inner.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, _callback: Box<dyn FnMut()>) {}

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.inner.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.inner.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.inner.state.borrow_mut().renderer.draw(scene);
    }

    /// Deliberately does *not* forward to `winit::Window::pre_present_notify`
    /// — on Wayland, that call arms a `wl_surface.frame` request that only
    /// takes effect on the *next* `wl_surface.commit`, and gpui always calls
    /// `completed_frame()` strictly after the commit that
    /// `PlatformWindow::draw` just performed (`WgpuRenderer::draw`'s
    /// internal `frame.present()`), not before it — the ordering
    /// `pre_present_notify`'s own contract requires. Calling it here sends
    /// an orphaned frame-callback request the compositor never associates
    /// with a commit, which permanently wedges winit's Wayland backend (it
    /// withholds every future `WindowEvent::RedrawRequested` until that
    /// request's callback fires, and it never will). Pacing is already
    /// covered without this: `WgpuSurfaceConfig::preferred_present_mode` is
    /// `None`, so `gpui_wgpu` configures the surface Fifo, and
    /// `get_current_texture`/`present` (inside `draw`, above) block for
    /// real vsync pacing while focused; the inactive-window ~30fps cap is
    /// gpui's own wall-clock throttle (`min_frame_interval` in
    /// `Window::on_request_frame`'s closure), independent of any platform
    /// hook. See docs/winit-backend-design.md's "Resolved incidents" ("Configure stall") section.
    fn completed_frame(&self) {}

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.inner.state.borrow().renderer.sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        self.inner
            .state
            .borrow()
            .renderer
            .supports_dual_source_blending()
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        Some(self.inner.state.borrow().renderer.gpu_specs())
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        // Called by gpui's `Window::invalidate_character_coordinates` when
        // the app thinks the caret moved outside of an IME composition
        // (e.g. after a plain keystroke) — mirrors gpui_linux's
        // `WaylandClient::update_ime_position`, which likewise just
        // re-issues `set_cursor_rectangle` so the *next* composition starts
        // at the right spot.
        self.inner.set_ime_cursor_area(bounds);
    }

    fn request_decorations(&self, _decorations: WindowDecorations) {}

    fn show_window_menu(&self, _position: Point<Pixels>) {}

    fn start_window_move(&self) {
        let _ = self.inner.window.drag_window();
    }

    fn start_window_resize(&self, _edge: ResizeEdge) {}

    fn window_decorations(&self) -> Decorations {
        // sctk-adwaita CSD on Wayland, native chrome on X11/other backends —
        // both are winit/compositor-owned, so from gpui's point of view
        // the server (or winit acting on the client's behalf) always owns
        // decorations here.
        Decorations::Server
    }

    fn set_app_id(&mut self, _app_id: &str) {}

    fn window_controls(&self) -> WindowControls {
        WindowControls {
            fullscreen: true,
            maximize: true,
            minimize: true,
            window_menu: false,
        }
    }

    fn set_client_inset(&self, _inset: Pixels) {}
}

#[cfg(test)]
mod tests {
    //! Drives `text_fallback_decision` directly — the pure seam behind
    //! `maybe_feed_unhandled_key_as_text`, extracted specifically so this
    //! doesn't need a live `WinitWindowInner`/`PlatformInputHandler`. See
    //! that function's doc for the mirrored gpui_linux behavior each case
    //! below is guarding.

    use super::{text_fallback_decision, TextFallbackDecision};
    use gpui::Modifiers;

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn unhandled_plain_key_with_text_is_fed() {
        // The direct-ASCII-mode bug this exists to fix: a plain printable
        // key nothing else consumed, not composing, must reach the
        // terminal as text.
        assert_eq!(
            text_fallback_decision(true, false, plain(), Some("a")),
            TextFallbackDecision::Feed
        );
    }

    #[test]
    fn shift_only_modifier_still_feeds() {
        // Shift is part of ordinary typing (capital letters, punctuation)
        // -- gpui_linux's own gate allows it too.
        assert_eq!(
            text_fallback_decision(true, false, Modifiers::shift(), Some("A")),
            TextFallbackDecision::Feed
        );
    }

    #[test]
    fn already_handled_key_is_skipped() {
        // Watch case 1 (kitty mode): handle_key already sent the key via
        // TerminalCommand::Key. If gpui reports it handled (propagate ==
        // false), the fallback must not also fire -- KeyTextDedup is only
        // the second line of defense for when propagate stays true anyway.
        assert_eq!(
            text_fallback_decision(false, false, plain(), Some("a")),
            TextFallbackDecision::SkipHandled
        );
    }

    #[test]
    fn composing_key_is_never_fed_even_if_unhandled() {
        // Watch case 2: the romaji/kana keys typed *while* an IME
        // composition is in progress must never be independently inserted
        // as literal text -- the composed result arrives via its own
        // Ime::Commit. This takes priority over every other condition.
        assert_eq!(
            text_fallback_decision(true, true, plain(), Some("k")),
            TextFallbackDecision::SkipComposing
        );
    }

    #[test]
    fn composing_gate_wins_even_when_also_unhandled_and_has_text() {
        // Same as above, phrased as a priority check: composing wins over
        // an otherwise-Feed-eligible combination.
        assert_eq!(
            text_fallback_decision(true, true, Modifiers::shift(), Some("K")),
            TextFallbackDecision::SkipComposing
        );
    }

    #[test]
    fn control_modifier_is_skipped() {
        assert_eq!(
            text_fallback_decision(
                true,
                false,
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
                Some("c")
            ),
            TextFallbackDecision::SkipModifiers
        );
    }

    #[test]
    fn alt_modifier_is_skipped() {
        assert_eq!(
            text_fallback_decision(
                true,
                false,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                Some("a")
            ),
            TextFallbackDecision::SkipModifiers
        );
    }

    #[test]
    fn platform_modifier_is_skipped() {
        assert_eq!(
            text_fallback_decision(
                true,
                false,
                Modifiers {
                    platform: true,
                    ..Modifiers::default()
                },
                Some("v")
            ),
            TextFallbackDecision::SkipModifiers
        );
    }

    #[test]
    fn no_key_char_is_skipped() {
        // Watch case 3: named keys (Tab, arrows, Enter, ...) carry no
        // key_char, so a bound List/keybinding action never risks a text
        // fallback regardless of whether it stopped propagation.
        assert_eq!(
            text_fallback_decision(true, false, plain(), None),
            TextFallbackDecision::SkipNoText
        );
    }
}
