//! Keeps the platform IME out of keystroke-only focus states.
//!
//! Some focus states take bare keys as commands rather than text: workspace
//! mode on the shell root (`h`/`j`/`k`/`l`, `:`), and the board views'
//! own key handling. With an IME switched on, those keys must reach the
//! key bindings instead of becoming preedit.
//!
//! gpui's Wayland backend decides once per frame whether the window's
//! `zwp_text_input_v3` stays enabled (`update_ime_enabled` in
//! gpui-pre-linux's `wayland/window.rs`): with an input handler installed
//! it asks the handler's `accepts_text_input`; with none it assumes `true`
//! and keeps text input enabled. A focus state without a handler of its own
//! therefore leaves the IME on, the IME turns `h` into preedit, and the
//! preedit is dropped because nothing is there to receive it — the binding
//! never sees the key.
//!
//! [`keystroke_only`] closes that gap: it returns an invisible, zero-size
//! element that, while the given focus handle is *exactly* focused,
//! installs [`KeystrokeOnlyInput`] — a handler that refuses text input — so
//! the backend sends `disable` and keys arrive as plain keystrokes. A
//! focused descendant with its own handler (a text field) is untouched,
//! because `Window::handle_input` registers only for the exactly focused
//! handle. Platform scope is recorded in `docs/workspace-mode-design.md`.

use std::ops::Range;

use gpui::{
    canvas, App, Bounds, FocusHandle, InputHandler, IntoElement, Pixels, Point, Styled as _,
    UTF16Selection, Window,
};

/// An input handler with no document: it holds no text, no selection, and
/// no marked range, ignores every edit, and refuses text input.
struct KeystrokeOnlyInput;

impl InputHandler for KeystrokeOnlyInput {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        None
    }

    fn marked_text_range(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Range<usize>> {
        None
    }

    fn text_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        _text: &str,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut App) {}

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        false
    }
}

/// An invisible element that installs [`KeystrokeOnlyInput`] for
/// `focus_handle` during paint. Add it as a child of the element that
/// tracks `focus_handle`; it is absolutely positioned with zero size, so it
/// takes no part in layout.
pub(crate) fn keystroke_only(focus_handle: &FocusHandle) -> impl IntoElement {
    register(focus_handle, KeystrokeOnlyInput)
}

fn register(focus_handle: &FocusHandle, handler: impl InputHandler) -> impl IntoElement {
    let focus_handle = focus_handle.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, cx| window.handle_input(&focus_handle, handler, cx),
    )
    .absolute()
    .size_0()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        div, AppContext as _, Context, FocusHandle, InteractiveElement as _, ParentElement as _,
        Render, TestAppContext,
    };

    use super::*;

    /// Counts how often gpui asks the installed handler whether it accepts
    /// text input — it does so once per frame for the handler it hands to
    /// the platform window.
    struct Probe(Rc<Cell<usize>>);

    impl InputHandler for Probe {
        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut App,
        ) -> Option<UTF16Selection> {
            None
        }
        fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
            None
        }
        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<String> {
            None
        }
        fn replace_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: &mut Window,
            _: &mut App,
        ) {
        }
        fn replace_and_mark_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) {
        }
        fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}
        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<Bounds<Pixels>> {
            None
        }
        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<usize> {
            None
        }
        fn accepts_text_input(&mut self, _: &mut Window, _: &mut App) -> bool {
            self.0.set(self.0.get() + 1);
            false
        }
    }

    struct View {
        outer: FocusHandle,
        inner: FocusHandle,
        asked: Rc<Cell<usize>>,
    }

    impl Render for View {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.outer)
                .child(register(&self.outer, Probe(self.asked.clone())))
                .child(div().track_focus(&self.inner))
        }
    }

    #[gpui::test]
    fn installs_only_while_the_handle_is_exactly_focused(cx: &mut TestAppContext) {
        let asked = Rc::new(Cell::new(0));
        let window = cx.add_window({
            let asked = asked.clone();
            |_, cx| View {
                outer: cx.focus_handle(),
                inner: cx.focus_handle(),
                asked,
            }
        });
        let (outer, inner) = window
            .read_with(cx, |view, _| (view.outer.clone(), view.inner.clone()))
            .unwrap();
        // Draw through the untyped handle: drawing renders `View`, which a
        // typed `update` would already hold.
        let frame = |focus: &FocusHandle, cx: &mut TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.focus(focus, cx);
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };

        frame(&outer, cx);
        assert_eq!(asked.get(), 1, "focused handle gets the handler");

        frame(&inner, cx);
        assert_eq!(asked.get(), 1, "a focused descendant does not");
    }
}
