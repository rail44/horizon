//! Pointer reporting, selection, hyperlinks and viewport wheel routing.

use super::input::{
    cell_from_position, is_openable_url, selection_kind_from_clicks, terminal_mouse_button,
    terminal_mouse_modifiers, url_from_lines, viewport_pixel_delta, ScrollAccumulator,
};
use super::{font::line_height, TerminalView};
use gpui::*;
use horizon_terminal_core::{TerminalMouseButton, TerminalMouseKind, TerminalMouseReport};

#[derive(Default)]
pub(super) struct PointerInput {
    selecting: bool,
    reporting_button: Option<TerminalMouseButton>,
    scroll_accum: ScrollAccumulator,
}

impl TerminalView {
    fn mouse_reporting(&self, cx: &App) -> bool {
        self.session
            .read(cx)
            .frame
            .as_ref()
            .is_some_and(|frame| frame.mouse_reporting)
    }

    fn cell_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<horizon_terminal_core::TerminalSelectionPoint> {
        let metrics = self.metrics.get()?;
        Some(cell_from_position(
            position,
            metrics.origin,
            metrics.cell_width,
            metrics.line_height,
        ))
    }

    /// The OSC 8 hyperlink under a pixel position, if any — the painted
    /// surface answers: a held scrollback window while one is visible (its
    /// fractional paint offset shifts row boundaries, so it folds into the
    /// row pick), the live frame otherwise.
    fn hyperlink_at(&self, position: Point<Pixels>, cx: &App) -> Option<String> {
        let metrics = self.metrics.get()?;
        let session = self.session.read(cx);
        let row_units = f32::from(position.y - metrics.origin.y) / f32::from(metrics.line_height);
        let col = (f32::from(position.x - metrics.origin.x) / f32::from(metrics.cell_width))
            .max(0.0)
            .floor() as usize;
        if let Some(scrollback) = session.visible_scrollback(self.last_size.get().rows as usize) {
            let row = (row_units + scrollback.fractional_row).max(0.0).floor() as usize;
            return url_from_lines(&scrollback.window.lines[scrollback.range.clone()], row, col);
        }
        url_from_lines(
            &session.frame.as_ref()?.lines,
            row_units.max(0.0).floor() as usize,
            col,
        )
    }

    pub(super) fn handle_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        // Cmd+click on a hyperlink opens it with the OS before mouse
        // reporting or selection can claim the click: a link activation must
        // never reach the attached application (where Cmd is not even
        // representable — `terminal_mouse_modifiers` drops it), while every
        // other click keeps its existing behavior. The scheme allow-list is
        // enforced here, at the open boundary.
        if event.button == MouseButton::Left && event.modifiers.platform {
            if let Some(url) = self.hyperlink_at(event.position, cx) {
                if is_openable_url(&url) {
                    cx.open_url(&url);
                    return;
                }
            }
        }
        if self.mouse_reporting(cx) {
            let Some(button) = terminal_mouse_button(event.button) else {
                return;
            };
            self.pointer.reporting_button = Some(button);
            self.session.read(cx).send_mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Press,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            });
        } else if event.button == MouseButton::Left {
            self.pointer.selecting = true;
            let kind = selection_kind_from_clicks(event.click_count);
            // `send_selection_start` drops any held scrollback window (review
            // fix ③): a selection is handed to the daemon-owned live viewport.
            // Notify so a bare click that starts a zero-width selection — which
            // may produce no frame — still switches the paint off the window
            // and onto the live frame immediately.
            self.session.read(cx).send_selection_start(point, kind);
            cx.notify();
        } else if event.button == MouseButton::Middle {
            self.paste_from_primary(cx);
        }
    }

    /// Middle-click paste from the OS primary selection buffer (X11/
    /// Wayland's middle-click-paste convention, only while mouse reporting
    /// is off -- see `handle_mouse_down`). No-op off Linux/FreeBSD,
    /// matching GPUI's native platform support for primary selection.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn paste_from_primary(&self, cx: &App) {
        if let Some(text) = cx
            .read_from_primary()
            .and_then(|item| item.text().map(|text| text.to_string()))
        {
            self.session.read(cx).send_paste(text);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn paste_from_primary(&self, _cx: &App) {}

    pub(super) fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &App) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_reporting(cx) {
            let Some(button) = self.pointer.reporting_button else {
                return;
            };
            self.session.read(cx).send_mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Drag,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            });
        } else if self.pointer.selecting {
            self.session.read(cx).send_selection_update(point);
        }
    }

    pub(super) fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &App) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_reporting(cx) {
            let button = self
                .pointer
                .reporting_button
                .take()
                .or_else(|| terminal_mouse_button(event.button));
            let Some(button) = button else {
                return;
            };
            self.session.read(cx).send_mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Release,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            });
        } else if event.button == MouseButton::Left && self.pointer.selecting {
            self.pointer.selecting = false;
            self.session.read(cx).send_selection_update(point);
        }
    }

    pub(super) fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let local_scrollback = self.session.read(cx).local_scrollback_available();
        if local_scrollback {
            // Passthrough debt is unrelated to the visible fractional
            // position and must not leak across a primary/alternate-screen
            // transition.
            self.pointer.scroll_accum.reset();
            // The presentation surface owns one continuous pixel position.
            // Precise input keeps its exact distance; imprecise wheel input is
            // normalized once at this boundary and is never interpreted as a
            // terminal-row UI operation.
            let pixels = viewport_pixel_delta(event.delta);
            if pixels.abs() <= f32::EPSILON {
                return;
            }
            let viewport_rows = self.last_size.get().rows as usize;
            let repaint =
                self.session
                    .read(cx)
                    .scroll_viewport(pixels, line_height(), viewport_rows);
            if repaint {
                cx.notify();
            }
            return;
        }

        // The terminal application or an old peer owns this wheel. Preserve
        // the existing discrete protocol path.
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if let Some(lines) =
            self.pointer
                .scroll_accum
                .consume(event.delta, event.touch_phase, line_height())
        {
            self.session.read(cx).scroll_protocol(lines, point);
        }
    }
}
