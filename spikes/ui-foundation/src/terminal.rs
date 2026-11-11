//! A terminal on this stack: a real shell's frames drawn by a widget, and
//! the keyboard, the input method, the clipboard, the mouse and the box's
//! size taken to it.
//!
//! The session ([`crate::pty`]) is Horizon's terminal core over a PTY. What
//! this view does is what `src/terminal/` does over GPUI: it turns the
//! document's events into the core's commands and paints the core's frames.
//! A frame's rows are shaped a row at a time, a row again only when it
//! differs from the one drawn before, and every glyph is put at its cell's
//! place rather than where the font's own advances would put it, so a row
//! holding wide characters stays on the grid.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyrender::PaintScene;
use blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, DomEventData};
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, NamedColor, TerminalColor, TerminalCommand,
    TerminalCursorShape, TerminalFrame, TerminalKeyInput, TerminalLine, TerminalMouseButton,
    TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport, TerminalScroll,
    TerminalSelectionKind, TerminalSelectionPoint, TerminalSize, TerminalUpdate, fixed_palette_rgb,
};
use keyboard_types::{Key, NamedKey};
use kurbo::{Affine, Rect, Stroke};
use parley::{Layout, PositionedLayoutItem};
use peniko::{Color, Fill};
use termwiz::input::{KeyCode, Modifiers};
use unicode_width::UnicodeWidthChar;

use crate::pty::Session;
use crate::term::{Ink, LINE_HEIGHT, Row, Shaper};
use crate::{El, each_layout, el, later, prevent_default};

const FOREGROUND: [u8; 3] = [0x20, 0x20, 0x20];
const BACKGROUND: [u8; 3] = [0xfd, 0xf6, 0xe3];
const CURSOR: [u8; 3] = [0x35, 0x6d, 0xf3];
const SELECTION: [u8; 3] = [0xc9, 0xd8, 0xfb];
/// The sixteen colors a program names by number: eight, then their bright
/// forms.
const PALETTE: [[u8; 3]; 16] = [
    [0x20, 0x20, 0x20],
    [0xc0, 0x39, 0x2b],
    [0x2e, 0x8b, 0x3d],
    [0xa0, 0x78, 0x00],
    [0x20, 0x60, 0xc0],
    [0x8e, 0x44, 0xad],
    [0x16, 0x86, 0x8a],
    [0xb8, 0xb8, 0xb8],
    [0x60, 0x60, 0x60],
    [0xe0, 0x50, 0x40],
    [0x3a, 0xa8, 0x50],
    [0xc0, 0x9a, 0x10],
    [0x3d, 0x7d, 0xe0],
    [0xa5, 0x5f, 0xc0],
    [0x22, 0xa0, 0xa4],
    [0xe8, 0xe8, 0xe8],
];

pub const CSS: &str = "
    .terminal { width: 100%; height: 100%; outline: none; }
";

fn dim(color: [u8; 3]) -> [u8; 3] {
    std::array::from_fn(|channel| ((color[channel] as u16 + BACKGROUND[channel] as u16) / 2) as u8)
}

fn rgb(color: TerminalColor) -> [u8; 3] {
    use NamedColor::*;
    match color {
        TerminalColor::Rgb(rgb) => rgb,
        TerminalColor::Indexed(index) if index < 16 => PALETTE[index as usize],
        TerminalColor::Indexed(index) => fixed_palette_rgb(index).unwrap_or(FOREGROUND),
        TerminalColor::Named(named) => match named {
            Foreground | BrightForeground => FOREGROUND,
            DimForeground => dim(FOREGROUND),
            Background => BACKGROUND,
            Cursor => CURSOR,
            Black | Red | Green | Yellow | Blue | Magenta | Cyan | White | BrightBlack
            | BrightRed | BrightGreen | BrightYellow | BrightBlue | BrightMagenta | BrightCyan
            | BrightWhite => PALETTE[named.override_index().unwrap_or(0) as usize],
            DimBlack => dim(PALETTE[0]),
            DimRed => dim(PALETTE[1]),
            DimGreen => dim(PALETTE[2]),
            DimYellow => dim(PALETTE[3]),
            DimBlue => dim(PALETTE[4]),
            DimMagenta => dim(PALETTE[5]),
            DimCyan => dim(PALETTE[6]),
            DimWhite => dim(PALETTE[7]),
        },
    }
}

