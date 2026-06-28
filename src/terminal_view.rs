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
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
    View, ViewId,
};
use floem_renderer::Renderer;
use horizon::terminal::{
    TerminalCommand, TerminalFrame, TerminalScroll, TerminalSelectionPoint, TerminalSize,
};
use unicode_width::UnicodeWidthStr;

const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f64 = 18.0;
const PADDING_X: f64 = 12.0;
const PADDING_Y: f64 = 12.0;

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
    lines: Vec<Vec<SpanLayout>>,
    preedit: Option<PreeditLayout>,
    terminal_tx: Option<Sender<TerminalCommand>>,
    last_size: Option<TerminalSize>,
    selecting: bool,
    window_origin: Box<dyn Fn() -> Point>,
    update_ime_cursor_area: Box<dyn Fn(Point, Size)>,
}

struct SpanLayout {
    text: TextLayout,
    columns: usize,
    bg: [u8; 3],
    visible: bool,
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
            terminal_tx,
            last_size: None,
            selecting: false,
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
            Event::PointerDown(pointer) if pointer.button.is_primary() => {
                self.selecting = true;
                self.send_selection_command(TerminalCommand::SelectionStart(cell_from_point(
                    pointer.pos,
                )));
                EventPropagation::Continue
            }
            Event::PointerMove(pointer) if self.selecting => {
                self.send_selection_command(TerminalCommand::SelectionUpdate(cell_from_point(
                    pointer.pos,
                )));
                EventPropagation::Stop
            }
            Event::PointerUp(pointer) if pointer.button.is_primary() && self.selecting => {
                self.selecting = false;
                self.send_selection_command(TerminalCommand::SelectionUpdate(cell_from_point(
                    pointer.pos,
                )));
                EventPropagation::Stop
            }
            Event::PointerWheel(pointer) => {
                if let Some(lines) = scroll_lines_from_wheel(pointer.delta.y) {
                    self.send_selection_command(TerminalCommand::Scroll(TerminalScroll {
                        lines,
                        point: cell_from_point(pointer.pos),
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
        let max_rows = ((height - PADDING_Y * 2.0) / LINE_HEIGHT).max(0.0).floor() as usize;
        let max_cols = ((width - PADDING_X * 2.0) / CELL_WIDTH).max(0.0).floor() as usize;
        self.resize_terminal(max_cols, max_rows);

        cx.save();
        cx.clip(&clip);

        for (row, spans) in self.lines.iter().take(max_rows).enumerate() {
            let y = PADDING_Y + row as f64 * LINE_HEIGHT;
            let mut x = PADDING_X;
            let mut col = 0_usize;
            for span in spans {
                if col >= max_cols {
                    break;
                }
                let columns = span.columns.min(max_cols - col);
                let bg_rect = Rect::new(x, y, x + columns as f64 * CELL_WIDTH, y + LINE_HEIGHT);
                if span.bg != [24, 27, 32] {
                    cx.fill(
                        &bg_rect,
                        &Color::rgb8(span.bg[0], span.bg[1], span.bg[2]),
                        0.0,
                    );
                }
                if span.visible {
                    cx.draw_text(&span.text, Point::new(x, y));
                }
                x += columns as f64 * CELL_WIDTH;
                col += columns;
            }
        }

        if let Some(cursor) = self.frame.cursor {
            if cursor.row < max_rows && cursor.col < max_cols {
                let x = PADDING_X + cursor.col as f64 * CELL_WIDTH;
                let y = PADDING_Y + cursor.row as f64 * LINE_HEIGHT;
                let ime_pos = (self.window_origin)() + Point::new(x, y + LINE_HEIGHT).to_vec2();
                let ime_size = Size::new(CELL_WIDTH, LINE_HEIGHT);
                (self.update_ime_cursor_area)(ime_pos, ime_size);
                set_ime_cursor_area(ime_pos, ime_size);
                if let Some(preedit) = &self.preedit {
                    let preedit_width = preedit.columns.max(1).min(max_cols - cursor.col);
                    let preedit_rect =
                        Rect::new(x, y, x + preedit_width as f64 * CELL_WIDTH, y + LINE_HEIGHT);
                    cx.fill(&preedit_rect, &Color::rgb8(52, 58, 70), 0.0);
                    cx.draw_text(&preedit.text, Point::new(x, y));
                    let underline = Rect::new(
                        x,
                        y + LINE_HEIGHT - 2.0,
                        x + preedit_width as f64 * CELL_WIDTH,
                        y + LINE_HEIGHT - 1.0,
                    );
                    cx.fill(&underline, &Color::rgb8(132, 220, 198), 0.0);
                } else {
                    let rect = Rect::new(x, y, x + CELL_WIDTH, y + LINE_HEIGHT);
                    cx.fill(&rect, &Color::rgba8(132, 220, 198, 150), 0.0);
                }
            }
        }

        cx.restore();
    }
}

const CELL_WIDTH: f64 = 8.0;

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

fn cell_from_point(point: Point) -> TerminalSelectionPoint {
    let col = ((point.x - PADDING_X) / CELL_WIDTH).max(0.0).floor() as usize;
    let row = ((point.y - PADDING_Y) / LINE_HEIGHT).max(0.0).floor() as usize;
    TerminalSelectionPoint { row, col }
}

fn scroll_lines_from_wheel(delta_y: f64) -> Option<i32> {
    if delta_y.abs() < f64::EPSILON {
        return None;
    }

    Some(if delta_y > 0.0 { 3 } else { -3 })
}

fn build_line_layouts(frame: &TerminalFrame) -> Vec<Vec<SpanLayout>> {
    let family: Vec<FamilyOwned> =
        FamilyOwned::parse_list("Noto Sans Mono CJK JP, monospace, Noto Sans CJK JP").collect();

    frame
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| {
                    let attrs = Attrs::new()
                        .color(Color::rgb8(span.fg[0], span.fg[1], span.fg[2]))
                        .family(&family)
                        .font_size(FONT_SIZE)
                        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
                    let mut layout = TextLayout::new();
                    layout.set_text(&span.text, AttrsList::new(attrs));
                    SpanLayout {
                        columns: span.columns,
                        bg: span.bg,
                        visible: !span.text.is_empty(),
                        text: layout,
                    }
                })
                .collect()
        })
        .collect()
}

fn build_preedit_layout(text: Option<&str>) -> Option<PreeditLayout> {
    let text = text.filter(|text| !text.is_empty())?;
    let family: Vec<FamilyOwned> =
        FamilyOwned::parse_list("Noto Sans Mono CJK JP, monospace, Noto Sans CJK JP").collect();
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
