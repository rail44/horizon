use crate::terminal::{
    TerminalCommand, TerminalFrame, TerminalMouseButton, TerminalMouseKind, TerminalMouseReport,
    TerminalScroll, TerminalSize,
};
use crossbeam_channel::Sender;
use floem::{
    action::set_ime_cursor_area,
    context::{EventCx, PaintCx, UpdateCx},
    event::{Event, EventPropagation},
    peniko::{
        kurbo::{Point, Rect, Size},
        Color,
    },
    reactive::create_updater,
    View, ViewId,
};
use floem_renderer::Renderer;

mod input;
mod layout;
mod metrics;
mod preedit;
mod render;

use input::{
    cell_from_point, scroll_lines_from_wheel, terminal_mouse_button, terminal_mouse_modifiers,
};
use layout::{build_line_layouts, update_line_layouts, CellLayout};
use metrics::TerminalMetrics;
use preedit::{build_preedit_layout, PreeditLayout};
use render::{draw_block_element, expanded_rect};

const PADDING_X: f64 = 12.0;
const PADDING_Y: f64 = 12.0;
const FALLBACK_CELL_WIDTH: f64 = 8.0;

/// `[terminal].font_size`, resolved from Horizon's config file — see
/// `terminal::config::TerminalConfig`.
fn font_size() -> f32 {
    crate::terminal::config::TerminalConfig::from_env().font_size
}

/// `[terminal].line_height`, same source as [`font_size`].
fn line_height() -> f64 {
    crate::terminal::config::TerminalConfig::from_env().line_height
}

pub(crate) fn terminal_text_view(
    frame: impl Fn() -> TerminalFrame + 'static,
    preedit: impl Fn() -> Option<String> + 'static,
    terminal_tx: Option<Sender<TerminalCommand>>,
    window_origin: impl Fn() -> Point + 'static,
    update_ime_cursor_area: impl Fn(Point, Size) + 'static,
) -> impl floem::IntoView {
    let id = ViewId::new();
    let initial = create_updater(
        move || TerminalViewState {
            frame: frame(),
            preedit: preedit(),
        },
        move |new_state| id.update_state(new_state),
    );
    TerminalTextView::new(
        id,
        initial,
        terminal_tx,
        Box::new(window_origin),
        Box::new(update_ime_cursor_area),
    )
}

struct TerminalTextView {
    id: ViewId,
    frame: TerminalFrame,
    lines: Vec<Vec<CellLayout>>,
    preedit: Option<PreeditLayout>,
    metrics: TerminalMetrics,
    terminal_tx: Option<Sender<TerminalCommand>>,
    last_size: Option<TerminalSize>,
    selecting: bool,
    reporting_button: Option<TerminalMouseButton>,
    window_origin: Box<dyn Fn() -> Point>,
    update_ime_cursor_area: Box<dyn Fn(Point, Size)>,
}

struct TerminalViewState {
    frame: TerminalFrame,
    preedit: Option<String>,
}

impl TerminalTextView {
    fn new(
        id: ViewId,
        state: TerminalViewState,
        terminal_tx: Option<Sender<TerminalCommand>>,
        window_origin: Box<dyn Fn() -> Point>,
        update_ime_cursor_area: Box<dyn Fn(Point, Size)>,
    ) -> Self {
        let lines = build_line_layouts(&state.frame);
        let preedit = build_preedit_layout(state.preedit.as_deref());
        Self {
            id,
            frame: state.frame,
            lines,
            preedit,
            metrics: TerminalMetrics::default(),
            terminal_tx,
            last_size: None,
            selecting: false,
            reporting_button: None,
            window_origin,
            update_ime_cursor_area,
        }
    }

    fn set_state(&mut self, state: TerminalViewState) {
        update_line_layouts(&mut self.lines, &self.frame.lines, &state.frame.lines);
        self.frame = state.frame;
        self.preedit = build_preedit_layout(state.preedit.as_deref());
        self.id.request_paint();
    }
}