/// What the view and its widget share.
struct State {
    session: Session,
    /// The frame on screen.
    frame: RefCell<Option<TerminalFrame>>,
    /// A cell's width in CSS pixels, and how many columns and rows fit.
    cells: Cell<(f64, usize, usize)>,
    /// What the input method is composing, drawn at the cursor.
    preedit: RefCell<String>,
    focused: Cell<bool>,
    exited: Cell<bool>,
    /// What the session put on the clipboard, until the shell is given it.
    copied: RefCell<Option<String>>,
    /// The part of a wheel's turn not yet a whole row.
    wheel: Cell<f64>,
    /// Whether a button went down here and is still held, and whether what
    /// it drags is a selection (or, otherwise, a report to the program).
    dragging: Cell<Option<bool>>,
}

/// The view, for whoever placed it.
#[derive(Clone)]
pub struct Terminal {
    state: Rc<State>,
    pub element: El,
}

impl Terminal {
    /// Takes in what the session has produced since the last call: the
    /// newest frame, and its updates. Whether anything came.
    pub fn pump(&self) -> bool {
        pump(&self.state)
    }

    /// The screen's rows, as text. A span's cells past its text are blank.
    pub fn text(&self) -> String {
        let frame = self.state.frame.borrow();
        let lines = frame.iter().flat_map(|frame| &frame.lines);
        let row = |line: &TerminalLine| {
            let mut row = String::new();
            for span in &line.spans {
                let width: usize = span.text.chars().filter_map(|c| c.width()).sum();
                row += &span.text;
                row += &" ".repeat(span.columns.saturating_sub(width));
            }
            row.trim_end().to_string()
        };
        lines.map(row).collect::<Vec<_>>().join("\n")
    }

    pub fn frame(&self) -> Option<TerminalFrame> {
        self.state.frame.borrow().clone()
    }

    /// Columns and rows.
    pub fn size(&self) -> (usize, usize) {
        let (_, columns, rows) = self.state.cells.get();
        (columns, rows)
    }

    /// What the input method is composing.
    pub fn preedit(&self) -> String {
        self.state.preedit.borrow().clone()
    }

    pub fn exited(&self) -> bool {
        self.state.exited.get()
    }

    /// What the session last copied, taken.
    pub fn take_copied(&self) -> Option<String> {
        self.state.copied.borrow_mut().take()
    }

    /// Who to wake when the session has a frame.
    pub fn set_waker(&self, waker: std::task::Waker) {
        self.state.session.set_waker(waker);
    }
}

fn pump(state: &State) -> bool {
    let mut came = false;
    if let Some(frame) = state.session.take_frame() {
        *state.frame.borrow_mut() = Some(frame);
        came = true;
    }
    for update in state.session.take_updates() {
        came = true;
        match update {
            TerminalUpdate::Clipboard {
                text,
                destination: ClipboardDestination::Clipboard,
            } => *state.copied.borrow_mut() = Some(text),
            TerminalUpdate::Exited => state.exited.set(true),
            _ => {}
        }
    }
    came
}

fn modifiers(key: &BlitzKeyEvent) -> Modifiers {
    let mut modifiers = Modifiers::NONE;
    if key.modifiers.shift() {
        modifiers |= Modifiers::SHIFT;
    }
    if key.modifiers.ctrl() {
        modifiers |= Modifiers::CTRL;
    }
    if key.modifiers.alt() {
        modifiers |= Modifiers::ALT;
    }
    if key.modifiers.meta() {
        modifiers |= Modifiers::SUPER;
    }
    modifiers
}

