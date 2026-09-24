//! The terminal pane view: grid-positioned span painting, key routing
//! through `TerminalCommand::Key`, and IME via `EntityInputHandler`.
//! Patterns and their provenance are recorded in
//! docs/gpui-migration-design.md and
//! docs/research/gpui-terminal-implementations.md.
//!
//! Headless verification taps (promoted from the spike):
//! - `HORIZON_GPUI_DUMP=<path>` mirrors every snapshot (text + span
//!   color table) to a file.
//! - `HORIZON_GPUI_DRIVE=<bytes>` types bytes into the session shortly
//!   after startup; `HORIZON_GPUI_DRIVE_ENTER=1` sends the trailing
//!   newline as a `TerminalCommand::Key` to exercise the core encoder.
//!   Note this bypasses `handle_key`/`replace_text_in_range` in `keyboard`
//!   entirely (it writes straight to the session's command channel), so
//!   it cannot exercise or verify the real input pipeline the rest of
//!   the input modules implement.
//! - `HORIZON_INPUT_TRACE=1` (or a file path) traces every hop of the
//!   real input pipeline instead — see `crate::input_trace`.

mod diagnostics;
mod font;
mod glyphs;
mod input;
mod keyboard;
mod paint;
mod pointer;
mod scrollback;
mod session;
mod shape_cache;
mod shaping;

use crate::theme;
#[cfg(test)]
pub(crate) use font::DEFAULT_FONT_SIZE;
pub(crate) use font::{
    adjust_font_size, font_size, reset_font_size, resolved_font, FONT_SIZE_STEP,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use horizon_terminal_core::TerminalSize;
use keyboard::KeyboardInput;
use paint::{paint_terminal, PaintCaches, PaintMetrics};
use pointer::PointerInput;
pub(crate) use session::TerminalSession;
use std::{cell::Cell, rc::Rc};

/// Key context applied to the terminal pane's root `div` so workspace-wide
/// bindings scoped to the enclosing `Root`/`Workspace` contexts can be
/// overridden here. Specifically, gpui-component's `Root` binds bare
/// `tab`/`shift-tab` to its `Tab`/`TabPrev` focus-traversal actions in the
/// `"Root"` context (`crates/ui/src/root.rs`); without a more-specific
/// context on the terminal pane, that binding wins and the `KeyDownEvent`
/// for Tab is consumed as an action before it can reach the pane's
/// `on_key_down` handler — silently swallowing Tab (board #31). The
/// `NoAction` overrides registered against this context in
/// `workspace::bindings` cancel that, letting Tab fall through to the
/// terminal's key encoder as `0x09`.
pub(crate) const TERMINAL_CONTEXT: &str = "Terminal";

pub(crate) struct TerminalView {
    // The pane's session — owned by the shell's session store, not this
    // view, so a closed pane detaches rather than terminates.
    session: Entity<TerminalSession>,
    focus_handle: FocusHandle,
    // Shared with the paint closure (which only gets &mut App, not the
    // entity) so bounds-driven resize can be deduped without an update.
    last_size: Rc<Cell<TerminalSize>>,
    metrics: Rc<Cell<Option<PaintMetrics>>>,
    // Row-keyed memos of shaped lines (see `shape_cache`), shared with the
    // paint closure the same way as `last_size`/`metrics`. Live rows use
    // viewport generations; the held scrollback window uses its immutable
    // window-row indices, so moving by one row shapes only the exposed edge.
    paint_caches: Rc<PaintCaches>,
    keyboard: KeyboardInput,
    pointer: PointerInput,
    _session_observation: Subscription,
}

impl TerminalView {
    pub(crate) fn new(
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let observation = cx.observe(&session, |_, _, cx| cx.notify());
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            session,
            focus_handle,
            last_size: Rc::new(Cell::new(TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            })),
            metrics: Rc::new(Cell::new(None)),
            paint_caches: Rc::new(PaintCaches::new()),
            keyboard: KeyboardInput::default(),
            pointer: PointerInput::default(),
            _session_observation: observation,
        }
    }

    /// Status text shown at the bottom of the pane when the session is
    /// unreachable or the shell has exited, parity with `AgentView::status_line`.
    fn status_line(&self, cx: &App) -> (String, Hsla) {
        let session = self.session.read(cx);
        if session.exited() {
            return ("shell exited — session closed".to_string(), theme::danger());
        }
        if session.runtime_unreachable() {
            return (
                session
                    .error()
                    .unwrap_or_else(|| "session runtime unreachable".to_string()),
                theme::danger(),
            );
        }
        (String::new(), theme::text_muted())
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let last_size = self.last_size.clone();
        let metrics = self.metrics.clone();
        let paint_caches = self.paint_caches.clone();
        let focus_handle = self.focus_handle.clone();
        let (status, status_color) = self.status_line(cx);
        fn on_down(
            view: &mut TerminalView,
            event: &MouseDownEvent,
            _window: &mut Window,
            cx: &mut Context<TerminalView>,
        ) {
            view.handle_mouse_down(event, cx);
        }
        fn on_up(
            view: &mut TerminalView,
            event: &MouseUpEvent,
            _window: &mut Window,
            cx: &mut Context<TerminalView>,
        ) {
            view.handle_mouse_up(event, cx);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::background()))
            .key_context(TERMINAL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                view.handle_key(event, cx);
            }))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _window, cx| {
                view.handle_key_up(event, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(on_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(on_down))
            .on_mouse_up(MouseButton::Left, cx.listener(on_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(on_up))
            .on_mouse_up(MouseButton::Right, cx.listener(on_up))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                view.handle_mouse_move(event, cx);
            }))
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _window, cx| {
                view.handle_scroll_wheel(event, cx);
            }))
            .child(
                div().flex_1().min_h_0().overflow_hidden().child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, cx| {
                            window.handle_input(
                                &focus_handle,
                                ElementInputHandler::new(bounds, entity.clone()),
                                cx,
                            );
                            paint_terminal(
                                bounds,
                                &entity,
                                &last_size,
                                &metrics,
                                &paint_caches,
                                window,
                                cx,
                            );
                        },
                    )
                    .size_full(),
                ),
            )
            // Status row (backlog #35 parity with `AgentView::status_line`):
            // rendered only when there's something to say, so an ordinary,
            // reachable pane looks exactly as before.
            .when(!status.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .text_size(px(11.0))
                        .text_color(status_color)
                        .child(status),
                )
            })
    }
}
