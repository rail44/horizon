//! S1 — render a live horizon-terminal-core session in a GPUI view.
//! See docs/gpui-migration-consideration.md for the spike plan.
//!
//! Set SPIKE_DUMP=/path/to/file to mirror every snapshot's plain text to a
//! file (the headless-verification tap, standing in for the gui-verify
//! terminal dump until real tooling exists on this stack).

mod colors;
mod pty;

use std::cell::Cell;
use std::rc::Rc;

use std::sync::Arc;

use futures::StreamExt;
use gpui::*;
use gpui_component::dock::{DockArea, DockItem, Panel, PanelEvent};
use gpui_component::Root;
use horizon_terminal_core::{TerminalCommand, TerminalFrame, TerminalSize, TerminalUpdate};

const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 17.0;

struct TerminalView {
    frame: Option<TerminalFrame>,
    tx: crossbeam_channel::Sender<TerminalCommand>,
    focus_handle: FocusHandle,
    // Shared with the paint closure (which only gets &mut App, not the
    // entity) so bounds-driven resize can be deduped without an update.
    last_size: Rc<Cell<TerminalSize>>,
    // IME preedit — client-side only, never sent to the PTY. The commit
    // path (replace_text_in_range) writes raw UTF-8 bytes instead. See
    // docs/research/gpui-terminal-implementations.md §4 S3.
    ime_marked_text: Option<String>,
    exited: bool,
}

impl TerminalView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        };
        let session = pty::spawn(initial).expect("failed to spawn PTY session");
        let update_rx = session.rx;

        // Headless test driver: type SPIKE_DRIVE's bytes into the session
        // shortly after startup, so frame content can be verified via
        // SPIKE_DUMP without a human at the keyboard.
        if let Ok(script) = std::env::var("SPIKE_DRIVE") {
            // SPIKE_DRIVE_ENTER=1 sends the trailing newline as a
            // TerminalCommand::Key instead of raw bytes, exercising the
            // core-side key encoder end to end (the S2 path).
            let key_enter = std::env::var_os("SPIKE_DRIVE_ENTER").is_some();
            let drive_tx = session.tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = drive_tx.send(TerminalCommand::Input(script.into_bytes()));
                if key_enter {
                    let _ = drive_tx.send(TerminalCommand::Key {
                        key: termwiz::input::KeyCode::Enter,
                        modifiers: termwiz::input::Modifiers::NONE,
                        event: horizon_terminal_core::KeyEventKind::Press,
                    });
                }
            });
        }

        // Bridge the blocking crossbeam receiver onto GPUI's async world.
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(update) = update_rx.recv() {
                if async_tx.unbounded_send(update).is_err() {
                    return;
                }
            }
        });
        let dump_path = std::env::var_os("SPIKE_DUMP").map(std::path::PathBuf::from);
        cx.spawn(async move |this, cx| {
            while let Some(update) = async_rx.next().await {
                let apply = this.update(cx, |view: &mut TerminalView, cx| {
                    match update {
                        TerminalUpdate::Snapshot(frame) => {
                            if let Some(path) = &dump_path {
                                let _ = std::fs::write(path, dump_frame(&frame));
                            }
                            view.frame = Some(frame);
                        }
                        TerminalUpdate::Exited => view.exited = true,
                        TerminalUpdate::Error(error) => eprintln!("terminal error: {error}"),
                        TerminalUpdate::Title(_)
                        | TerminalUpdate::Bell
                        | TerminalUpdate::Clipboard(_) => {}
                    }
                    cx.notify();
                });
                if apply.is_err() {
                    return;
                }
            }
        })
        .detach();

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            frame: None,
            tx: session.tx,
            focus_handle,
            last_size: Rc::new(Cell::new(initial)),
            ime_marked_text: None,
            exited: false,
        }
    }

    // S2 — the real mapping layer: GPUI keystrokes become
    // TerminalCommand::Key (termwiz KeyCode + Modifiers + event kind), so
    // encoding — legacy escape sequences AND negotiated kitty state — is
    // horizon-terminal-core's job, not the view's. Plain printable text
    // stays on the text-input pipeline (replace_text_in_range): on macOS
    // it fires for every printable keypress, so routing it here too would
    // double-feed (the S3 lesson). A kitty "report all keys" session
    // would need a mode-aware switch for text keys — recorded as an
    // integration-time question in docs/gpui-migration-consideration.md.
    fn handle_key(&mut self, event: &KeyDownEvent) {
        // While the IME is composing, every keystroke belongs to the IME
        // (candidate selection etc.) — letting it through would double-feed
        // the terminal. The composed result arrives via
        // replace_text_in_range instead.
        if self.ime_marked_text.is_some() {
            return;
        }
        let keystroke = &event.keystroke;
        let Some(key) = term_key_code(keystroke) else {
            return;
        };
        let kind = if event.is_held {
            horizon_terminal_core::KeyEventKind::Repeat
        } else {
            horizon_terminal_core::KeyEventKind::Press
        };
        let _ = self.tx.send(TerminalCommand::Key {
            key,
            modifiers: term_modifiers(&keystroke.modifiers),
            event: kind,
        });
    }
}

