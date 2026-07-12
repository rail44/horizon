//! The winit `ApplicationHandler`: the actual event-loop pump. `Platform::run`
//! hands control to `EventLoop::run_app` with one of these, and every
//! callback below re-enters gpui via the callbacks `WinitPlatformWindow`
//! stashed (`on_request_frame`, `on_input`, `on_resize`, ...), wrapped in an
//! `ActiveLoopGuard` so `Platform::open_window` can reach the
//! `ActiveEventLoop` if gpui decides to open a window mid-callback.
//!
//! Redraw scheduling mirrors gpui_web's requestAnimationFrame loop: each
//! `RedrawRequested` immediately re-requests the next redraw, so frames
//! keep flowing as long as the window exists. gpui's own
//! `on_request_frame` closure (registered in `Window::new`, see
//! gpui/src/window.rs) throttles this down to ~30fps when unfocused and
//! relies on the surface's Fifo present mode to pace it to vsync while
//! focused — we don't need our own throttle on top of that.

use std::rc::Rc;

use gpui::{
    KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollWheelEvent,
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

pub(crate) struct WinitAppHandler<'a> {
    platform: &'a WinitPlatform,
    on_finish_launching: Option<Box<dyn FnOnce()>>,
}

impl<'a> WinitAppHandler<'a> {
    pub(crate) fn new(platform: &'a WinitPlatform, on_finish_launching: Box<dyn FnOnce()>) -> Self {
        Self {
            platform,
            on_finish_launching: Some(on_finish_launching),
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
            WinitUserEvent::Wake => {}
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
                inner.window.request_redraw();
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
                inner.window.request_redraw();
            }
            WindowEvent::Focused(is_focused) => {
                inner.state.borrow_mut().is_active = is_focused;
                if let Some(callback) = inner.callbacks.borrow_mut().active_status_change.as_mut() {
                    callback(is_focused);
                }
                inner.window.request_redraw();
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
                    let input = match event.state {
                        ElementState::Pressed => PlatformInput::KeyDown(KeyDownEvent {
                            keystroke,
                            is_held: event.repeat,
                            prefer_character_input: false,
                        }),
                        ElementState::Released => PlatformInput::KeyUp(KeyUpEvent { keystroke }),
                    };
                    dispatch_input(&inner, input);
                    inner.window.request_redraw();
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
                inner.window.request_redraw();
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
                inner.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let callback = inner.callbacks.borrow_mut().request_frame.take();
                if let Some(mut callback) = callback {
                    callback(gpui::RequestFrameOptions {
                        require_presentation: true,
                        force_render: false,
                    });
                    inner.callbacks.borrow_mut().request_frame = Some(callback);
                }
                // Keep the loop going, matching gpui_web's rAF-reschedules-
                // itself pattern; see module docs for the throttle this
                // relies on.
                inner.window.request_redraw();
            }
            _ => {}
        }

        self.platform.dispatcher.drain_main_queue();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _guard = ActiveLoopGuard::enter(event_loop);
        self.platform.dispatcher.drain_main_queue();
    }
}

fn dispatch_input(inner: &WinitWindowInner, input: PlatformInput) {
    if let Some(callback) = inner.callbacks.borrow_mut().input.as_mut() {
        callback(input);
    }
}