impl View for TerminalTextView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "TerminalTextView".into()
    }

    fn update(&mut self, _cx: &mut UpdateCx, state: Box<dyn std::any::Any>) {
        if let Ok(state) = state.downcast::<TerminalViewState>() {
            self.set_state(*state);
        }
    }

    fn event_after_children(&mut self, _cx: &mut EventCx, event: &Event) -> EventPropagation {
        match event {
            Event::PointerDown(pointer) if self.frame.mouse_reporting => {
                let Some(button) = terminal_mouse_button(pointer.button) else {
                    return EventPropagation::Continue;
                };
                self.reporting_button = Some(button);
                self.send_selection_command(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Press,
                    button,
                    point: cell_from_point(pointer.pos, self.metrics),
                    modifiers: terminal_mouse_modifiers(pointer.modifiers),
                }));
                EventPropagation::Stop
            }
            Event::PointerDown(pointer) if pointer.button.is_primary() => {
                self.selecting = true;
                self.send_selection_command(TerminalCommand::SelectionStart(cell_from_point(
                    pointer.pos,
                    self.metrics,
                )));
                EventPropagation::Continue
            }
            Event::PointerMove(pointer) if self.frame.mouse_reporting => {
                let Some(button) = self.reporting_button else {
                    return EventPropagation::Continue;
                };
                self.send_selection_command(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Drag,
                    button,
                    point: cell_from_point(pointer.pos, self.metrics),
                    modifiers: terminal_mouse_modifiers(pointer.modifiers),
                }));
                EventPropagation::Stop
            }
            Event::PointerMove(pointer) if self.selecting => {
                self.send_selection_command(TerminalCommand::SelectionUpdate(cell_from_point(
                    pointer.pos,
                    self.metrics,
                )));
                EventPropagation::Stop
            }
            Event::PointerUp(pointer) if self.frame.mouse_reporting => {
                let button = self
                    .reporting_button
                    .take()
                    .or_else(|| terminal_mouse_button(pointer.button));
                let Some(button) = button else {
                    return EventPropagation::Continue;
                };
                self.send_selection_command(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Release,
                    button,
                    point: cell_from_point(pointer.pos, self.metrics),
                    modifiers: terminal_mouse_modifiers(pointer.modifiers),
                }));
                EventPropagation::Stop
            }
            Event::PointerUp(pointer) if pointer.button.is_primary() && self.selecting => {
                self.selecting = false;
                self.send_selection_command(TerminalCommand::SelectionUpdate(cell_from_point(
                    pointer.pos,
                    self.metrics,
                )));
                EventPropagation::Stop
            }
            Event::PointerWheel(pointer) => {
                if let Some(lines) = scroll_lines_from_wheel(pointer.delta.y) {
                    self.send_selection_command(TerminalCommand::Scroll(TerminalScroll {
                        lines,
                        point: cell_from_point(pointer.pos, self.metrics),
                    }));
                    EventPropagation::Stop
                } else {
                    EventPropagation::Continue
                }
            }
            _ => EventPropagation::Continue,
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let height = self
            .id
            .get_layout()
            .map(|layout| layout.size.height as f64)
            .unwrap_or_default();
        let width = self
            .id
            .get_layout()
            .map(|layout| layout.size.width as f64)
            .unwrap_or_default();
        if width <= PADDING_X * 2.0 || height <= PADDING_Y * 2.0 {
            return;
        }
        let clip = Rect::new(0.0, 0.0, width, height);
        let cell_width = self.metrics.cell_width;
        let line_height = self.metrics.line_height;
        let max_rows = ((height - PADDING_Y * 2.0) / line_height).max(0.0).floor() as usize;
        let max_cols = ((width - PADDING_X * 2.0) / cell_width).max(0.0).floor() as usize;
        self.resize_terminal(max_cols, max_rows);

        cx.save();
        cx.clip(&clip);

        for (row, cells) in self.lines.iter().take(max_rows).enumerate() {
            let y = PADDING_Y + row as f64 * line_height;
            let mut col = 0_usize;
            for cell in cells {
                if col >= max_cols {
                    break;
                }

                let x = PADDING_X + col as f64 * cell_width;
                let columns = cell.columns.min(max_cols - col);
                let bg_rect = Rect::new(x, y, x + columns as f64 * cell_width, y + line_height);
                // Always painted (no "skip when it matches the pane's
                // ambient background" shortcut): that shortcut compared
                // against a literal hardcoded color, which happened to
                // match the terminal's old fixed default background. Now
                // that the terminal's background projects from the app
                // theme (`ui::theme::terminal_background`) and is
                // configurable independently of the workspace pane's own
                // theme, the resolved background must always be drawn
                // explicitly to actually show up.
                cx.fill(
                    &expanded_rect(bg_rect),
                    Color::from_rgb8(cell.bg[0], cell.bg[1], cell.bg[2]),
                    0.0,
                );
                col += columns;
            }
        }

        for (row, cells) in self.lines.iter().take(max_rows).enumerate() {
            let y = PADDING_Y + row as f64 * line_height;
            let mut col = 0_usize;
            for cell in cells {
                if col >= max_cols {
                    break;
                }

                let x = PADDING_X + col as f64 * cell_width;
                let columns = cell.columns.min(max_cols - col);
                let cell_rect = Rect::new(x, y, x + columns as f64 * cell_width, y + line_height);
                if let Some(block) = cell.block {
                    draw_block_element(cx, block, cell_rect, cell.fg);
                } else if cell.visible {
                    cx.draw_text(&cell.text, Point::new(x, y));
                }
                col += columns;
            }
        }

        if let Some(cursor) = self.frame.cursor {
            if cursor.row < max_rows && cursor.col < max_cols {
                let x = PADDING_X + cursor.col as f64 * cell_width;
                let y = PADDING_Y + cursor.row as f64 * line_height;
                let ime_pos = (self.window_origin)() + Point::new(x, y + line_height).to_vec2();
                let ime_size = Size::new(cell_width, line_height);
                (self.update_ime_cursor_area)(ime_pos, ime_size);
                set_ime_cursor_area(ime_pos, ime_size);
                if let Some(preedit) = &self.preedit {
                    let preedit_width = preedit.columns.max(1).min(max_cols - cursor.col);
                    let preedit_rect =
                        Rect::new(x, y, x + preedit_width as f64 * cell_width, y + line_height);
                    cx.fill(&preedit_rect, Color::from_rgb8(52, 58, 70), 0.0);
                    cx.draw_text(&preedit.text, Point::new(x, y));
                    let underline = Rect::new(
                        x,
                        y + line_height - 2.0,
                        x + preedit_width as f64 * cell_width,
                        y + line_height - 1.0,
                    );
                    cx.fill(&underline, Color::from_rgb8(132, 220, 198), 0.0);
                } else {
                    let rect = Rect::new(x, y, x + cell_width, y + line_height);
                    let cursor = crate::terminal::config::resolved_colors().cursor;
                    cx.fill(
                        &rect,
                        Color::from_rgba8(cursor[0], cursor[1], cursor[2], 150),
                        0.0,
                    );
                }
            }
        }

        cx.restore();
    }
}

