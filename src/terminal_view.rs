use crossbeam_channel::Sender;
use floem::{
    action::set_ime_cursor_area,
    context::{EventCx, PaintCx, UpdateCx},
    event::{Event, EventPropagation},
    peniko::{
        kurbo::{Point, Rect, Size},
        Color,
    },
    pointer::PointerButton,
    reactive::create_updater,
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
    View, ViewId,
};
use floem_renderer::Renderer;
use horizon::terminal::{
    TerminalCommand, TerminalFrame, TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers,
    TerminalMouseReport, TerminalScroll, TerminalSelectionPoint, TerminalSize,
};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f64 = 18.0;
const PADDING_X: f64 = 12.0;
const PADDING_Y: f64 = 12.0;
const TERMINAL_FONT_FAMILY: &str =
    "Iosevka Nerd Font Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP, monospace, Noto Sans CJK JP";

pub fn terminal_text_view(
    frame: impl Fn() -> TerminalFrame + 'static,
    preedit: impl Fn() -> Option<String> + 'static,
    terminal_tx: Option<Sender<TerminalCommand>>,
    window_origin: impl Fn() -> Point + 'static,
    update_ime_cursor_area: impl Fn(Point, Size) + 'static,
) -> TerminalTextView {
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

pub struct TerminalTextView {
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

#[derive(Clone, Copy, Debug)]
struct TerminalMetrics {
    cell_width: f64,
    line_height: f64,
}

struct CellLayout {
    text: TextLayout,
    columns: usize,
    fg: [u8; 3],
    bg: [u8; 3],
    block: Option<BlockElement>,
    visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockElement {
    Full,
    UpperFraction(u8),
    LowerFraction(u8),
    LeftFraction(u8),
    RightFraction(u8),
    Quadrants {
        upper_left: bool,
        upper_right: bool,
        lower_left: bool,
        lower_right: bool,
    },
}

struct PreeditLayout {
    text: TextLayout,
    columns: usize,
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
        self.frame = state.frame;
        self.lines = build_line_layouts(&self.frame);
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
                if cell.bg != [24, 27, 32] {
                    cx.fill(
                        &expanded_rect(bg_rect),
                        &Color::rgb8(cell.bg[0], cell.bg[1], cell.bg[2]),
                        0.0,
                    );
                }
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
                    cx.fill(&preedit_rect, &Color::rgb8(52, 58, 70), 0.0);
                    cx.draw_text(&preedit.text, Point::new(x, y));
                    let underline = Rect::new(
                        x,
                        y + line_height - 2.0,
                        x + preedit_width as f64 * cell_width,
                        y + line_height - 1.0,
                    );
                    cx.fill(&underline, &Color::rgb8(132, 220, 198), 0.0);
                } else {
                    let rect = Rect::new(x, y, x + cell_width, y + line_height);
                    cx.fill(&rect, &Color::rgba8(132, 220, 198, 150), 0.0);
                }
            }
        }

        cx.restore();
    }
}

const FALLBACK_CELL_WIDTH: f64 = 8.0;

