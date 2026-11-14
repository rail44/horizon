//! A terminal on this stack: a real shell's frames drawn by a widget, and
//! the keyboard, the input method, the clipboard, the mouse and the box's
//! size taken to it.
//!
//! The session ([`Session`]) is one of `horizon-terminald`'s. This view turns
//! the document's events into the terminal core's commands and paints its
//! frames.
//! A frame's rows are shaped a row at a time, a row again only when it
//! differs from the one drawn before, and every glyph is put at its cell's
//! place rather than where the font's own advances would put it, so a row
//! holding wide characters stays on the grid.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyrender::PaintScene;
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, NamedColor, TerminalColor, TerminalColorScheme,
    TerminalCommand, TerminalCursorShape, TerminalFrame, TerminalKeyInput, TerminalLine,
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
    TerminalScroll, TerminalSelectionKind, TerminalSelectionPoint, TerminalSize, TerminalUpdate,
    fixed_palette_rgb,
};
use horizon_ui::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, DomEventData, MouseEventButton,
};
use keyboard_types::{Key, NamedKey};
use kurbo::{Affine, Rect, Stroke};
use parley::{Layout, PositionedLayoutItem};
use peniko::{Color, Fill};
use termwiz::input::{KeyCode, Modifiers};
use unicode_width::UnicodeWidthChar;

use horizon_shell_runtime::terminal_glyphs::{self as drawn_glyphs, GlyphShape};
use horizon_shell_runtime::terminal_links::{is_openable_url, url_from_lines};
use horizon_shell_runtime::terminal_scrollback::{ScrollIpc, Scrollback};
use horizon_theme_css::TerminalLook;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Component, El, copy, each_layout, el, prevent_default};

pub mod plugin;
mod session;
mod shaping;

pub use session::Session;
use shaping::{Ink, Row, Shaper};

mod styles {
    horizon_ui::css_module! {"
    .terminal { width: 100%; height: 100%; outline: none; }
"}
}

fn dim(color: [u8; 3], toward: [u8; 3]) -> [u8; 3] {
    std::array::from_fn(|channel| ((color[channel] as u16 + toward[channel] as u16) / 2) as u8)
}

/// A frame's logical color in the theme's.
fn rgb(color: TerminalColor, look: &TerminalLook) -> [u8; 3] {
    use NamedColor::*;
    let dim = |color| dim(color, look.background);
    match color {
        TerminalColor::Rgb(rgb) => rgb,
        TerminalColor::Indexed(index) if index < 16 => look.ansi[index as usize],
        TerminalColor::Indexed(index) => fixed_palette_rgb(index).unwrap_or(look.foreground),
        TerminalColor::Named(named) => match named {
            Foreground | BrightForeground => look.foreground,
            DimForeground => dim(look.foreground),
            Background => look.background,
            Cursor => look.cursor,
            Black | Red | Green | Yellow | Blue | Magenta | Cyan | White | BrightBlack
            | BrightRed | BrightGreen | BrightYellow | BrightBlue | BrightMagenta | BrightCyan
            | BrightWhite => look.ansi[named.override_index().unwrap_or(0) as usize],
            DimBlack => dim(look.ansi[0]),
            DimRed => dim(look.ansi[1]),
            DimGreen => dim(look.ansi[2]),
            DimYellow => dim(look.ansi[3]),
            DimBlue => dim(look.ansi[4]),
            DimMagenta => dim(look.ansi[5]),
            DimCyan => dim(look.ansi[6]),
            DimWhite => dim(look.ansi[7]),
        },
    }
}

/// Hands a URL to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;

/// What the view and its widget share.
struct State {
    /// Shared with whoever placed the view: the session outlives a view
    /// of it that is closed.
    session: Rc<Session>,
    /// The theme's colors, font and row height, and how many times they
    /// have been given.
    look: RefCell<TerminalLook>,
    restyled: Cell<u64>,
    /// The frame on screen, and its number among the session's.
    frame: RefCell<Option<TerminalFrame>>,
    seen: Cell<u64>,
    /// A cell's width in CSS pixels, and how many columns and rows fit.
    cells: Cell<(f64, usize, usize)>,
    /// Where the box is on the page, in CSS pixels: on a display that is
    /// not at one device pixel to the CSS pixel, the box may start part of
    /// a device pixel in, which what is drawn to whole device pixels
    /// allows for.
    origin: Cell<(f64, f64)>,
    /// What the input method is composing, drawn at the cursor.
    preedit: RefCell<String>,
    focused: Cell<bool>,
    /// What the session last copied, for whoever placed the view to see,
    /// and what it copied that the clipboard has not been given yet.
    copied: RefCell<Option<String>>,
    outgoing: RefCell<Option<String>>,
    /// The history being scrolled through, when the wheel is the
    /// terminal's own and not the program's: a window of it held here and
    /// moved over row by row and part of a row, with no word to the
    /// daemon until its edge is near (`docs/terminal-scrollback-design.md`).
    scrollback: RefCell<Scrollback>,
    /// The part of a wheel's turn not yet a whole row, for a wheel that
    /// is the program's.
    wheel: Cell<f64>,
    /// The last press that began a selection: when, at which cell, and
    /// how many presses in a row it made.
    clicked: Cell<Option<(std::time::Instant, TerminalSelectionPoint, u8)>>,
    /// The button a press reported to the program is for.
    reporting: Cell<Option<TerminalMouseButton>>,
    /// What a link clicked with the platform's key held is opened with.
    open_url: Option<OpenUrl>,
    /// Whether a button went down here and is still held, and whether what
    /// it drags is a selection (or, otherwise, a report to the program).
    dragging: Cell<Option<bool>>,
}

/// A terminal session, drawn and typed into.
///
/// The session is whoever placed the view's, and outlives it. What the
/// session says of itself (its title, its end) is read from the session
/// (`Session::take_reports`), not from the view.
pub struct TerminalView {
    pub session: Rc<Session>,
    /// The theme's colors, font and row height: the view follows it, and
    /// has the program that asks what the colors are answered with its.
    pub look: Signal<TerminalLook>,
    /// Opens a link clicked with the platform's key held.
    pub open_url: Option<OpenUrl>,
}

impl Component for TerminalView {
    fn view(self) -> El {
        build(self).0
    }
}

/// A way into the view for what is not a prop yet: the shell's commands
/// on it, and its tests' look at what it has drawn.
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

