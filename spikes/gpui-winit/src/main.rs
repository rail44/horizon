//! Leg-1 winit windowing backend spike: proves gpui can run against a
//! window opened by a winit `ApplicationHandler` (not gpui_linux's own
//! wayland/x11 backend), with decorations, wgpu-rendered text, and
//! keyboard input all flowing through the injected `Platform`. See
//! docs/research/winit-backend-spike.md for the findings this prototype
//! backs up.

mod active_loop;
mod app_handler;
mod dispatcher;
mod display;
mod platform;
mod window;

use std::rc::Rc;

use gpui::*;

use crate::platform::WinitPlatform;

struct DemoView {
    focus_handle: FocusHandle,
    typed: String,
}

impl DemoView {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            typed: String::new(),
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.typed.pop();
            }
            "space" => self.typed.push(' '),
            "enter" => self.typed.push('\n'),
            "escape" | "tab" => {}
            key => {
                let text = event
                    .keystroke
                    .key_char
                    .clone()
                    .unwrap_or_else(|| key.to_string());
                if text.chars().count() == 1 {
                    self.typed.push_str(&text);
                }
            }
        }
        log::info!("typed buffer now: {:?}", self.typed);
        cx.notify();
    }
}

impl Render for DemoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                view.handle_key(event, cx);
            }))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .text_color(rgb(0xffffff))
            .text_size(px(24.0))
            .child(format!(
                "gpui on a winit-owned window\n\ntype something: {}",
                self.typed
            ))
    }
}

fn main() {
    env_logger::init();

    let platform = Rc::new(WinitPlatform::new());
    let app = Application::with_platform(platform);

    app.run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.0), px(360.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("gpui-winit spike".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(DemoView::new);
                    let focus_handle = view.read(cx).focus_handle.clone();
                    window.focus(&focus_handle, cx);
                    view
                },
            )
            .expect("failed to open window");
        cx.activate(true);
        log::info!("window opened: {:?}", window.window_id());
    });
}