// S4 — mount terminal views inside a gpui-component Dock: an h_split of
// two tab groups, each holding one live terminal session. Everything else
// (tab strip, resize dividers, drag-to-rearrange, zoom) comes from the
// Dock component itself.
struct SpikeWorkspace {
    dock_area: Entity<DockArea>,
}

impl SpikeWorkspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("spike-dock", Some(1), window, cx));
        let weak = dock_area.downgrade();

        let left = DockItem::tab(cx.new(|cx| TerminalView::new(window, cx)), &weak, window, cx);
        let right = DockItem::tab(cx.new(|cx| TerminalView::new(window, cx)), &weak, window, cx);
        let center = DockItem::h_split(vec![left, right], &weak, window, cx);

        dock_area.update(cx, |dock_area, cx| {
            dock_area.set_center(center, window, cx);
        });

        Self { dock_area }
    }
}

impl Render for SpikeWorkspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}

impl EventEmitter<PanelEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for TerminalView {
    fn panel_name(&self) -> &'static str {
        "terminal"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Terminal"
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// GPUI keystroke → termwiz key code. Named/function keys always map;
/// character keys map only when Ctrl is held (otherwise they are text and
/// belong to the input-handler pipeline). Alt-held characters are left to
/// macOS's option-composition for now — whether to treat option as alt is
/// a real-integration decision, not a spike one.
fn term_key_code(keystroke: &Keystroke) -> Option<termwiz::input::KeyCode> {
    use termwiz::input::KeyCode;

    let named = match keystroke.key.as_str() {
        "enter" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "backspace" => Some(KeyCode::Backspace),
        "escape" => Some(KeyCode::Escape),
        "up" => Some(KeyCode::UpArrow),
        "down" => Some(KeyCode::DownArrow),
        "right" => Some(KeyCode::RightArrow),
        "left" => Some(KeyCode::LeftArrow),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" => Some(KeyCode::PageUp),
        "pagedown" => Some(KeyCode::PageDown),
        "delete" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        _ => None,
    };
    if let Some(key) = named {
        return Some(key);
    }
    if let Some(number) = keystroke
        .key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(KeyCode::Function(number));
    }

    let mut chars = keystroke.key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    keystroke.modifiers.control.then_some(KeyCode::Char(ch))
}