/// The key a key event is, in the core's vocabulary, and the text it made.
fn key_code(key: &BlitzKeyEvent) -> Option<(KeyCode, Option<String>)> {
    let named = |code| Some((code, None));
    match &key.key {
        Key::Character(text) => {
            let mut characters = text.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            // A chord's key is the letter, not what Shift made of it.
            let chord = key.modifiers.ctrl() || key.modifiers.alt();
            let character = match chord {
                true => character.to_ascii_lowercase(),
                false => character,
            };
            Some((KeyCode::Char(character), (!chord).then(|| text.to_string())))
        }
        Key::Named(key) => match key {
            NamedKey::Enter => named(KeyCode::Enter),
            NamedKey::Tab => named(KeyCode::Tab),
            NamedKey::Backspace => named(KeyCode::Backspace),
            NamedKey::Escape => named(KeyCode::Escape),
            NamedKey::Delete => named(KeyCode::Delete),
            NamedKey::Insert => named(KeyCode::Insert),
            NamedKey::ArrowLeft => named(KeyCode::LeftArrow),
            NamedKey::ArrowRight => named(KeyCode::RightArrow),
            NamedKey::ArrowUp => named(KeyCode::UpArrow),
            NamedKey::ArrowDown => named(KeyCode::DownArrow),
            NamedKey::Home => named(KeyCode::Home),
            NamedKey::End => named(KeyCode::End),
            NamedKey::PageUp => named(KeyCode::PageUp),
            NamedKey::PageDown => named(KeyCode::PageDown),
            NamedKey::F1 => named(KeyCode::Function(1)),
            NamedKey::F2 => named(KeyCode::Function(2)),
            NamedKey::F3 => named(KeyCode::Function(3)),
            NamedKey::F4 => named(KeyCode::Function(4)),
            NamedKey::F5 => named(KeyCode::Function(5)),
            NamedKey::F6 => named(KeyCode::Function(6)),
            NamedKey::F7 => named(KeyCode::Function(7)),
            NamedKey::F8 => named(KeyCode::Function(8)),
            NamedKey::F9 => named(KeyCode::Function(9)),
            NamedKey::F10 => named(KeyCode::Function(10)),
            NamedKey::F11 => named(KeyCode::Function(11)),
            NamedKey::F12 => named(KeyCode::Function(12)),
            _ => None,
        },
    }
}

