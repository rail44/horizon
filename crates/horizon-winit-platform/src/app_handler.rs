//! The winit `ApplicationHandler`: the actual event-loop pump. `Platform::run`
//! hands control to `EventLoop::run_app` with one of these, and every
//! callback below re-enters gpui via the callbacks `WinitPlatformWindow`
//! stashed (`on_request_frame`, `on_input`, `on_resize`, ...), wrapped in an
//! `ActiveLoopGuard` so `Platform::open_window` can reach the
//! `ActiveEventLoop` if gpui decides to open a window mid-callback.
//!
//! Redraw scheduling is event-driven, not a free-running loop: a per-window
//! `WinitWindowInner::needs_redraw` flag is set by any `WindowEvent` that
//! could need a repaint (input, resize, focus, IME) and by `user_event`'s
//! `Wake` case (covers gpui main-thread work with no `WindowEvent` of its
//! own — animation timers, a background thread's `cx.notify()`), then
//! consumed exactly once per iteration by `about_to_wait`, which is the
//! only place that calls `winit::window::Window::request_redraw` after the
//! bootstrap frame in `resumed`. `RedrawRequested` itself never re-requests
//! itself — an earlier version did (mirroring gpui_web's
//! requestAnimationFrame loop, see docs/winit-backend-design.md's "idle
//! CPU" section for why that ran the GPU present path at full display
//! refresh rate even fully idle). gpui's own `on_request_frame` closure
//! (registered in `Window::new`, see gpui/src/window.rs) still throttles
//! frame *pacing* down to ~30fps when unfocused and relies on the
//! surface's Fifo present mode for vsync while focused; that's orthogonal
//! to the *whether-to-redraw-at-all* coalescing this module now owns.

use std::rc::Rc;

use gpui::{
    DispatchEventResult, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollWheelEvent,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::active_loop::ActiveLoopGuard;
use crate::input::{
    winit_key_event_to_keystroke, winit_modifiers_to_gpui, winit_mouse_button_to_gpui,
    winit_scroll_delta_to_gpui, winit_touch_phase_to_gpui,
};
use crate::input_trace::input_trace;
use crate::platform::WinitPlatform;
use crate::window::WinitWindowInner;

#[derive(Debug, Clone)]
pub(crate) enum WinitUserEvent {
    /// Wakes the event loop so `WinitDispatcher::drain_main_queue` runs;
    /// carries no payload; the queue itself is the payload.
    Wake,
    /// A muda menu-item click, forwarded from `muda::MenuEvent`'s global
    /// handler (registered once in `WinitPlatform::new`) — see
    /// `macos_menu.rs` for why macOS routes menu clicks through here
    /// rather than handling them synchronously off the event loop.
    #[cfg(target_os = "macos")]
    MenuEvent(muda::MenuEvent),
}

/// A per-second snapshot of how many `WindowEvent::RedrawRequested` cycles
/// actually ran, traced via the existing `HORIZON_INPUT_TRACE` sink
/// (`frame-loop: ...` lines). Grew out of the idle-CPU investigation (see
/// docs/winit-backend-design.md's "idle CPU" section) as the instrument
/// that first showed the redraw loop running at a flat, unthrottled 60fps
/// even fully idle; kept as a permanent health signal for the same
/// regression class — a genuinely idle window should show ~0
/// `redraw_requested_per_sec` with `total` barely moving, matching
/// `input_trace`'s own "permanent diagnostic, not temp code" precedent.
struct FrameLoopStats {
    window_start: std::time::Instant,
    window_count: u64,
    total_count: u64,
    process_start: std::time::Instant,
}

impl FrameLoopStats {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            window_start: now,
            window_count: 0,
            total_count: 0,
            process_start: now,
        }
    }

    fn record_redraw_requested(&mut self) {
        if let Some(line) = self.record_redraw_requested_at(std::time::Instant::now()) {
            input_trace!("{line}");
        }
    }

    /// Clock-injected core so the decision stays pure and testable without
    /// sleeping (mirrors `ImeCommitGuard::should_suppress_at`/
    /// `text_fallback_decision`'s pattern in `src/terminal/mod.rs`/
    /// `window.rs`). Returns the trace line iff a full second-plus has
    /// elapsed since the last snapshot; `None` otherwise (the common case —
    /// most calls just tally into the running window).
    fn record_redraw_requested_at(&mut self, now: std::time::Instant) -> Option<String> {
        self.window_count += 1;
        self.total_count += 1;
        let elapsed = now.duration_since(self.window_start);
        if elapsed < std::time::Duration::from_secs(1) {
            return None;
        }
        let fps = self.window_count as f64 / elapsed.as_secs_f64();
        let line = format!(
            "frame-loop: redraw_requested_per_sec={:.1} total={} since_start={:.1}s",
            fps,
            self.total_count,
            now.duration_since(self.process_start).as_secs_f64()
        );
        self.window_count = 0;
        self.window_start = now;
        Some(line)
    }
}