    /// The rows on screen, as text: the live screen's, or while the view
    /// is scrolled back the rows of history it is over. A span's cells
    /// past its text are blank.
    pub fn text(&self) -> String {
        let frame = self.state.frame.borrow();
        let scrolled = (self.state.scrollback.borrow()).visible_lines(self.state.cells.get().2);
        let history = (scrolled.as_ref()).map(|(window, range, _)| &window.lines[range.clone()]);
        let live = frame.as_ref().map(|frame| &frame.lines[..]);
        let lines = history.or(live).into_iter().flatten();
        let row = |line: &TerminalLine| {
            let mut row = String::new();
            for span in &line.spans {
                // A tab's cell holds the tab: a blank on the screen.
                let shown: String = (span.text.chars())
                    .map(|character| match character.is_control() {
                        true => ' ',
                        false => character,
                    })
                    .collect();
                let width: usize = shown.chars().filter_map(|c| c.width()).sum();
                row += &shown;
                row += &" ".repeat(span.columns.saturating_sub(width));
            }
            row.trim_end().to_string()
        };
        lines.map(row).collect::<Vec<_>>().join("\n")
    }

    /// How far into its top row the view is scrolled, as a part of a row,
    /// while it is scrolled back.
    pub fn scrolled_into(&self) -> Option<f32> {
        (self.state.scrollback.borrow())
            .visible_lines(self.state.cells.get().2)
            .map(|(.., into)| into)
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

    /// The number, among the session's, of the frame on screen: it moves
    /// when a frame is taken in.
    pub fn frame_number(&self) -> u64 {
        self.state.seen.get()
    }

    /// Asks the session to copy what is selected.
    pub fn copy_selection(&self) {
        self.state.session.send(TerminalCommand::CopySelection);
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
    if let Some((number, frame)) = state.session.frame_after(state.seen.get()) {
        // A program that takes the screen takes it from the history too.
        (state.scrollback.borrow_mut()).on_live_frame(frame.scrollback_available);
        *state.frame.borrow_mut() = Some(frame);
        state.seen.set(number);
        came = true;
    }
    for update in state.session.take_updates() {
        came = true;
        match update {
            TerminalUpdate::Clipboard {
                text,
                destination: ClipboardDestination::Clipboard,
            } => {
                *state.outgoing.borrow_mut() = Some(text.clone());
                *state.copied.borrow_mut() = Some(text);
            }
            TerminalUpdate::Clipboard {
                text,
                destination: ClipboardDestination::Primary,
            } => crate::primary::set(text),
            TerminalUpdate::ScrollWindow(window) => {
                let install = state.scrollback.borrow_mut().install_window(window);
                if let Some((anchor, height)) = install.request {
                    (state.session).send(TerminalCommand::RequestScrollWindow { anchor, height });
                }
            }
            // A runtime that failed serves no window.
            TerminalUpdate::Error(_) => {
                state.scrollback.borrow_mut().abandon();
            }
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

/// Builds the view, with the way into it.
pub fn build(given: TerminalView) -> (El, Terminal) {
    let TerminalView {
        session,
        look,
        open_url,
    } = given;
    let state = Rc::new(State {
        session,
        look: RefCell::new(look.get_untracked()),
        restyled: Cell::new(0),
        frame: RefCell::new(None),
        seen: Cell::new(0),
        cells: Cell::new((1., 0, 0)),
        origin: Cell::new((0., 0.)),
        preedit: RefCell::new(String::new()),
        focused: Cell::new(false),
        copied: RefCell::new(None),
        outgoing: RefCell::new(None),
        scrollback: RefCell::default(),
        wheel: Cell::new(0.),
        clicked: Cell::new(None),
        reporting: Cell::new(None),
        open_url,
        dragging: Cell::new(None),
    });
    // The colors a program is told when it asks (OSC 4, 10, 11, 12) are
    // the ones it is drawn in, whenever they change.
    let restyled = state.clone();
    Effect::new(move |_| {
        let look = look.get();
        (restyled.session).send(TerminalCommand::SetColorScheme(color_scheme(&look)));
        *restyled.look.borrow_mut() = look;
        restyled.restyled.set(restyled.restyled.get() + 1);
    });
    let element = el("div").class(styles::TERMINAL).attr("tabindex", "0");

    // The cell under a point of the element's box.
    let cell_at = {
        let state = state.clone();
        move |x: f32, y: f32| {
            let (width, columns, rows) = state.cells.get();
            TerminalSelectionPoint {
                row: ((y as f64 / state.look.borrow().line_height).max(0.) as usize)
                    .min(rows.saturating_sub(1)),
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
            let button = button(pointer.button);
            // A link clicked with the platform's key held is opened, and
            // the press goes no further: not to the program, not into a
            // selection.
            if button == TerminalMouseButton::Left && pointer.mods.meta() {
                let link = link_at(&pressed, pointer.element.x, pointer.element.y);
                let open = pressed.open_url.clone();
                if let (Some(link), Some(open)) = (link.filter(|link| is_openable_url(link)), open)
                {
                    open(&link);
                    return;
                }
            }
            // A program that asked for the mouse gets it, unless Shift
            // keeps the press for a selection.
            let selects = !reports_down() || pointer.mods.shift();
            if !selects {
                pressed.dragging.set(Some(false));
                pressed.reporting.set(Some(button));
                (pressed.session).send(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Press,
                    button,
                    point,
                    modifiers: mouse_modifiers(&pointer.mods),
                }));
                return;
            }
            // The middle button pastes the primary selection.
            if button == TerminalMouseButton::Middle {
                crate::primary::request_paste();
            }
            if button != TerminalMouseButton::Left {
                return;
            }
            pressed.dragging.set(Some(true));
            // A second press at the same cell soon after the first selects
            // by word, and a third by line.
            let now = std::time::Instant::now();
            let presses = match pressed.clicked.get() {
                Some((at, cell, presses))
                    if cell == point && now.duration_since(at) < MULTI_CLICK_WITHIN =>
                {
                    presses.saturating_add(1)
                }
                _ => 1,
            };
            pressed.clicked.set(Some((now, point, presses)));
            // A selection is of the live screen, which the daemon holds.
            pressed.scrollback.borrow_mut().abandon();
            pressed.session.send(TerminalCommand::SelectionStart {
                point,
                kind: match presses {
                    1 => TerminalSelectionKind::Simple,
                    2 => TerminalSelectionKind::Word,
                    _ => TerminalSelectionKind::Line,
                },
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
            if selects {
                moved.scrollback.borrow_mut().abandon();
                moved.session.send(TerminalCommand::SelectionUpdate(point));
            } else if let Some(button) = moved.reporting.get() {
                (moved.session).send(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Drag,
                    button,
                    point,
                    modifiers: mouse_modifiers(&pointer.mods),
                }));
            }
        })
        .on("pointerup", move |event| {
            let DomEventData::PointerUp(pointer) = &event.data else {
                return;
            };
            if lifted.dragging.take() == Some(false) {
                let point = cell_up(pointer.element.x, pointer.element.y);
                let held = lifted.reporting.take();
                (lifted.session).send(TerminalCommand::Mouse(TerminalMouseReport {
                    kind: TerminalMouseKind::Release,
                    button: held.unwrap_or_else(|| button(pointer.button)),
                    point,
                    modifiers: mouse_modifiers(&pointer.mods),
                }));
            }
        })
        .on("wheel", move |event| {
            let DomEventData::Wheel(wheel) = &event.data else {
                return;
            };
            let line_height = wheeled.look.borrow().line_height;
            let local =
                (wheeled.frame.borrow().as_ref()).is_some_and(|frame| frame.scrollback_available);
            if local {
                // The history is the terminal's own to scroll: by the
                // distance the wheel turned, a part of a row too.
                wheeled.wheel.set(0.);
                let rows = match wheel.delta {
                    BlitzWheelDelta::Lines(_, lines) => lines * WHEEL_LINE / line_height,
                    BlitzWheelDelta::Pixels(_, pixels) => pixels / line_height,
                };
                let viewport = wheeled.cells.get().2;
                let decision = (wheeled.scrollback.borrow_mut()).on_wheel(rows as f32, viewport);
                if let ScrollIpc::Request { anchor, height } = decision.ipc {
                    (wheeled.session).send(TerminalCommand::RequestScrollWindow { anchor, height });
                }
                prevent_default();
                return;
            }
            // The wheel is the program's (or the peer's): whole rows, told
            // to it.
            wheeled.scrollback.borrow_mut().abandon();
            let turned = match wheel.delta {
                BlitzWheelDelta::Lines(_, lines) => lines * 3.,
                BlitzWheelDelta::Pixels(_, pixels) => pixels / line_height,
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
                (cursor.row as f64 * area.look.borrow().line_height) as f32,
                width as f32,
                area.look.borrow().line_height as f32,
            ))
        })
        .widget({
            let state = state.clone();
            move || Screen {
                state: state.clone(),
                shaper: Shaper::with_font(
                    &state.look.borrow().font_family,
                    state.look.borrow().font_size,
                ),
                rows: Vec::new(),
                history: (0, std::collections::HashMap::new()),
                shaped_at: 0.,
                baseline: 0.,
                // (The first paint takes the look as it is then.)
                styled: u64::MAX,
            }
        });

    // The session's frames and updates come in with every layout, and the
    // box's size in cells goes out when it changes.
    let sized = state.clone();
    // A cell's width in CSS pixels, in the look numbered `styled`.
    let measure =
        |look: &TerminalLook| Shaper::with_font(&look.font_family, look.font_size).cell_width(1.);
    let mut cell_width = measure(&state.look.borrow());
    let mut styled = 0;
    each_layout(move |page| {
        pump(&sized);
        if styled != sized.restyled.get() {
            styled = sized.restyled.get();
            cell_width = measure(&sized.look.borrow());
        }
        // Whether the keys come here: asked of the page, since a focus
        // given from code is not announced by an event. A window that
        // does not have the desktop's focus brings no keys.
        let focused = page.has_focus(element) && crate::window::active();
        if focused != sized.focused.replace(focused) {
            if !focused {
                sized.preedit.borrow_mut().clear();
            }
            sized.session.send(TerminalCommand::Focus(focused));
        }
        if let Some(text) = sized.outgoing.borrow_mut().take() {
            copy(text);
        }
        if let Some(origin) = page.painted_at(element) {
            sized.origin.set(origin);
        }
        let Some(size) = page.size(element) else {
            return;
        };
        let size = kurbo::Size::new(size.0, size.1);
        let columns = (size.width / cell_width) as usize;
        let rows = (size.height / sized.look.borrow().line_height) as usize;
        if (columns, rows) != (sized.cells.get().1, sized.cells.get().2) && columns * rows > 0 {
            sized.cells.set((cell_width, columns, rows));
            // A window of history was served for the old size.
            sized.scrollback.borrow_mut().abandon();
            (sized.session).send(TerminalCommand::Resize(TerminalSize::new(
                columns as u16,
                rows as u16,
            )));
        }
    });
    (element, Terminal { state, element })
}

/// The colors a program in the terminal is answered with when it asks:
/// those of `look`.
pub fn color_scheme(look: &TerminalLook) -> TerminalColorScheme {
    TerminalColorScheme::from_rgb(look.foreground, look.background, look.cursor, look.ansi)
}

/// The link at a point of the view's box, in the rows it is showing: the
/// live screen's, or the history's while it is scrolled back.
fn link_at(state: &State, x: f32, y: f32) -> Option<String> {
    let (cell_width, ..) = state.cells.get();
    let line_height = state.look.borrow().line_height;
    let column = (x as f64 / cell_width).max(0.) as usize;
    let row = y as f64 / line_height;
    let scrolled = (state.scrollback.borrow()).visible_lines(state.cells.get().2);
    if let Some((window, range, into)) = scrolled {
        let row = (row + into as f64).max(0.) as usize;
        return url_from_lines(&window.lines[range], row, column);
    }
    let frame = state.frame.borrow();
    url_from_lines(&frame.as_ref()?.lines, row.max(0.) as usize, column)
}

/// How far one line of a wheel that turns in lines moves the history, in
/// CSS pixels (`src/terminal/input.rs`'s unit).
const WHEEL_LINE: f64 = 20.;

/// How soon after a press another at the same cell is the same gesture.
const MULTI_CLICK_WITHIN: std::time::Duration = std::time::Duration::from_millis(400);

fn button(button: MouseEventButton) -> TerminalMouseButton {
    match button {
        MouseEventButton::Auxiliary => TerminalMouseButton::Middle,
        MouseEventButton::Secondary => TerminalMouseButton::Right,
        _ => TerminalMouseButton::Left,
    }
}

fn mouse_modifiers(modifiers: &keyboard_types::Modifiers) -> TerminalMouseModifiers {
    TerminalMouseModifiers {
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        control: modifiers.ctrl(),
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
    /// The characters drawn from shapes and not from a font (box drawing,
    /// block elements, sextants, Braille): each one's column and color. A
    /// font's glyph cannot fill a cell taller than its em box, and its
    /// edges leave seams between cells.
    drawn: Vec<(usize, char, [u8; 3])>,
}

fn shape(line: &TerminalLine, shaper: &mut Shaper, scale: f32, look: &TerminalLook) -> Shaped {
    let (mut text, mut colors, mut backgrounds, mut columns) =
        (String::new(), Vec::new(), Vec::new(), Vec::new());
    let mut drawn = Vec::new();
    let mut column = 0;
    for span in &line.spans {
        let start = text.len();
        for character in span.text.chars() {
            // A cell a tab was written to holds the tab itself: one blank
            // cell on the screen, and no glyph in any font.
            let character = match character.is_control() {
                true => ' ',
                false => character,
            };
            columns.push((text.len(), column));
            // What is drawn from shapes is a blank to the font.
            if drawn_glyphs::is_geometric(character) {
                drawn.push((column, character, rgb(span.fg, look)));
                text.push(' ');
            } else {
                text.push(character);
            }
            column += character.width().unwrap_or(0);
        }
        colors.push((start..text.len(), Ink(rgb(span.fg, look))));
        // The frame's count of the span's cells is what places the next
        // span, whatever the characters' own widths came to.
        let first = columns.iter().rev().find(|(byte, _)| *byte <= start);
        let from = first.map_or(column, |(_, column)| *column);
        let from = if text.len() == start { column } else { from };
        column = from + span.columns;
        let background = rgb(span.bg, look);
        if background != look.background {
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
        drawn,
    }
}

/// A length in the box, moved to the nearest edge of a device pixel of
/// the display, the box itself starting `inset` into one.
fn on_pixel(length: f64, inset: f64) -> f64 {
    (length + inset).round() - inset
}

/// Draws a row's characters that are shapes, the row's top at `top`: each
/// in its cell, `cell` wide and `line` tall, for a font of `font_size`,
/// all in device pixels. `inset` is how far into a device pixel the box
/// itself starts: the shapes' edges are on the display's pixels, not on
/// the box's.
fn shapes(
    scene: &mut impl PaintScene,
    shaped: &Shaped,
    cell: f64,
    top: f64,
    line: f64,
    font_size: f32,
    inset: (f64, f64),
) {
    for (column, character, [r, g, b]) in &shaped.drawn {
        let bounds = drawn_glyphs::Bounds {
            origin: drawn_glyphs::Point {
                x: (inset.0 + *column as f64 * cell) as f32,
                y: (inset.1 + top) as f32,
            },
            size: drawn_glyphs::Size {
                width: cell as f32,
                height: line as f32,
            },
        };
        let at =
            |point: drawn_glyphs::Point<f32>| (point.x as f64 - inset.0, point.y as f64 - inset.1);
        // (The lengths are device pixels already: the edges are snapped
        // to them.)
        drawn_glyphs::glyph_shapes(
            bounds,
            *character,
            font_size,
            1.,
            &mut |shape| match shape {
                GlyphShape::Fill { bounds, alpha } => {
                    let (left, top) = at(bounds.origin);
                    let rect = Rect::new(
                        left,
                        top,
                        left + bounds.size.width as f64,
                        top + bounds.size.height as f64,
                    );
                    let alpha = (alpha.clamp(0., 1.) * 255.).round() as u8;
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::from_rgba8(*r, *g, *b, alpha),
                        None,
                        &rect,
                    );
                }
                GlyphShape::RoundedCorner(spec) => {
                    let mut path = kurbo::BezPath::new();
                    path.move_to(at(spec.start));
                    path.line_to(at(spec.curve_start));
                    path.curve_to(at(spec.control_a), at(spec.control_b), at(spec.curve_end));
                    path.line_to(at(spec.end));
                    scene.stroke(
                        &Stroke::new(spec.stroke_width as f64),
                        Affine::IDENTITY,
                        Color::from_rgb8(*r, *g, *b),
                        None,
                        &path,
                    );
                }
                GlyphShape::Diagonal(spec) => {
                    let path = kurbo::Line::new(at(spec.start), at(spec.end));
                    scene.stroke(
                        &Stroke::new(spec.stroke_width as f64),
                        Affine::IDENTITY,
                        Color::from_rgb8(*r, *g, *b),
                        None,
                        &path,
                    );
                }
            },
        );
    }
}

/// The frame, painted into the element's box.
struct Screen {
    state: Rc<State>,
    shaper: Shaper,
    rows: Vec<Shaped>,
    /// The rows shaped of the window of history last drawn from: which
    /// window, and each row by its place in it.
    history: (usize, std::collections::HashMap<usize, Shaped>),
    /// The scale the rows were shaped at.
    shaped_at: f64,
    /// A row's baseline below its top, at that scale.
    baseline: f32,
    /// Which of the looks given the rows were shaped in.
    styled: u64,
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
    baseline: f32,
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
        let (font, style) = (run.run(), run.style_index());
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

impl horizon_ui::blitz_dom::Widget for Screen {
    fn paint(
        &mut self,
        _context: &mut dyn anyrender::RenderContext,
        _styles: &horizon_ui::blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyrender::Scene {
        let state = self.state.clone();
        let look = state.look.borrow();
        let look = &*look;
        let mut scene = anyrender::Scene::new();
        // Another font or size: nothing shaped before holds.
        if self.styled != state.restyled.get() {
            self.styled = state.restyled.get();
            self.shaper = Shaper::with_font(&look.font_family, look.font_size);
            self.shaped_at = 0.;
        }
        fill(
            &mut scene,
            look.background,
            Rect::new(0., 0., width as f64, height as f64),
        );
        let frame = self.state.frame.borrow();
        let Some(frame) = frame.as_ref() else {
            return scene;
        };
        let (cell, line) = (self.state.cells.get().0 * scale, look.line_height * scale);
        let font_px = look.font_size * scale as f32;
        let (left, above) = self.state.origin.get();
        let inset = ((left * scale).fract(), (above * scale).fract());
        if self.shaped_at != scale {
            self.history.1.clear();
        }
        // Scrolled back: rows of the window of history held, moved up by
        // the part of a row the view is into its top one. No cursor and no
        // selection: those are of the live screen.
        let scrolled = (self.state.scrollback.borrow()).visible_lines(self.state.cells.get().2);
        if let Some((window, range, into)) = scrolled {
            if self.shaped_at != scale {
                self.shaped_at = scale;
                self.rows.clear();
                self.baseline = (self.shaper).baseline(scale as f32, look.line_height);
            }
            // The rows shaped are this window's, by their place in it.
            let held = std::sync::Arc::as_ptr(&window) as usize;
            if self.history.0 != held {
                self.history = (held, std::collections::HashMap::new());
            }
            let near = range.start.saturating_sub(range.len())..range.end + range.len();
            self.history.1.retain(|row, _| near.contains(row));
            let clip = Rect::new(0., 0., width as f64, height as f64);
            scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
            for (place, row) in range.enumerate() {
                let top = (place as f64 - into as f64) * line;
                let shaper = &mut self.shaper;
                let shaped = (self.history.1.entry(row))
                    .or_insert_with(|| shape(&window.lines[row], shaper, scale as f32, look));
                for (columns, color) in &shaped.row.backgrounds {
                    let behind = Rect::new(
                        on_pixel(columns.start as f64 * cell, inset.0),
                        top,
                        on_pixel(columns.end as f64 * cell, inset.0),
                        top + line,
                    );
                    fill(&mut scene, *color, behind);
                }
                glyphs(
                    &mut scene,
                    shaped,
                    &shaped.columns,
                    cell,
                    0.,
                    top,
                    self.baseline,
                );
                shapes(&mut scene, shaped, cell, top, line, font_px, inset);
            }
            scene.pop_layer();
            return scene;
        }

        if self.shaped_at != scale {
            self.rows.clear();
            self.shaped_at = scale;
            self.baseline = (self.shaper).baseline(scale as f32, look.line_height);
        }
        // A row drawn before is not shaped again, at whatever place it is
        // now: output that scrolls moves every row and changes one.
        let mut before: Vec<Option<Shaped>> = (std::mem::take(&mut self.rows).into_iter())
            .map(Some)
            .collect();
        // How far below its new place the last row found was, and how many
        // rows were looked for among all of those drawn before and not
        // found: after a couple, the screen is taken to be all new (output
        // faster than the frames), and the rest are not looked for.
        let (mut below, mut missed) = (0, 0);
        for (index, wanted) in frame.lines.iter().enumerate() {
            let same = |shaped: &Option<Shaped>| {
                (shaped.as_ref()).is_some_and(|shaped| shaped.line == *wanted)
            };
            let mut found = [index + below, index]
                .into_iter()
                .find(|at| before.get(*at).is_some_and(same));
            if found.is_none() && missed < 2 {
                found = before.iter().position(same);
                missed += usize::from(found.is_none());
            }
            let shaped = match found.and_then(|at| Some((at, before[at].take()?))) {
                Some((at, shaped)) => {
                    below = at.saturating_sub(index);
                    shaped
                }
                None => shape(wanted, &mut self.shaper, scale as f32, look),
            };
            self.rows.push(shaped);
        }

        // (Edges on the display's pixels: two fills that meet part of a
        // pixel in leave a line between them.)
        let cells = |row: usize, from: usize, to: usize| {
            Rect::new(
                on_pixel(from as f64 * cell, inset.0),
                on_pixel(row as f64 * line, inset.1),
                on_pixel(to as f64 * cell, inset.0),
                on_pixel((row + 1) as f64 * line, inset.1),
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
                fill(&mut scene, look.selection, cells(row, from, to));
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
                self.baseline,
            );
            shapes(
                &mut scene,
                shaped,
                cell,
                index as f64 * line,
                line,
                font_px,
                inset,
            );
        }

        if let Some(cursor) = frame.cursor {
            let place = cells(cursor.row, cursor.col, cursor.col + 1);
            let [r, g, b] = look.cursor;
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
                let shaped = shape(&composing, &mut self.shaper, scale as f32, look);
                let wide = shaped.line.spans[0].columns;
                let over = cells(cursor.row, cursor.col, cursor.col + wide);
                fill(&mut scene, look.background, over);
                glyphs(
                    &mut scene,
                    &shaped,
                    &shaped.columns,
                    cell,
                    place.x0,
                    place.y0,
                    self.baseline,
                );
                let under = Rect::new(over.x0, over.y1 - 1.5 * scale, over.x1, over.y1);
                fill(&mut scene, look.cursor, under);
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
                    Color::from_rgba8(r, g, b, 0xc0),
                    None,
                    &shape,
                );
            }
        }
        scene
    }
}