impl TerminalTextView {
    fn resize_terminal(&mut self, cols: usize, rows: usize) {
        let size = TerminalSize {
            cols: cols.clamp(1, u16::MAX as usize) as u16,
            rows: rows.clamp(1, u16::MAX as usize) as u16,
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

impl Default for TerminalMetrics {
    fn default() -> Self {
        Self {
            cell_width: measured_cell_width(),
            line_height: LINE_HEIGHT,
        }
    }
}

fn cell_from_point(point: Point, metrics: TerminalMetrics) -> TerminalSelectionPoint {
    let col = ((point.x - PADDING_X) / metrics.cell_width)
        .max(0.0)
        .floor() as usize;
    let row = ((point.y - PADDING_Y) / metrics.line_height)
        .max(0.0)
        .floor() as usize;
    TerminalSelectionPoint { row, col }
}

fn scroll_lines_from_wheel(delta_y: f64) -> Option<i32> {
    if delta_y.abs() < f64::EPSILON {
        return None;
    }

    Some(if delta_y > 0.0 { 3 } else { -3 })
}

fn terminal_mouse_button(button: PointerButton) -> Option<TerminalMouseButton> {
    if button.is_primary() {
        Some(TerminalMouseButton::Left)
    } else if button.is_auxiliary() {
        Some(TerminalMouseButton::Middle)
    } else if button.is_secondary() {
        Some(TerminalMouseButton::Right)
    } else {
        None
    }
}

fn terminal_mouse_modifiers(modifiers: floem::keyboard::Modifiers) -> TerminalMouseModifiers {
    TerminalMouseModifiers {
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        control: modifiers.control(),
    }
}

fn build_line_layouts(frame: &TerminalFrame) -> Vec<Vec<CellLayout>> {
    let family = terminal_font_family();

    frame
        .lines
        .iter()
        .map(|line| {
            let mut cells = Vec::new();
            for span in &line.spans {
                cells.extend(build_span_cells(span, &family));
            }
            cells
        })
        .collect()
}

fn build_span_cells(
    span: &horizon::terminal::TerminalSpan,
    family: &[FamilyOwned],
) -> Vec<CellLayout> {
    if span.text.is_empty() {
        return (0..span.columns)
            .map(|_| empty_cell(span.bg))
            .collect::<Vec<_>>();
    }

    let mut cells = Vec::new();
    let mut current = String::new();
    let mut current_columns = 0_usize;

    for ch in span.text.chars() {
        let columns = char_columns(ch);
        if columns == 0 {
            if current.is_empty() {
                current.push(ch);
                current_columns = 1;
            } else {
                current.push(ch);
            }
            continue;
        }

        if !current.is_empty() {
            cells.push(text_cell(
                std::mem::take(&mut current),
                current_columns,
                span.fg,
                span.bg,
                family,
            ));
        }
        current.push(ch);
        current_columns = columns;
    }

    if !current.is_empty() {
        cells.push(text_cell(
            std::mem::take(&mut current),
            current_columns,
            span.fg,
            span.bg,
            family,
        ));
    }

    let used_columns = cells.iter().map(|cell| cell.columns).sum::<usize>();
    if used_columns < span.columns {
        cells.extend((used_columns..span.columns).map(|_| empty_cell(span.bg)));
    }

    cells
}

fn text_cell(
    text: String,
    columns: usize,
    fg: [u8; 3],
    bg: [u8; 3],
    family: &[FamilyOwned],
) -> CellLayout {
    let attrs = Attrs::new()
        .color(Color::rgb8(fg[0], fg[1], fg[2]))
        .family(family)
        .font_size(FONT_SIZE)
        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
    let mut layout = TextLayout::new();
    layout.set_text(&text, AttrsList::new(attrs));
    let block = block_element(text.as_str());
    CellLayout {
        text: layout,
        columns,
        fg,
        bg,
        block,
        visible: true,
    }
}

fn empty_cell(bg: [u8; 3]) -> CellLayout {
    CellLayout {
        text: TextLayout::new(),
        columns: 1,
        fg: [0, 0, 0],
        bg,
        block: None,
        visible: false,
    }
}

fn char_columns(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn block_element(text: &str) -> Option<BlockElement> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }

    match ch {
        '█' => Some(BlockElement::Full),
        '▔' => Some(BlockElement::UpperFraction(1)),
        '▀' => Some(BlockElement::UpperFraction(4)),
        '▁' => Some(BlockElement::LowerFraction(1)),
        '▂' => Some(BlockElement::LowerFraction(2)),
        '▃' => Some(BlockElement::LowerFraction(3)),
        '▄' => Some(BlockElement::LowerFraction(4)),
        '▅' => Some(BlockElement::LowerFraction(5)),
        '▆' => Some(BlockElement::LowerFraction(6)),
        '▇' => Some(BlockElement::LowerFraction(7)),
        '▏' => Some(BlockElement::LeftFraction(1)),
        '▎' => Some(BlockElement::LeftFraction(2)),
        '▍' => Some(BlockElement::LeftFraction(3)),
        '▌' => Some(BlockElement::LeftFraction(4)),
        '▋' => Some(BlockElement::LeftFraction(5)),
        '▊' => Some(BlockElement::LeftFraction(6)),
        '▉' => Some(BlockElement::LeftFraction(7)),
        '▐' => Some(BlockElement::RightFraction(4)),
        '▕' => Some(BlockElement::RightFraction(1)),
        '▖' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: false,
            lower_left: true,
            lower_right: false,
        }),
        '▗' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: false,
            lower_left: false,
            lower_right: true,
        }),
        '▘' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: false,
            lower_right: false,
        }),
        '▝' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: false,
            lower_right: false,
        }),
        '▚' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: false,
            lower_right: true,
        }),
        '▞' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: true,
            lower_right: false,
        }),
        '▙' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: true,
            lower_right: true,
        }),
        '▛' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: true,
            lower_left: true,
            lower_right: false,
        }),
        '▜' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: true,
            lower_left: false,
            lower_right: true,
        }),
        '▟' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: true,
            lower_right: true,
        }),
        _ => None,
    }
}