pub(crate) struct WinitAppHandler<'a> {
    platform: &'a WinitPlatform,
    on_finish_launching: Option<Box<dyn FnOnce()>>,
    frame_loop_stats: FrameLoopStats,
}

impl<'a> WinitAppHandler<'a> {
    pub(crate) fn new(platform: &'a WinitPlatform, on_finish_launching: Box<dyn FnOnce()>) -> Self {
        Self {
            platform,
            on_finish_launching: Some(on_finish_launching),
            frame_loop_stats: FrameLoopStats::new(),
        }
    }

    fn window_by_id(&self, window_id: WindowId) -> Option<Rc<WinitWindowInner>> {
        self.platform
            .windows
            .borrow()
            .iter()
            .find(|inner| inner.window.id() == window_id)
            .cloned()
    }
}

impl<'a> ApplicationHandler<WinitUserEvent> for WinitAppHandler<'a> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _guard = ActiveLoopGuard::enter(event_loop);
        self.platform.dispatcher.drain_main_queue();

        if let Some(on_finish_launching) = self.on_finish_launching.take() {
            // Runs gpui's startup closure synchronously; any `cx.open_window`
            // call inside it reaches `Platform::open_window`, which reads
            // back the `ActiveEventLoop` we just stashed above.
            on_finish_launching();
        }

        // Kick off the redraw loop for whatever window(s) that startup
        // closure opened.
        for inner in self.platform.windows.borrow().iter() {
            inner.window.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WinitUserEvent) {
        let _guard = ActiveLoopGuard::enter(event_loop);
        match event {
            WinitUserEvent::Wake => {
                // `WinitDispatcher::dispatch_on_main_thread`/`dispatch_after`
                // send this for any gpui main-thread work -- an animation
                // timer tick, a background thread's `cx.notify()` reaching
                // the main thread via `Entity::update`, etc. None of that
                // is visible to us as a `WindowEvent`, so (conservatively;
                // we can't tell which window, if any, it was for) mark
                // every window as owing a redraw rather than risk a
                // notify-driven update sitting unpainted until some
                // unrelated `WindowEvent` happens to nudge the loop.
                for inner in self.platform.windows.borrow().iter() {
                    inner.mark_needs_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            WinitUserEvent::MenuEvent(menu_event) => {
                self.platform.dispatch_menu_action(&menu_event.id);
            }
        }
        self.platform.dispatcher.drain_main_queue();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let _guard = ActiveLoopGuard::enter(event_loop);

        let Some(inner) = self.window_by_id(window_id) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                let should_close = inner
                    .callbacks
                    .borrow_mut()
                    .should_close
                    .as_mut()
                    .map(|f| f())
                    .unwrap_or(true);
                if should_close {
                    if let Some(close) = inner.callbacks.borrow_mut().close.take() {
                        close();
                    }
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(physical_size) => {
                let scale_factor = inner.state.borrow().scale_factor as f64;
                let logical = physical_size.to_logical::<f32>(scale_factor);
                let new_size = gpui::Size {
                    width: gpui::px(logical.width),
                    height: gpui::px(logical.height),
                };
                inner.state.borrow_mut().bounds.size = new_size;
                inner
                    .state
                    .borrow_mut()
                    .renderer
                    .update_drawable_size(gpui::Size {
                        width: gpui::DevicePixels(physical_size.width as i32),
                        height: gpui::DevicePixels(physical_size.height as i32),
                    });
                if let Some(resize) = inner.callbacks.borrow_mut().resize.as_mut() {
                    resize(new_size, scale_factor as f32);
                }
                inner.mark_needs_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // winit fires this ahead of any `Resized` the OS also sends
                // for the same DPI change; recompute logical bounds from
                // the window's *current* physical size immediately so
                // `PlatformWindow::scale_factor`/`content_size` are
                // consistent even if no `Resized` follows (e.g. moving the
                // window to a different-DPI monitor without resizing it in
                // physical pixels — gpui_linux's wayland backend handles
                // preferred_buffer_scale similarly by re-deriving logical
                // size on every scale update rather than waiting on a
                // separate resize signal).
                let physical_size = inner.window.inner_size();
                let logical = physical_size.to_logical::<f32>(scale_factor);
                let new_size = gpui::Size {
                    width: gpui::px(logical.width),
                    height: gpui::px(logical.height),
                };
                {
                    let mut state = inner.state.borrow_mut();
                    state.scale_factor = scale_factor as f32;
                    state.bounds.size = new_size;
                }
                if let Some(resize) = inner.callbacks.borrow_mut().resize.as_mut() {
                    resize(new_size, scale_factor as f32);
                }
                inner.mark_needs_redraw();
            }
            WindowEvent::Focused(is_focused) => {
                inner.state.borrow_mut().is_active = is_focused;
                if let Some(callback) = inner.callbacks.borrow_mut().active_status_change.as_mut() {
                    callback(is_focused);
                }
                inner.mark_needs_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let modifiers = winit_modifiers_to_gpui(modifiers.state());
                inner.state.borrow_mut().modifiers = modifiers;
                dispatch_input(
                    &inner,
                    PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                        modifiers,
                        capslock: gpui::Capslock::default(),
                    }),
                );
                inner.mark_needs_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                input_trace!(
                    "winit KeyboardInput physical_key={:?} state={:?} repeat={}",
                    event.physical_key,
                    event.state,
                    event.repeat
                );
                let modifiers = inner.state.borrow().modifiers;
                if let Some(keystroke) = winit_key_event_to_keystroke(&event, modifiers) {
                    let pressed = event.state == ElementState::Pressed;
                    let input = match event.state {
                        ElementState::Pressed => PlatformInput::KeyDown(KeyDownEvent {
                            keystroke: keystroke.clone(),
                            is_held: event.repeat,
                            prefer_character_input: false,
                        }),
                        ElementState::Released => PlatformInput::KeyUp(KeyUpEvent {
                            keystroke: keystroke.clone(),
                        }),
                    };
                    let result = dispatch_input(&inner, input);
                    // Mirrors gpui_linux's own text-input fallback — see
                    // `WinitWindowInner::maybe_feed_unhandled_key_as_text`'s
                    // doc. Only for KeyDown: gpui_linux's own fallback never
                    // looks at KeyUp either.
                    if pressed {
                        inner.maybe_feed_unhandled_key_as_text(&keystroke, result.propagate);
                    }
                    inner.mark_needs_redraw();
                }
            }
            WindowEvent::Ime(ime) => {
                inner.handle_ime(ime);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale_factor = inner.state.borrow().scale_factor as f64;
                let logical = position.to_logical::<f32>(scale_factor);
                let position = gpui::point(gpui::px(logical.x), gpui::px(logical.y));
                let (modifiers, pressed_button) = {
                    let mut state = inner.state.borrow_mut();
                    state.mouse_position = position;
                    (state.modifiers, state.pressed_button)
                };
                dispatch_input(
                    &inner,
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button,
                        modifiers,
                    }),
                );
                // Hover-sensitive UI (button/list highlight, cursor icon)
                // depends on this: unlike the input events below, cursor
                // motion previously relied entirely on the
                // unconditionally-looping `RedrawRequested` to eventually
                // reflect it, which no longer runs while idle.
                inner.mark_needs_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                let (position, modifiers, pressed_button) = {
                    let state = inner.state.borrow();
                    (state.mouse_position, state.modifiers, state.pressed_button)
                };
                dispatch_input(
                    &inner,
                    PlatformInput::MouseExited(MouseExitEvent {
                        position,
                        pressed_button,
                        modifiers,
                    }),
                );
                inner.mark_needs_redraw();
            }
            WindowEvent::CursorEntered { .. } => {}
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(button) = winit_mouse_button_to_gpui(button) else {
                    return;
                };
                let (position, modifiers) = {
                    let state = inner.state.borrow();
                    (state.mouse_position, state.modifiers)
                };
                match state {
                    ElementState::Pressed => {
                        let click_count = {
                            let mut state = inner.state.borrow_mut();
                            state.pressed_button = Some(button);
                            state.click_tracker.register_press(button, position)
                        };
                        dispatch_input(
                            &inner,
                            PlatformInput::MouseDown(MouseDownEvent {
                                button,
                                position,
                                modifiers,
                                click_count,
                                first_mouse: false,
                            }),
                        );
                    }
                    ElementState::Released => {
                        let click_count = {
                            let mut state = inner.state.borrow_mut();
                            state.pressed_button = None;
                            state.click_tracker.current_count()
                        };
                        dispatch_input(
                            &inner,
                            PlatformInput::MouseUp(MouseUpEvent {
                                button,
                                position,
                                modifiers,
                                click_count,
                            }),
                        );
                    }
                }
                inner.mark_needs_redraw();
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let (position, modifiers) = {
                    let state = inner.state.borrow();
                    (state.mouse_position, state.modifiers)
                };
                dispatch_input(
                    &inner,
                    PlatformInput::ScrollWheel(ScrollWheelEvent {
                        position,
                        delta: winit_scroll_delta_to_gpui(delta),
                        modifiers,
                        touch_phase: winit_touch_phase_to_gpui(phase),
                    }),
                );
                inner.mark_needs_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.frame_loop_stats.record_redraw_requested();
                inner.drew_this_cycle.set(false);
                let callback = inner.callbacks.borrow_mut().request_frame.take();
                if let Some(mut callback) = callback {
                    callback(gpui::RequestFrameOptions {
                        // Deliberately *not* forced to `true` (an earlier
                        // version of this handler always did — see
                        // docs/winit-backend-design.md's "idle CPU"
                        // section) — that defeated gpui's own "skip the
                        // GPU work when nothing is dirty" branch inside
                        // this callback (forcing `needs_present` true
                        // regardless of `invalidator.is_dirty()`), *and*
                        // it made `PlatformWindow::draw` fire on every
                        // cycle unconditionally, which would make the
                        // animation-frame re-arm below runaway-loop
                        // forever instead of self-terminating. Letting
                        // gpui's own dirty-tracking decide matches every
                        // other platform backend (gpui_linux, gpui_web)
                        // and makes `drew_this_cycle` a truthful signal —
                        // see docs/winit-backend-design.md's "Animation
                        // frame continuity" section.
                        require_presentation: false,
                        force_render: false,
                    });
                    inner.callbacks.borrow_mut().request_frame = Some(callback);
                }
                // Deliberately does *not* unconditionally re-request a
                // redraw here (see docs/winit-backend-design.md's "idle
                // CPU" section): that used to loop forever, matching
                // gpui_web's rAF pattern. `about_to_wait` owns scheduling
                // the next redraw, consuming `WinitWindowInner::needs_redraw`
                // -- exactly the events below (and any window that already
                // ran) set before this iteration reaches it. The one
                // exception is the animation-frame re-arm: if gpui actually
                // drew this cycle, an in-progress `with_animation` element
                // may have just queued more next-frame work with no other
                // signal visible to this platform crate (see
                // `WinitWindowInner::drew_this_cycle`'s doc) -- so mark this
                // window dirty for exactly one more cycle, self-terminating
                // once a cycle draws nothing.
                if should_rearm_for_next_frame(inner.drew_this_cycle.get()) {
                    input_trace!(
                        "winit animation-frame rearm: drew this cycle, requesting one more redraw"
                    );
                    inner.mark_needs_redraw();
                }
            }
            _ => {}
        }

        self.platform.dispatcher.drain_main_queue();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _guard = ActiveLoopGuard::enter(event_loop);
        self.platform.dispatcher.drain_main_queue();
        // The sole place that turns `needs_redraw` into an actual
        // `winit::window::Window::request_redraw` call — see the field doc
        // on `WinitWindowInner::needs_redraw`. Runs once per event-loop
        // iteration regardless of how many `WindowEvent`s/user events fed
        // into it, so a burst of e.g. several mouse-move events between
        // wakeups still yields exactly one redraw request, not one per
        // event.
        for inner in self.platform.windows.borrow().iter() {
            if inner.needs_redraw.take() {
                inner.window.request_redraw();
            }
        }
    }
}