/// Builds the view over `session`.
pub fn view(session: Session) -> (El, Terminal) {
    let state = Rc::new(State {
        session,
        frame: RefCell::new(None),
        cells: Cell::new((1., 0, 0)),
        preedit: RefCell::new(String::new()),
        focused: Cell::new(false),
        exited: Cell::new(false),
        copied: RefCell::new(None),
        wheel: Cell::new(0.),
        dragging: Cell::new(None),
    });
    let element = el("div").class("terminal").attr("tabindex", "0");

    // The cell under a point of the element's box.
    let cell_at = {
        let state = state.clone();
        move |x: f32, y: f32| {
            let (width, columns, rows) = state.cells.get();
            TerminalSelectionPoint {
                row: ((y as f64 / LINE_HEIGHT).max(0.) as usize).min(rows.saturating_sub(1)),
                col: ((x as f64 / width).max(0.) as usize).min(columns.saturating_sub(1)),
            }
        }
    };
    let reports = {
        let state = state.clone();
        move || (state.frame.borrow().as_ref()).is_some_and(|frame| frame.mouse_reporting)
    };

    let (keyed, released, composed, pasted) =
        (state.clone(), state.clone(), state.clone(), state.clone());
    let (pressed, moved, lifted, wheeled) =
        (state.clone(), state.clone(), state.clone(), state.clone());
    let (cell_down, cell_move, cell_up, cell_wheel) =
        (cell_at.clone(), cell_at.clone(), cell_at.clone(), cell_at);
    let reports_down = reports;
    let area = state.clone();
    element
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            // While the input method composes, the keys are its own.
            if key.is_composing || !keyed.preedit.borrow().is_empty() {
                return;
            }
            let character = match &key.key {
                Key::Character(character) => character.to_lowercase(),
                _ => String::new(),
            };
            // Copy and paste are the view's, on the chords a terminal
            // leaves the program its Ctrl+C and Ctrl+V with.
            if key.modifiers.ctrl() && key.modifiers.shift() && character == "c" {
                keyed.session.send(TerminalCommand::CopySelection);
                prevent_default();
                return;
            }
            if key.modifiers.ctrl() && key.modifiers.shift() && character == "v" {
                later(|document| {
                    let _ = document.shell_provider.request_clipboard_text();
                });
                prevent_default();
                return;
            }
            let Some((code, text)) = key_code(key) else {
                return;
            };
            keyed
                .session
                .send(TerminalCommand::KeyInput(TerminalKeyInput {
                    key: code,
                    modifiers: modifiers(key),
                    kind: match key.is_auto_repeating {
                        true => KeyEventKind::Repeat,
                        false => KeyEventKind::Press,
                    },
                    text,
                }));
            // Not the document's Tab to the next element, nor its scroll.
            prevent_default();
        })
        .on("keyup", move |event| {
            let DomEventData::KeyUp(key) = &event.data else {
                return;
            };
            if let Some((code, _)) = key_code(key) {
                released
                    .session
                    .send(TerminalCommand::KeyInput(TerminalKeyInput {
                        key: code,
                        modifiers: modifiers(key),
                        kind: KeyEventKind::Release,
                        text: None,
                    }));
            }
        })
        .on("composition", move |event| {
            let DomEventData::Ime(ime) = &event.data else {
                return;
            };
            match ime {
                BlitzImeEvent::Preedit(text, _) => *composed.preedit.borrow_mut() = text.clone(),
                BlitzImeEvent::Commit(text) => {
                    composed.preedit.borrow_mut().clear();
                    composed
                        .session
                        .send(TerminalCommand::TextInput(text.clone()));
                }
                _ => composed.preedit.borrow_mut().clear(),
            }
        })
        .on_paste(move |text| {
            pasted
                .session
                .send(TerminalCommand::Paste(text.to_string()))
        })
        .on("pointerdown", move |event| {
            let DomEventData::PointerDown(pointer) = &event.data else {
                return;
            };
            let point = cell_down(pointer.element.x, pointer.element.y);
            // A program that asked for the mouse gets it, unless Shift
            // keeps the press for a selection.
            let selects = !reports_down() || pointer.mods.shift();
            pressed.dragging.set(Some(selects));
            pressed.session.send(match selects {
                true => TerminalCommand::SelectionStart {
                    point,
                    kind: TerminalSelectionKind::Simple,
                },
                false => TerminalCommand::Mouse(report(TerminalMouseKind::Press, point)),
            });
        })
        .on("pointermove", move |event| {
            let DomEventData::PointerMove(pointer) = &event.data else {
                return;
            };
            let Some(selects) = moved.dragging.get() else {
                return;
            };
            let point = cell_move(pointer.element.x, pointer.element.y);
            moved.session.send(match selects {
                true => TerminalCommand::SelectionUpdate(point),
                false => TerminalCommand::Mouse(report(TerminalMouseKind::Drag, point)),
            });
        })
        .on("pointerup", move |event| {
            let DomEventData::PointerUp(pointer) = &event.data else {
                return;
            };
            if lifted.dragging.take() == Some(false) {
                let point = cell_up(pointer.element.x, pointer.element.y);
                lifted.session.send(TerminalCommand::Mouse(report(
                    TerminalMouseKind::Release,
                    point,
                )));
            }
        })
        .on("wheel", move |event| {
            let DomEventData::Wheel(wheel) = &event.data else {
                return;
            };
            let turned = match wheel.delta {
                BlitzWheelDelta::Lines(_, lines) => lines * 3.,
                BlitzWheelDelta::Pixels(_, pixels) => pixels / LINE_HEIGHT,
            };
            let turned = wheeled.wheel.get() + turned;
            let lines = turned.trunc();
            wheeled.wheel.set(turned - lines);
            if lines != 0. {
                wheeled
                    .session
                    .send(TerminalCommand::Scroll(TerminalScroll {
                        lines: lines as i32,
                        point: cell_wheel(wheel.element.x, wheel.element.y),
                    }));
            }
            prevent_default();
        })
        // The input method's candidates go by the cursor's cell.
        .takes_text(move || {
            let (width, ..) = area.cells.get();
            let frame = area.frame.borrow();
            let cursor = frame.as_ref()?.cursor?;
            Some((
                (cursor.col as f64 * width) as f32,
                (cursor.row as f64 * LINE_HEIGHT) as f32,
                width as f32,
                LINE_HEIGHT as f32,
            ))
        })
        .widget(Screen {
            state: state.clone(),
            shaper: Shaper::default(),
            rows: Vec::new(),
            shaped_at: 0.,
        });

    // The session's frames and updates come in with every layout, and the
    // box's size in cells goes out when it changes.
    let (sized, mut shaper) = (state.clone(), Shaper::default());
    let cell_width = shaper.cell_width(1.);
    each_layout(move |document| {
        pump(&sized);
        // Whether the keys come here: asked of the document, since a focus
        // it was given from code is not announced by an event.
        let focused = document.get_focussed_node_id() == Some(element.0);
        if focused != sized.focused.replace(focused) {
            if !focused {
                sized.preedit.borrow_mut().clear();
            }
            sized.session.send(TerminalCommand::Focus(focused));
        }
        if let Some(text) = sized.copied.borrow().clone() {
            let _ = document.shell_provider.set_clipboard_text(text);
        }
        let Some(node) = document.get_node(element.0) else {
            return;
        };
        let size = node.final_layout().size;
        let columns = (size.width as f64 / cell_width) as usize;
        let rows = (size.height as f64 / LINE_HEIGHT) as usize;
        if (columns, rows) != (sized.cells.get().1, sized.cells.get().2) && columns * rows > 0 {
            sized.cells.set((cell_width, columns, rows));
            (sized.session).send(TerminalCommand::Resize(TerminalSize::new(
                columns as u16,
                rows as u16,
            )));
        }
    });
    (element, Terminal { state, element })
}