fn term_modifiers(modifiers: &Modifiers) -> termwiz::input::Modifiers {
    use termwiz::input::Modifiers as TermModifiers;

    let mut result = TermModifiers::NONE;
    if modifiers.control {
        result |= TermModifiers::CTRL;
    }
    if modifiers.alt {
        result |= TermModifiers::ALT;
    }
    if modifiers.shift {
        result |= TermModifiers::SHIFT;
    }
    if modifiers.platform {
        result |= TermModifiers::SUPER;
    }
    result
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        self.ime_marked_text.clone()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.ime_marked_text.as_deref().map(utf16_len).unwrap_or(0);
        Some(UTF16Selection {
            range: caret..caret,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.ime_marked_text
            .as_deref()
            .map(|marked| 0..utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime_marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked_text = None;
        let _ = self
            .tx
            .send(TerminalCommand::Input(text.as_bytes().to_vec()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked_text = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_string())
        };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.frame.as_ref()?.cursor?;
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font(FONT_FAMILY));
        let cell_width = text_system
            .advance(font_id, px(FONT_SIZE), 'M')
            .map(|size| size.width)
            .unwrap_or(px(8.0));
        let origin = element_bounds.origin
            + point(
                cell_width * cursor.col as f32 + cell_width * range_utf16.start as f32,
                px(LINE_HEIGHT) * cursor.row as f32,
            );
        Some(Bounds::new(origin, gpui::size(cell_width, px(LINE_HEIGHT))))
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

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let tx = self.tx.clone();
        let last_size = self.last_size.clone();
        let focus_handle = self.focus_handle.clone();
        div()
            .size_full()
            .bg(rgb(colors::BACKGROUND))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, _cx| {
                view.handle_key(event);
            }))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        paint_terminal(bounds, &entity, &tx, &last_size, window, cx);
                    },
                )
                .size_full(),
            )
    }
}