/// Returns gpui's own `DispatchEventResult` (defaulting to
/// `propagate: true` — "nothing handled it" — when no callback is
/// registered yet, matching gpui_linux's equivalent) so `KeyboardInput`'s
/// handler can decide whether `maybe_feed_unhandled_key_as_text` should
/// run.
fn dispatch_input(inner: &WinitWindowInner, input: PlatformInput) -> DispatchEventResult {
    if let Some(callback) = inner.callbacks.borrow_mut().input.as_mut() {
        callback(input)
    } else {
        DispatchEventResult::default()
    }
}

/// The pure decision behind the animation-frame re-arm: whether the
/// `RedrawRequested` cycle that just finished should be followed by exactly
/// one more, given whether `PlatformWindow::draw` actually ran during it.
/// See `WinitWindowInner::drew_this_cycle`'s doc for why "a draw happened"
/// is the available proxy for "gpui might have just queued more next-frame
/// work" (e.g. an in-progress `with_animation` element), and why chaining
/// on this signal alone is self-terminating rather than a runaway loop —
/// extracted as a free function, mirroring `window.rs`'s
/// `text_fallback_decision`, so the termination property is directly
/// unit-testable without a live window.
fn should_rearm_for_next_frame(drew_this_cycle: bool) -> bool {
    drew_this_cycle
}