fn report(kind: TerminalMouseKind, point: TerminalSelectionPoint) -> TerminalMouseReport {
    TerminalMouseReport {
        kind,
        button: TerminalMouseButton::Left,
        point,
        modifiers: TerminalMouseModifiers::default(),
    }
}

/// One row as drawn: the line it was shaped from, and for each character
/// the column it starts at.
struct Shaped {
    line: TerminalLine,
    row: Row,
    layout: Layout<Ink>,
    /// The byte each character starts at, and its column.
    columns: Vec<(usize, usize)>,
}

fn shape(line: &TerminalLine, shaper: &mut Shaper, scale: f32) -> Shaped {
    let (mut text, mut colors, mut backgrounds, mut columns) =
        (String::new(), Vec::new(), Vec::new(), Vec::new());
    let mut column = 0;
    for span in &line.spans {
        let start = text.len();
        for character in span.text.chars() {
            columns.push((text.len(), column));
            text.push(character);
            column += character.width().unwrap_or(0);
        }
        colors.push((start..text.len(), Ink(rgb(span.fg))));
        // The frame's count of the span's cells is what places the next
        // span, whatever the characters' own widths came to.
        let first = columns.iter().rev().find(|(byte, _)| *byte <= start);
        let from = first.map_or(column, |(_, column)| *column);
        let from = if text.len() == start { column } else { from };
        column = from + span.columns;
        let background = rgb(span.bg);
        if background != BACKGROUND {
            backgrounds.push((from..column, background));
        }
    }
    let row = Row {
        text,
        colors,
        backgrounds,
    };
    Shaped {
        layout: shaper.shape(&row, scale),
        line: line.clone(),
        row,
        columns,
    }
}

/// The frame, painted into the element's box.
struct Screen {
    state: Rc<State>,
    shaper: Shaper,
    rows: Vec<Shaped>,
    /// The scale the rows were shaped at.
    shaped_at: f64,
}

fn fill(scene: &mut impl PaintScene, color: [u8; 3], rect: Rect) {
    let [r, g, b] = color;
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(r, g, b),
        None,
        &rect,
    );
}

/// Draws a shaped row with its top at `top`, each character's glyphs at
/// its column.
fn glyphs(
    scene: &mut impl PaintScene,
    shaped: &Shaped,
    columns: &[(usize, usize)],
    cell: f64,
    left: f64,
    top: f64,
) {
    let column_of = |byte: usize| {
        let at = columns.partition_point(|(start, _)| *start <= byte);
        columns[at.saturating_sub(1)].1
    };
    for item in shaped.layout.lines().flat_map(|line| line.items()) {
        let PositionedLayoutItem::GlyphRun(run) = item else {
            continue;
        };
        let [r, g, b] = run.style().brush.0;
        let (font, baseline, style) = (run.run(), run.baseline(), run.style_index());
        let placed = (font.clusters())
            .filter(|cluster| cluster.style_index() == style && !columns.is_empty())
            .flat_map(|cluster| {
                let x = left + column_of(cluster.text_range().start) as f64 * cell;
                let mut pen = 0.;
                (cluster.glyphs().collect::<Vec<_>>())
                    .into_iter()
                    .map(move |glyph| {
                        let placed = anyrender::Glyph {
                            id: glyph.id as _,
                            x: x as f32 + pen + glyph.x,
                            y: baseline + glyph.y,
                        };
                        pen += glyph.advance;
                        placed
                    })
            });
        scene.draw_glyphs(
            font.font(),
            font.font_size(),
            true,
            &[],
            kurbo::Vec2::default(),
            Fill::NonZero,
            &anyrender::Paint::from(Color::from_rgb8(r, g, b)),
            1.0,
            Affine::translate((0., top)),
            None,
            placed,
        );
    }
}