fn draw_block_element(cx: &mut PaintCx, block: BlockElement, cell_rect: Rect, fg: [u8; 3]) {
    let color = Color::rgb8(fg[0], fg[1], fg[2]);
    match block {
        BlockElement::Full => cx.fill(&expanded_rect(cell_rect), &color, 0.0),
        BlockElement::UpperFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y0,
                cell_rect.x1,
                cell_rect.y0 + cell_rect.height() * fraction(eighths),
            );
            cx.fill(&expanded_rect(rect), &color, 0.0);
        }
        BlockElement::LowerFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y1 - cell_rect.height() * fraction(eighths),
                cell_rect.x1,
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), &color, 0.0);
        }
        BlockElement::LeftFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y0,
                cell_rect.x0 + cell_rect.width() * fraction(eighths),
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), &color, 0.0);
        }
        BlockElement::RightFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x1 - cell_rect.width() * fraction(eighths),
                cell_rect.y0,
                cell_rect.x1,
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), &color, 0.0);
        }
        BlockElement::Quadrants {
            upper_left,
            upper_right,
            lower_left,
            lower_right,
        } => {
            let mid_x = midpoint(cell_rect.x0, cell_rect.x1);
            let mid_y = midpoint(cell_rect.y0, cell_rect.y1);
            if upper_left {
                cx.fill(
                    &expanded_rect(Rect::new(cell_rect.x0, cell_rect.y0, mid_x, mid_y)),
                    &color,
                    0.0,
                );
            }
            if upper_right {
                cx.fill(
                    &expanded_rect(Rect::new(mid_x, cell_rect.y0, cell_rect.x1, mid_y)),
                    &color,
                    0.0,
                );
            }
            if lower_left {
                cx.fill(
                    &expanded_rect(Rect::new(cell_rect.x0, mid_y, mid_x, cell_rect.y1)),
                    &color,
                    0.0,
                );
            }
            if lower_right {
                cx.fill(
                    &expanded_rect(Rect::new(mid_x, mid_y, cell_rect.x1, cell_rect.y1)),
                    &color,
                    0.0,
                );
            }
        }
    }
}

fn midpoint(start: f64, end: f64) -> f64 {
    start + (end - start) / 2.0
}

fn fraction(eighths: u8) -> f64 {
    eighths.clamp(1, 8) as f64 / 8.0
}

fn expanded_rect(rect: Rect) -> Rect {
    const OVERLAP: f64 = 0.5;
    Rect::new(
        rect.x0 - OVERLAP,
        rect.y0 - OVERLAP,
        rect.x1 + OVERLAP,
        rect.y1 + OVERLAP,
    )
}

fn build_preedit_layout(text: Option<&str>) -> Option<PreeditLayout> {
    let text = text.filter(|text| !text.is_empty())?;
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::rgb8(233, 236, 242))
        .family(&family)
        .font_size(FONT_SIZE)
        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
    let mut layout = TextLayout::new();
    layout.set_text(text, AttrsList::new(attrs));
    Some(PreeditLayout {
        text: layout,
        columns: UnicodeWidthStr::width(text),
    })
}

fn measured_cell_width() -> f64 {
    let sample = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::rgb8(233, 236, 242))
        .family(&family)
        .font_size(FONT_SIZE)
        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
    let mut layout = TextLayout::new();
    layout.set_text(sample, AttrsList::new(attrs));
    let width = layout.size().width / sample.len() as f64;

    if width.is_finite() && width > 1.0 {
        width
    } else {
        FALLBACK_CELL_WIDTH
    }
}

fn terminal_font_family() -> Vec<FamilyOwned> {
    FamilyOwned::parse_list(TERMINAL_FONT_FAMILY).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon::terminal::TerminalSpan;

    fn test_family() -> Vec<FamilyOwned> {
        terminal_font_family()
    }

    #[test]
    fn measured_cell_width_is_usable() {
        assert!(measured_cell_width() > 1.0);
    }

    #[test]
    fn cell_from_point_uses_terminal_metrics() {
        let metrics = TerminalMetrics {
            cell_width: 10.0,
            line_height: 20.0,
        };

        assert_eq!(
            cell_from_point(Point::new(PADDING_X + 21.0, PADDING_Y + 41.0), metrics),
            TerminalSelectionPoint { row: 2, col: 2 }
        );
    }

    #[test]
    fn span_cells_expand_spaces_as_invisible_cells() {
        let cells = build_span_cells(
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
        let cells = build_span_cells(
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
        let cells = build_span_cells(
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
                Some(BlockElement::Full),
                Some(BlockElement::LowerFraction(4)),
                Some(BlockElement::UpperFraction(4)),
            ]
        );
    }

    #[test]
    fn span_cells_mark_fractional_block_elements_for_rect_rendering() {
        let cells = build_span_cells(
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
                Some(BlockElement::LeftFraction(1)),
                Some(BlockElement::LeftFraction(2)),
                Some(BlockElement::LeftFraction(3)),
                Some(BlockElement::LeftFraction(4)),
                Some(BlockElement::LeftFraction(5)),
                Some(BlockElement::LeftFraction(6)),
                Some(BlockElement::LeftFraction(7)),
                Some(BlockElement::LowerFraction(1)),
                Some(BlockElement::LowerFraction(2)),
                Some(BlockElement::LowerFraction(3)),
                Some(BlockElement::LowerFraction(4)),
                Some(BlockElement::LowerFraction(5)),
                Some(BlockElement::LowerFraction(6)),
                Some(BlockElement::LowerFraction(7)),
            ]
        );
    }
}