#[cfg(test)]
mod tests {
    use super::{should_rearm_for_next_frame, FrameLoopStats};
    use std::time::Duration;

    // `should_rearm_for_next_frame`'s three scenarios from the task brief:
    // pending animation re-arms, an idle cycle doesn't, and a completed
    // animation's chain terminates instead of running away.

    #[test]
    fn idle_cycle_that_drew_nothing_does_not_rearm() {
        // No `with_animation` element (or any other dirty state) means
        // gpui's own `invalidator.is_dirty()` stayed false, so
        // `PlatformWindow::draw` never ran this cycle -- nothing to chase,
        // `about_to_wait` must not be told to schedule another redraw.
        assert!(!should_rearm_for_next_frame(false));
    }

    #[test]
    fn a_cycle_that_drew_rearms_the_next_one() {
        // An in-progress animation (or any other genuinely dirty draw)
        // means gpui may have just queued a `next_frame` callback with no
        // other signal visible to this platform crate -- re-arm once so
        // that callback actually gets a chance to run.
        assert!(should_rearm_for_next_frame(true));
    }

    #[test]
    fn animation_chain_self_terminates_after_one_trailing_empty_cycle() {
        // Simulates a ~200ms with_animation element (e.g. the Segmented
        // tab indicator): three cycles actually draw the in-progress
        // animation (each one's `AnimationElement::request_layout`
        // re-queues gpui's own `next_frame` callback while `!done`), then
        // the animation's final frame (`done == true`) still draws once
        // more without queuing anything further -- so the harmless
        // trailing probe cycle after it draws nothing, and the chain stops
        // on its own. Mirrors gpui_linux's own `wl_surface.frame`
        // arm-before-draw self-termination (see
        // docs/winit-backend-design.md).
        let drew_per_cycle = [true, true, true, false];
        let rearmed: Vec<bool> = drew_per_cycle
            .iter()
            .copied()
            .map(should_rearm_for_next_frame)
            .collect();
        assert_eq!(rearmed, vec![true, true, true, false]);
        // The trailing `false` is what actually stops `about_to_wait` from
        // ever requesting a redraw again for this window -- no runaway
        // loop, regardless of how many frames the animation took.
        assert_eq!(rearmed.last(), Some(&false));
    }