impl TerminalTextView {
    fn resize_terminal(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(1, u16::MAX as usize) as u16;
        let rows = rows.clamp(1, u16::MAX as usize) as u16;
        let size = TerminalSize {
            cols,
            rows,
            pixel_width: pixel_dimension(cols, self.metrics.cell_width),
            pixel_height: pixel_dimension(rows, self.metrics.line_height),
        };
        if self.last_size == Some(size) {
            return;
        }
        self.last_size = Some(size);

        if let Some(tx) = &self.terminal_tx {
            let _ = tx.send(TerminalCommand::Resize(size));
        }
    }

    fn send_selection_command(&self, command: TerminalCommand) {
        if let Some(tx) = &self.terminal_tx {
            let _ = tx.send(command);
        }
    }
}

/// Total grid pixel size along one axis (`count * cell_extent`), clamped
/// into `u16` for `TerminalSize::pixel_width`/`pixel_height` — see the
/// field docs on `TerminalSize` for why the PTY needs this instead of the
/// zeros it used to get.
fn pixel_dimension(count: u16, cell_extent: f64) -> u16 {
    (count as f64 * cell_extent)
        .round()
        .clamp(0.0, u16::MAX as f64) as u16
}

#[cfg(test)]
use input::cell_from_point as _test_cell_from_point;
#[cfg(test)]
use layout::{
    build_span_cells as _test_build_span_cells, update_line_layouts as _test_update_line_layouts,
    BlockElement as _TestBlockElement,
};
#[cfg(test)]
use metrics::{
    measured_cell_width as _test_measured_cell_width,
    terminal_font_family as _test_terminal_font_family,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalLine;
    use crate::terminal::TerminalSelectionPoint;
    use crate::terminal::TerminalSpan;
    use floem::text::FamilyOwned;

    fn test_line(text: &str) -> TerminalLine {
        TerminalLine {
            spans: vec![TerminalSpan {
                text: text.to_string(),
                columns: text.chars().count(),
                fg: [1, 2, 3],
                bg: [4, 5, 6],
            }],
        }
    }

    fn test_family() -> Vec<FamilyOwned> {
        _test_terminal_font_family()
    }

    #[test]
    fn measured_cell_width_is_usable() {
        assert!(_test_measured_cell_width() > 1.0);
    }

    #[test]
    fn resize_terminal_derives_pixel_dimensions_from_metrics() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut view = TerminalTextView::new(
            ViewId::new(),
            TerminalViewState {
                frame: TerminalFrame::from_text(String::new()),
                preedit: None,
            },
            Some(tx),
            Box::new(|| Point::ZERO),
            Box::new(|_, _| {}),
        );
        view.metrics = TerminalMetrics {
            cell_width: 9.0,
            line_height: 18.0,
        };

        view.resize_terminal(80, 24);

        match rx.try_recv() {
            Ok(TerminalCommand::Resize(size)) => {
                assert_eq!(size.cols, 80);
                assert_eq!(size.rows, 24);
                assert_eq!(size.pixel_width, 720);
                assert_eq!(size.pixel_height, 432);
            }
            other => panic!("expected a Resize command, got {other:?}"),
        }
    }

    #[test]
    fn cell_from_point_uses_terminal_metrics() {
        let metrics = TerminalMetrics {
            cell_width: 10.0,
            line_height: 20.0,
        };

        assert_eq!(
            _test_cell_from_point(Point::new(PADDING_X + 21.0, PADDING_Y + 41.0), metrics),
            TerminalSelectionPoint { row: 2, col: 2 }
        );
    }

    #[test]
    fn span_cells_expand_spaces_as_invisible_cells() {
        let cells = _test_build_span_cells(
            &TerminalSpan {
                text: String::new(),
                columns: 3,
                fg: [1, 2, 3],
                bg: [4, 5, 6],
            },
            &test_family(),
        );

        assert_eq!(cells.len(), 3);
        assert!(cells.iter().all(|cell| !cell.visible));
        assert_eq!(cells.iter().map(|cell| cell.columns).sum::<usize>(), 3);
    }

    #[test]
    fn span_cells_preserve_wide_and_combining_columns() {
        let cells = _test_build_span_cells(
            &TerminalSpan {
                text: "A日e\u{301}".to_string(),
                columns: 4,
                fg: [1, 2, 3],
                bg: [4, 5, 6],
            },
            &test_family(),
        );

        assert_eq!(cells.len(), 3);
        assert_eq!(
            cells.iter().map(|cell| cell.columns).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert_eq!(cells.iter().map(|cell| cell.columns).sum::<usize>(), 4);
    }

    #[test]
    fn span_cells_mark_block_elements_for_rect_rendering() {
        let cells = _test_build_span_cells(
            &TerminalSpan {
                text: "█▄▀".to_string(),
                columns: 3,
                fg: [1, 2, 3],
                bg: [4, 5, 6],
            },
            &test_family(),
        );

        assert_eq!(
            cells.iter().map(|cell| cell.block).collect::<Vec<_>>(),
            vec![
                Some(_TestBlockElement::Full),
                Some(_TestBlockElement::LowerFraction(4)),
                Some(_TestBlockElement::UpperFraction(4)),
            ]
        );
    }

    #[test]
    fn span_cells_mark_fractional_block_elements_for_rect_rendering() {
        let cells = _test_build_span_cells(
            &TerminalSpan {
                text: "▏▎▍▌▋▊▉▁▂▃▄▅▆▇".to_string(),
                columns: 14,
                fg: [1, 2, 3],
                bg: [4, 5, 6],
            },
            &test_family(),
        );

        assert_eq!(
            cells.iter().map(|cell| cell.block).collect::<Vec<_>>(),
            vec![
                Some(_TestBlockElement::LeftFraction(1)),
                Some(_TestBlockElement::LeftFraction(2)),
                Some(_TestBlockElement::LeftFraction(3)),
                Some(_TestBlockElement::LeftFraction(4)),
                Some(_TestBlockElement::LeftFraction(5)),
                Some(_TestBlockElement::LeftFraction(6)),
                Some(_TestBlockElement::LeftFraction(7)),
                Some(_TestBlockElement::LowerFraction(1)),
                Some(_TestBlockElement::LowerFraction(2)),
                Some(_TestBlockElement::LowerFraction(3)),
                Some(_TestBlockElement::LowerFraction(4)),
                Some(_TestBlockElement::LowerFraction(5)),
                Some(_TestBlockElement::LowerFraction(6)),
                Some(_TestBlockElement::LowerFraction(7)),
            ]
        );
    }

    #[test]
    fn update_line_layouts_rebuilds_only_changed_rows() {
        let old = vec![test_line("aaa"), test_line("bbb")];
        let mut lines = Vec::new();
        _test_update_line_layouts(&mut lines, &[], &old);
        assert_eq!(lines.len(), 2);

        // Row 0 changes, row 1 stays identical: both rows must reflect the
        // new content (rebuilt or retained), and the retained row's cell
        // metadata must be untouched.
        let new = vec![test_line("zzz"), test_line("bbb")];
        _test_update_line_layouts(&mut lines, &old, &new);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 3);
        assert!(lines[0].iter().all(|cell| cell.visible));
        assert_eq!(lines[1].len(), 3);
        assert_eq!(lines[1].iter().map(|cell| cell.columns).sum::<usize>(), 3);
    }

    #[test]
    fn update_line_layouts_grows_and_truncates_rows() {
        let mut lines = Vec::new();
        _test_update_line_layouts(&mut lines, &[], &[test_line("a")]);
        assert_eq!(lines.len(), 1);

        let grown = vec![test_line("a"), test_line("b"), test_line("c")];
        _test_update_line_layouts(&mut lines, &[test_line("a")], &grown);
        assert_eq!(lines.len(), 3);

        _test_update_line_layouts(&mut lines, &grown, &[test_line("a")]);
        assert_eq!(lines.len(), 1);
    }
}
