//! winit windowing backend spike. Leg 1 proved gpui can run against a
//! window opened by a winit `ApplicationHandler` (not gpui_linux's own
//! wayland/x11 backend), with decorations, wgpu-rendered text, and
//! keyboard input all flowing through the injected `Platform`. Leg 2 adds
//! Japanese IME preedit/commit, exercising the same `EntityInputHandler` +
//! `ElementInputHandler` shape Horizon's real terminal view uses
//! (`src/terminal/mod.rs`). See docs/research/winit-backend-spike.md for
//! the findings this prototype backs up.

mod active_loop;
mod app_handler;
mod dispatcher;
mod display;
mod platform;
mod window;

use std::ops::Range;
use std::rc::Rc;

use gpui::*;

use crate::platform::WinitPlatform;

/// UTF-16 length of `s`, the unit `EntityInputHandler`'s ranges are
/// expressed in (matching Horizon's `TerminalView` helper of the same
/// name in `src/terminal/mod.rs`).
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

struct DemoView {
    focus_handle: FocusHandle,
    typed: String,
    // Client-side-only preedit state, never sent anywhere but rendered as
    // an underlined overlay and cleared on commit — same shape as
    // Horizon's `TerminalView::ime_marked_text`.
    marked_text: Option<String>,
}

impl DemoView {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            typed: String::new(),
            marked_text: None,
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // While composing, printable keys arrive through
        // replace_and_mark_text_in_range/replace_text_in_range instead —
        // same guard as Horizon's TerminalView::on_key_down.
        if self.marked_text.is_some() {
            return;
        }
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
        // Caret moved outside of a composition; let the platform reposition
        // the (currently invisible) IME candidate window for next time.
        window.invalidate_character_coordinates();
        cx.notify();
    }
}

impl EntityInputHandler for DemoView {
    fn text_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        self.marked_text.clone()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.marked_text.as_deref().map(utf16_len).unwrap_or(0);
        Some(UTF16Selection {
            range: caret..caret,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_deref()
            .map(|marked| 0..utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_composing = self.marked_text.take().is_some();
        self.typed.push_str(text);
        log::info!(
            "ime commit: {text:?} (was_composing={was_composing}, buffer now {:?})",
            self.typed
        );
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_string())
        };
        log::info!("ime preedit: {:?}", self.marked_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Deliberately as simple as Horizon's own bounds_for_range
        // (cursor cell + range_utf16.start * advance) — see
        // docs/research/gpui-terminal-implementations.md S3 point 6 for why
        // every surveyed project stops at this level of precision.
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&window.text_style().font());
        let advance = text_system
            .advance(font_id, px(24.0), 'M')
            .map(|size| size.width)
            .unwrap_or(px(12.0));
        let prefix_chars = self.typed.chars().count() + range_utf16.start;
        let origin = element_bounds.origin + point(advance * prefix_chars as f32, px(0.0));
        let bounds = Bounds::new(origin, size(advance, px(30.0)));
        log::info!("bounds_for_range({range_utf16:?}) -> {bounds:?}");
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for DemoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.focus_handle.clone();
        let entity = cx.entity();
        div()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .track_focus(&focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.handle_key(event, window, cx);
            }))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .text_color(rgb(0xffffff))
            .text_size(px(24.0))
            .child("gpui on a winit-owned window (leg 2: IME)")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .child(format!("type something: {}", self.typed))
                    .child(
                        div()
                            .underline()
                            .child(self.marked_text.clone().unwrap_or_default()),
                    ),
            )
            // No visual output of its own: wires window.handle_input so
            // gpui routes IME calls (set_input_handler/take_input_handler,
            // see window.rs) at this entity, matching the
            // ElementInputHandler pattern from gpui's own
            // examples/input.rs and Horizon's TerminalElement::paint.
            .child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
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