fn paint_terminal(
    bounds: Bounds<Pixels>,
    entity: &Entity<TerminalView>,
    tx: &crossbeam_channel::Sender<TerminalCommand>,
    last_size: &Rc<Cell<TerminalSize>>,
    window: &mut Window,
    cx: &mut App,
) {
    let text_system = window.text_system().clone();
    let font = font(FONT_FAMILY);
    let font_size = px(FONT_SIZE);
    let line_height = px(LINE_HEIGHT);
    let font_id = text_system.resolve_font(&font);
    let cell_width = text_system
        .advance(font_id, font_size, 'M')
        .map(|size| size.width)
        .unwrap_or(px(8.0));

    let cols = (f32::from(bounds.size.width) / f32::from(cell_width)).floor() as u16;
    let rows = (f32::from(bounds.size.height) / f32::from(line_height)).floor() as u16;
    let size = TerminalSize {
        cols: cols.max(2),
        rows: rows.max(2),
        pixel_width: (f32::from(cell_width) * cols.max(2) as f32) as u16,
        pixel_height: (f32::from(line_height) * rows.max(2) as f32) as u16,
    };
    if last_size.get() != size {
        last_size.set(size);
        let _ = tx.send(TerminalCommand::Resize(size));
    }

    let (frame, marked_text) = {
        let view = entity.read(cx);
        (view.frame.clone(), view.ime_marked_text.clone())
    };
    let Some(frame) = frame else {
        return;
    };

    let default_bg = colors::to_hsla(colors::resolve(
        horizon_terminal_core::TerminalColor::Named(
            alacritty_terminal::vte::ansi::NamedColor::Background,
        ),
        &frame.palette_overrides,
    ));

    // Grid-positioned painting (the pattern every surveyed GPUI terminal
    // converges on — see docs/research/gpui-terminal-implementations.md):
    // each span is painted at its computed column offset, never left to
    // shaped-text flow, and glyph advances are snapped to the cell grid
    // via shape_line's force_width when the span is width-uniform.
    for (row, line) in frame.lines.iter().enumerate() {
        if row >= size.rows as usize {
            break;
        }
        let y = line_height * row as f32;
        let mut col = 0_usize;
        for span in &line.spans {
            let x = cell_width * col as f32;
            col += span.columns;
            if span.text.is_empty() {
                continue;
            }
            let origin = bounds.origin + point(x, y);

            let bg = colors::to_hsla(colors::resolve(span.bg, &frame.palette_overrides));
            if bg != default_bg {
                window.paint_quad(fill(
                    Bounds::new(
                        origin,
                        gpui::size(cell_width * span.columns as f32, line_height),
                    ),
                    bg,
                ));
            }

            if span.text.trim().is_empty() {
                continue;
            }
            let fg = colors::to_hsla(colors::resolve(span.fg, &frame.palette_overrides));
            let run = TextRun {
                len: span.text.len(),
                font: font.clone(),
                color: fg,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // Snap glyphs to the cell grid only when every char in the span
            // occupies the same number of columns; a mixed-width span keeps
            // natural shaping (positioned correctly at its start column,
            // with only intra-span drift possible).
            let chars = span.text.chars().count();
            let force_width = if span.columns == chars {
                Some(cell_width)
            } else if span.columns == chars * 2 {
                Some(cell_width * 2.0)
            } else {
                None
            };
            let shaped =
                text_system.shape_line(span.text.clone().into(), font_size, &[run], force_width);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
    }

    // IME preedit overlay: paint the composing text at the cursor cell,
    // underlined, over an opaque background quad that hides whatever the
    // grid has there. The regular cursor is suppressed while composing
    // (every surveyed implementation does the same).
    if let Some(marked) = marked_text.filter(|marked| !marked.is_empty()) {
        if let Some(cursor) = frame.cursor {
            let origin = bounds.origin
                + point(
                    cell_width * cursor.col as f32,
                    line_height * cursor.row as f32,
                );
            let columns: usize = marked
                .chars()
                .map(|ch| unicode_width(ch).max(1))
                .sum();
            window.paint_quad(fill(
                Bounds::new(origin, gpui::size(cell_width * columns as f32, line_height)),
                colors::to_hsla(colors::resolve(
                    horizon_terminal_core::TerminalColor::Named(
                        alacritty_terminal::vte::ansi::NamedColor::Background,
                    ),
                    &frame.palette_overrides,
                )),
            ));
            let fg = colors::to_hsla(colors::resolve(
                horizon_terminal_core::TerminalColor::Named(
                    alacritty_terminal::vte::ansi::NamedColor::Foreground,
                ),
                &frame.palette_overrides,
            ));
            let run = TextRun {
                len: marked.len(),
                font: font.clone(),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let shaped = text_system.shape_line(marked.into(), font_size, &[run], None);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
        return;
    }

    if let Some(cursor) = frame.cursor {
        let origin = bounds.origin
            + point(
                cell_width * cursor.col as f32,
                line_height * cursor.row as f32,
            );
        let mut color = colors::to_hsla(colors::resolve(
            horizon_terminal_core::TerminalColor::Named(
                alacritty_terminal::vte::ansi::NamedColor::Cursor,
            ),
            &frame.palette_overrides,
        ));
        color.a = 0.6;
        window.paint_quad(fill(
            Bounds::new(origin, gpui::size(cell_width, line_height)),
            color,
        ));
    }
}

/// East-Asian width of a char in terminal columns (mirrors the
/// `char_width` helper in horizon-terminal-core's frame module).
fn unicode_width(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    ch.width().unwrap_or(0)
}

/// Plain text plus a per-line span/color table (logical colors as parsed,
/// cursor position) — the headless half of S1 verification; actual pixel
/// output still needs eyes on the window.
fn dump_frame(frame: &TerminalFrame) -> String {
    use std::fmt::Write as _;

    let mut out = frame.text.clone();
    out.push_str("\n--- spans ---\n");
    if let Some(cursor) = frame.cursor {
        let _ = writeln!(out, "cursor: row={} col={}", cursor.row, cursor.col);
    }
    for (row, line) in frame.lines.iter().enumerate() {
        for span in &line.spans {
            if span.text.trim().is_empty() {
                continue;
            }
            let _ = writeln!(
                out,
                "row {row}: {:?} fg={:?} bg={:?}",
                span.text, span.fg, span.bg
            );
        }
    }
    out
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let workspace = cx.new(|cx| SpikeWorkspace::new(window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