    #[test]
    fn no_snapshot_before_a_second_elapses() {
        let mut stats = FrameLoopStats::new();
        let t0 = stats.window_start;
        // A burst of redraws well inside the first second must not emit
        // anything yet -- just tally.
        assert_eq!(stats.record_redraw_requested_at(t0), None);
        assert_eq!(
            stats.record_redraw_requested_at(t0 + Duration::from_millis(500)),
            None
        );
        assert_eq!(stats.total_count, 2);
    }

    #[test]
    fn snapshot_fires_once_a_second_elapses_and_resets_the_window() {
        let mut stats = FrameLoopStats::new();
        let t0 = stats.window_start;
        stats.record_redraw_requested_at(t0);
        let line = stats
            .record_redraw_requested_at(t0 + Duration::from_secs(1))
            .expect("a full second elapsed, a snapshot line was expected");
        assert!(line.starts_with("frame-loop: redraw_requested_per_sec="));
        assert!(line.contains("total=2"));
        // The window resets: the very next call, even a moment later,
        // shouldn't immediately re-fire.
        assert_eq!(
            stats.record_redraw_requested_at(
                t0 + Duration::from_secs(1) + Duration::from_millis(10)
            ),
            None
        );
    }

    #[test]
    fn fps_reflects_actual_redraw_count_in_the_window() {
        let mut stats = FrameLoopStats::new();
        let t0 = stats.window_start;
        // Five redraws spread across the first second...
        for i in 0..5 {
            stats.record_redraw_requested_at(t0 + Duration::from_millis(i * 100));
        }
        // ...then the sixth crosses the 1s boundary and reports ~6fps for
        // this window (6 redraws / ~1.0s), not some unrelated constant --
        // the whole point of this counter is measuring the *actual* rate,
        // which is what first showed the pre-fix loop pinned at 60fps
        // even fully idle.
        let line = stats
            .record_redraw_requested_at(t0 + Duration::from_secs(1))
            .expect("a full second elapsed");
        assert!(
            line.contains("redraw_requested_per_sec=6.0"),
            "expected ~6fps, got: {line}"
        );
    }
}