impl blitz_dom::Widget for Screen {
    fn paint(
        &mut self,
        _context: &mut dyn anyrender::RenderContext,
        _styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        fill(
            &mut scene,
            BACKGROUND,
            Rect::new(0., 0., width as f64, height as f64),
        );
        let frame = self.state.frame.borrow();
        let Some(frame) = frame.as_ref() else {
            return scene;
        };
        let (cell, line) = (self.state.cells.get().0 * scale, LINE_HEIGHT * scale);

        // A row is shaped again only when it is not the row drawn before.
        if self.shaped_at != scale {
            self.rows.clear();
            self.shaped_at = scale;
        }
        self.rows.truncate(frame.lines.len());
        for (index, wanted) in frame.lines.iter().enumerate() {
            match self.rows.get(index) {
                Some(shaped) if shaped.line == *wanted => {}
                Some(_) => self.rows[index] = shape(wanted, &mut self.shaper, scale as f32),
                None => (self.rows).push(shape(wanted, &mut self.shaper, scale as f32)),
            }
        }

        let cells = |row: usize, from: usize, to: usize| {
            Rect::new(
                from as f64 * cell,
                row as f64 * line,
                to as f64 * cell,
                (row + 1) as f64 * line,
            )
        };
        for (index, shaped) in self.rows.iter().enumerate() {
            for (columns, color) in &shaped.row.backgrounds {
                fill(&mut scene, *color, cells(index, columns.start, columns.end));
            }
        }
        // The selection: whole rows between its ends, both inclusive.
        if let Some(selection) = &frame.selection {
            let columns = self.state.cells.get().1;
            for row in selection.start.row..=selection.end.row {
                let from = match row == selection.start.row {
                    true => selection.start.col,
                    false => 0,
                };
                let to = match row == selection.end.row {
                    true => selection.end.col + 1,
                    false => columns,
                };
                fill(&mut scene, SELECTION, cells(row, from, to));
            }
        }
        for (index, shaped) in self.rows.iter().enumerate() {
            glyphs(
                &mut scene,
                shaped,
                &shaped.columns,
                cell,
                0.,
                index as f64 * line,
            );
        }

        if let Some(cursor) = frame.cursor {
            let place = cells(cursor.row, cursor.col, cursor.col + 1);
            let [r, g, b] = CURSOR;
            let preedit = self.state.preedit.borrow();
            if !preedit.is_empty() {
                // What is being composed, over the cells from the cursor,
                // underlined.
                let composing = TerminalLine {
                    spans: vec![horizon_terminal_core::TerminalSpan {
                        text: preedit.clone(),
                        columns: preedit.chars().filter_map(|c| c.width()).sum(),
                        fg: TerminalColor::Named(NamedColor::Foreground),
                        bg: TerminalColor::Named(NamedColor::Background),
                        italic: false,
                        strikethrough: false,
                        underline: Default::default(),
                        underline_color: None,
                        url: None,
                    }],
                };
                let shaped = shape(&composing, &mut self.shaper, scale as f32);
                let wide = shaped.line.spans[0].columns;
                let over = cells(cursor.row, cursor.col, cursor.col + wide);
                fill(&mut scene, BACKGROUND, over);
                glyphs(
                    &mut scene,
                    &shaped,
                    &shaped.columns,
                    cell,
                    place.x0,
                    place.y0,
                );
                let under = Rect::new(over.x0, over.y1 - 1.5 * scale, over.x1, over.y1);
                fill(&mut scene, CURSOR, under);
            } else if !self.state.focused.get() {
                // Without the focus, the cursor's outline.
                scene.stroke(
                    &Stroke::new(scale),
                    Affine::IDENTITY,
                    Color::from_rgb8(r, g, b),
                    None,
                    &place.inset(-scale / 2.),
                );
            } else {
                let shape = match cursor.shape {
                    TerminalCursorShape::Underline => {
                        Rect::new(place.x0, place.y1 - 2. * scale, place.x1, place.y1)
                    }
                    TerminalCursorShape::Beam => {
                        Rect::new(place.x0, place.y0, place.x0 + 2. * scale, place.y1)
                    }
                    _ => place,
                };
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(r, g, b, 0x80),
                    None,
                    &shape,
                );
            }
        }
        scene
    }
}
