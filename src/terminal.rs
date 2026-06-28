use std::env;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, Rgb};
use crossbeam_channel::{Receiver, Sender};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.columns().saturating_sub(1))
    }

    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines().saturating_sub(1) as i32)
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
}

#[derive(Clone, Debug, Default)]
pub struct TerminalEvents {
    pub pty_writes: Vec<Vec<u8>>,
    pub title: Option<String>,
    pub bell_count: usize,
}

#[derive(Clone, Debug, Default)]
struct EventSink {
    events: Arc<Mutex<TerminalEvents>>,
}

impl EventSink {
    fn drain(&self) -> TerminalEvents {
        std::mem::take(&mut *self.events.lock().expect("terminal event mutex poisoned"))
    }
}

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        let mut events = self.events.lock().expect("terminal event mutex poisoned");
        match event {
            Event::PtyWrite(text) => events.pty_writes.push(text.into_bytes()),
            Event::Title(title) => events.title = Some(title),
            Event::ResetTitle => events.title = None,
            Event::Bell => events.bell_count += 1,
            _ => {}
        }
    }
}

pub struct TerminalCore {
    term: Term<EventSink>,
    parser: Processor,
    events: EventSink,
    size: TerminalSize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFrame {
    pub text: String,
    pub lines: Vec<TerminalLine>,
    pub cursor: Option<TerminalCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalLine {
    pub spans: Vec<TerminalSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSpan {
    pub text: String,
    pub columns: usize,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSelectionPoint {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalScroll {
    pub lines: i32,
    pub point: TerminalSelectionPoint,
}

impl TerminalFrame {
    pub fn from_text(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| TerminalLine {
                spans: vec![TerminalSpan {
                    columns: line.chars().map(char_width).sum(),
                    text: line.to_string(),
                    fg: DEFAULT_FG,
                    bg: DEFAULT_BG,
                }],
            })
            .collect();
        Self {
            text,
            lines,
            cursor: None,
        }
    }
}

impl TerminalCore {
    pub fn new(size: TerminalSize) -> Self {
        let events = EventSink::default();
        let config = TermConfig {
            kitty_keyboard: true,
            ..TermConfig::default()
        };
        let term = Term::new(config, &size, events.clone());

        Self {
            term,
            parser: Processor::new(),
            events,
            size,
        }
    }

    pub fn write_vt(&mut self, bytes: &[u8]) -> TerminalEvents {
        self.parser.advance(&mut self.term, bytes);
        self.events.drain()
    }

    pub fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(size);
    }

    pub fn scroll_display(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn handle_scroll(&mut self, scroll: TerminalScroll) -> Option<Vec<u8>> {
        if self.application_scroll_mode() {
            return Some(self.scroll_input(scroll));
        }

        self.scroll_display(scroll.lines);
        None
    }

    pub fn paste_input(&self, text: &str) -> Vec<u8> {
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            let mut input = Vec::with_capacity(text.len() + 12);
            input.extend_from_slice(b"\x1b[200~");
            input.extend_from_slice(text.as_bytes());
            input.extend_from_slice(b"\x1b[201~");
            input
        } else {
            text.as_bytes().to_vec()
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn snapshot_text(&self) -> String {
        self.snapshot_frame().text
    }

    pub fn snapshot_frame(&self) -> TerminalFrame {
        let mut rows = vec![String::new(); self.size.rows as usize];
        let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; self.size.rows as usize];
        let content = self.term.renderable_content();

        for indexed in content.display_iter {
            let row = indexed.point.line.0;
            if row < 0 {
                continue;
            }

            let row = row as usize;
            if row >= rows.len() {
                continue;
            }

            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
            {
                continue;
            }

            let fg = cell_fg(cell.fg, cell.flags, content.colors);
            let bg = cell_bg(cell.bg, cell.flags, content.colors);
            let (fg, bg) = if content
                .selection
                .as_ref()
                .is_some_and(|selection| selection.contains(indexed.point))
            {
                (DEFAULT_BG, [132, 220, 198])
            } else {
                (fg, bg)
            };
            let columns = cell_width(cell.c, cell.flags);
            rows[row].push(cell.c);
            push_styled_cell(&mut styled_rows[row], cell.c, columns, fg, bg);
            if let Some(zerowidth) = cell.zerowidth() {
                rows[row].extend(zerowidth);
                for ch in zerowidth {
                    push_styled_cell(&mut styled_rows[row], *ch, 0, fg, bg);
                }
            }
        }

        let text = rows
            .into_iter()
            .map(|row| row.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        TerminalFrame {
            text,
            lines: styled_rows,
            cursor: cursor_position(content.cursor.point.line.0, content.cursor.point.column.0),
        }
    }

    pub fn encode_key(&self, key: KeyCode, mods: Modifiers, is_down: bool) -> String {
        key.encode(mods, self.encode_modes(), is_down)
            .unwrap_or_default()
    }

    pub fn key_input(&self, key: KeyCode, mods: Modifiers, is_down: bool) -> Vec<u8> {
        self.encode_key(key, mods, is_down).into_bytes()
    }

    pub fn start_selection(&mut self, point: TerminalSelectionPoint) {
        let point = self.selection_point(point);
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    pub fn update_selection(&mut self, point: TerminalSelectionPoint) {
        let point = self.selection_point(point);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, Side::Right);
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    fn encode_modes(&self) -> KeyCodeEncodeModes {
        let mode = *self.term.mode();
        let kitty_flags = kitty_flags_from_mode(mode);
        let encoding = if kitty_flags.is_empty() {
            KeyboardEncoding::Xterm
        } else {
            KeyboardEncoding::Kitty(kitty_flags)
        };

        KeyCodeEncodeModes {
            encoding,
            application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            newline_mode: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            modify_other_keys: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES).then_some(2),
        }
    }

    fn application_scroll_mode(&self) -> bool {
        self.term
            .mode()
            .intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
    }

    fn scroll_input(&self, scroll: TerminalScroll) -> Vec<u8> {
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
            return sgr_mouse_wheel_input(
                scroll.lines,
                scroll.point.col.saturating_add(1),
                scroll.point.row.saturating_add(1),
            );
        }

        arrow_scroll_input(scroll.lines)
    }

    fn selection_point(&self, point: TerminalSelectionPoint) -> TermPoint {
        TermPoint::new(
            Line(point.row as i32 - self.term.grid().display_offset() as i32),
            Column(point.col.min(self.size.cols.saturating_sub(1) as usize)),
        )
    }
}

impl Default for TerminalCore {
    fn default() -> Self {
        Self::new(TerminalSize::default())
    }
}

#[derive(Debug, Error)]
pub enum TerminalSessionError {
    #[error("failed to create PTY pair")]
    Pty(#[from] anyhow::Error),
    #[error("failed to clone PTY reader")]
    Reader(#[source] anyhow::Error),
    #[error("failed to clone PTY writer")]
    Writer(#[source] anyhow::Error),
    #[error("failed to spawn shell")]
    Spawn(#[source] anyhow::Error),
}

#[derive(Clone, Debug)]
pub enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        key: KeyCode,
        modifiers: Modifiers,
        is_down: bool,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    SelectionStart(TerminalSelectionPoint),
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum TerminalUpdate {
    Snapshot(TerminalFrame),
    Title(Option<String>),
    Bell,
    Clipboard(String),
    Exited,
    Error(String),
}

pub struct TerminalSession {
    tx: Sender<TerminalCommand>,
    rx: Receiver<TerminalUpdate>,
}

impl TerminalSession {
    pub fn spawn(size: TerminalSize) -> Result<Self, TerminalSessionError> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cmd = terminal_command(&shell);
        pair.slave
            .spawn_command(cmd)
            .map_err(TerminalSessionError::Spawn)?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(TerminalSessionError::Reader)?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(TerminalSessionError::Writer)?;

        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
        let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
        let (key_tx, key_rx) = crossbeam_channel::unbounded();
        let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
        let master = pair.master;
        let response_tx = command_tx.clone();
        let read_update_tx = update_tx.clone();

        thread::spawn(move || {
            read_pty(&mut *reader, pty_tx, read_update_tx);
        });
        thread::spawn(move || {
            run_terminal_core(
                size,
                pty_rx,
                resize_rx,
                scroll_rx,
                paste_rx,
                key_rx,
                selection_rx,
                response_tx,
                update_tx,
            );
        });
        thread::spawn(move || {
            run_writer(
                master,
                &mut *writer,
                command_rx,
                resize_tx,
                scroll_tx,
                paste_tx,
                key_tx,
                selection_tx,
            );
        });

        Ok(Self {
            tx: command_tx,
            rx: update_rx,
        })
    }

    pub fn sender(&self) -> Sender<TerminalCommand> {
        self.tx.clone()
    }

    pub fn updates(&self) -> Receiver<TerminalUpdate> {
        self.rx.clone()
    }
}

pub fn initial_terminal_text() -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut core = TerminalCore::default();
    core.write_vt(
        format!(
            "Terminal plugin\r\n\r\nPTY backend: portable-pty\r\nVT core: alacritty_terminal\r\nInput encoding: termwiz\r\n\r\nDefault shell: {shell}\r\n\r\nLive PTY session wiring is available in horizon::terminal::TerminalSession.\r\n"
        )
        .as_bytes(),
    );
    core.snapshot_text()
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
    let mut buf = [0_u8; 8192];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
            Ok(read) => {
                if pty_tx.send(buf[..read].to_vec()).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = update_tx.send(TerminalUpdate::Error(error.to_string()));
                return;
            }
        }
    }
}

fn run_terminal_core(
    size: TerminalSize,
    pty_rx: Receiver<Vec<u8>>,
    resize_rx: Receiver<TerminalSize>,
    scroll_rx: Receiver<TerminalScroll>,
    paste_rx: Receiver<String>,
    key_rx: Receiver<(KeyCode, Modifiers, bool)>,
    selection_rx: Receiver<SelectionCommand>,
    command_tx: Sender<TerminalCommand>,
    update_tx: Sender<TerminalUpdate>,
) {
    let mut core = TerminalCore::new(size);

    loop {
        crossbeam_channel::select! {
            recv(resize_rx) -> size => {
                let Ok(size) = size else {
                    return;
                };
                core.resize(size);
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(scroll_rx) -> scroll => {
                let Ok(scroll) = scroll else {
                    return;
                };
                if let Some(input) = core.handle_scroll(scroll) {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(paste_rx) -> text => {
                let Ok(text) = text else {
                    return;
                };
                let _ = command_tx.send(TerminalCommand::Input(core.paste_input(&text)));
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(key_rx) -> key => {
                let Ok((key, modifiers, is_down)) = key else {
                    return;
                };
                let input = core.key_input(key, modifiers, is_down);
                if !input.is_empty() {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(selection_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                match command {
                    SelectionCommand::Start(point) => {
                        core.start_selection(point);
                        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
                    }
                    SelectionCommand::Update(point) => {
                        core.update_selection(point);
                        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
                    }
                    SelectionCommand::Copy => {
                        if let Some(text) = core.selected_text() {
                            let _ = update_tx.send(TerminalUpdate::Clipboard(text));
                        }
                    }
                }
            }
            recv(pty_rx) -> bytes => {
                let Ok(bytes) = bytes else {
                    return;
                };
                let events = core.write_vt(&bytes);
                for bytes in events.pty_writes {
                    let _ = command_tx.send(TerminalCommand::Input(bytes));
                }
                if events.bell_count > 0 {
                    let _ = update_tx.send(TerminalUpdate::Bell);
                }
                if events.title.is_some() {
                    let _ = update_tx.send(TerminalUpdate::Title(events.title));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
        }
    }
}

const DEFAULT_FG: [u8; 3] = [222, 226, 234];
const DEFAULT_BG: [u8; 3] = [24, 27, 32];

const TERMINAL_ENV_REMOVE: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_INTEGRATION_NO_SUDO",
    "GHOSTTY_SHELL_INTEGRATION_XDG_DIR",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_WINDOW_ID",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "VTE_VERSION",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_PROFILE_NAME",
    "KONSOLE_VERSION",
    "TERM_SESSION_ID",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "SSH_TTY",
    "DESKTOP_STARTUP_ID",
    "XDG_ACTIVATION_TOKEN",
];

fn terminal_command(shell: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    configure_terminal_environment(&mut cmd);
    cmd
}

fn configure_terminal_environment(cmd: &mut CommandBuilder) {
    for key in TERMINAL_ENV_REMOVE {
        cmd.env_remove(key);
    }
    cmd.env("TERM", "xterm-kitty");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "horizon");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}

fn push_styled_cell(line: &mut TerminalLine, ch: char, columns: usize, fg: [u8; 3], bg: [u8; 3]) {
    if let Some(last) = line.spans.last_mut() {
        if columns == 0 && last.fg == fg && last.bg == bg {
            last.text.push(ch);
            return;
        }

        if ch == ' ' && columns > 0 && last.text.is_empty() && last.fg == fg && last.bg == bg {
            last.columns += columns;
            return;
        }
    }

    if ch == ' ' && columns > 0 {
        line.spans.push(TerminalSpan {
            text: String::new(),
            columns,
            fg,
            bg,
        });
        return;
    }

    line.spans.push(TerminalSpan {
        text: ch.to_string(),
        columns,
        fg,
        bg,
    });
}

fn cell_width(ch: char, flags: Flags) -> usize {
    if flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        char_width(ch)
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

fn cell_fg(color: AnsiColor, flags: Flags, colors: &Colors) -> [u8; 3] {
    let color = if flags.contains(Flags::BOLD) {
        match color {
            AnsiColor::Named(named) => AnsiColor::Named(named.to_bright()),
            other => other,
        }
    } else if flags.contains(Flags::DIM) {
        match color {
            AnsiColor::Named(named) => AnsiColor::Named(named.to_dim()),
            other => other,
        }
    } else {
        color
    };

    resolve_color(color, colors).unwrap_or(DEFAULT_FG)
}

fn cell_bg(color: AnsiColor, flags: Flags, colors: &Colors) -> [u8; 3] {
    let mut fg = cell_fg(AnsiColor::Named(NamedColor::Foreground), flags, colors);
    let mut bg = resolve_color(color, colors).unwrap_or(DEFAULT_BG);
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    bg
}

fn cursor_position(row: i32, col: usize) -> Option<TerminalCursor> {
    (row >= 0).then_some(TerminalCursor {
        row: row as usize,
        col,
    })
}

fn resolve_color(color: AnsiColor, colors: &Colors) -> Option<[u8; 3]> {
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(index) => colors[index as usize].unwrap_or_else(|| indexed_rgb(index)),
        AnsiColor::Named(named) => colors[named].unwrap_or_else(|| named_rgb(named)),
    };
    Some([rgb.r, rgb.g, rgb.b])
}

fn named_rgb(color: NamedColor) -> Rgb {
    let [r, g, b] = match color {
        NamedColor::Black => [35, 38, 46],
        NamedColor::Red => [224, 108, 117],
        NamedColor::Green => [152, 195, 121],
        NamedColor::Yellow => [229, 192, 123],
        NamedColor::Blue => [97, 175, 239],
        NamedColor::Magenta => [198, 120, 221],
        NamedColor::Cyan => [86, 182, 194],
        NamedColor::White => [222, 226, 234],
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => [95, 99, 112],
        NamedColor::BrightRed | NamedColor::DimRed => [255, 123, 127],
        NamedColor::BrightGreen | NamedColor::DimGreen => [181, 214, 140],
        NamedColor::BrightYellow | NamedColor::DimYellow => [245, 211, 139],
        NamedColor::BrightBlue | NamedColor::DimBlue => [120, 194, 255],
        NamedColor::BrightMagenta | NamedColor::DimMagenta => [218, 140, 255],
        NamedColor::BrightCyan | NamedColor::DimCyan => [103, 205, 216],
        NamedColor::BrightWhite => [255, 255, 255],
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            DEFAULT_FG
        }
        NamedColor::Background => DEFAULT_BG,
        NamedColor::Cursor => [132, 220, 198],
    };
    Rgb { r, g, b }
}

fn indexed_rgb(index: u8) -> Rgb {
    if index < 16 {
        return named_rgb(match index {
            0 => NamedColor::Black,
            1 => NamedColor::Red,
            2 => NamedColor::Green,
            3 => NamedColor::Yellow,
            4 => NamedColor::Blue,
            5 => NamedColor::Magenta,
            6 => NamedColor::Cyan,
            7 => NamedColor::White,
            8 => NamedColor::BrightBlack,
            9 => NamedColor::BrightRed,
            10 => NamedColor::BrightGreen,
            11 => NamedColor::BrightYellow,
            12 => NamedColor::BrightBlue,
            13 => NamedColor::BrightMagenta,
            14 => NamedColor::BrightCyan,
            _ => NamedColor::BrightWhite,
        });
    }

    if index < 232 {
        let index = index - 16;
        let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return Rgb {
            r: component(index / 36),
            g: component((index / 6) % 6),
            b: component(index % 6),
        };
    }

    let gray = 8 + (index - 232) * 10;
    Rgb {
        r: gray,
        g: gray,
        b: gray,
    }
}

fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    command_rx: Receiver<TerminalCommand>,
    resize_tx: Sender<TerminalSize>,
    scroll_tx: Sender<TerminalScroll>,
    paste_tx: Sender<String>,
    key_tx: Sender<(KeyCode, Modifiers, bool)>,
    selection_tx: Sender<SelectionCommand>,
) {
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                is_down,
            } => {
                let _ = key_tx.send((key, modifiers, is_down));
            }
            TerminalCommand::Paste(text) => {
                let _ = paste_tx.send(text);
            }
            TerminalCommand::Resize(size) => {
                let _ = master.resize(PtySize {
                    rows: size.rows,
                    cols: size.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                let _ = resize_tx.send(size);
            }
            TerminalCommand::Scroll(scroll) => {
                let _ = scroll_tx.send(scroll);
            }
            TerminalCommand::SelectionStart(point) => {
                let _ = selection_tx.send(SelectionCommand::Start(point));
            }
            TerminalCommand::SelectionUpdate(point) => {
                let _ = selection_tx.send(SelectionCommand::Update(point));
            }
            TerminalCommand::CopySelection => {
                let _ = selection_tx.send(SelectionCommand::Copy);
            }
            TerminalCommand::Shutdown => return,
        }
    }
}

enum SelectionCommand {
    Start(TerminalSelectionPoint),
    Update(TerminalSelectionPoint),
    Copy,
}

fn kitty_flags_from_mode(mode: TermMode) -> KittyKeyboardFlags {
    let mut flags = KittyKeyboardFlags::NONE;

    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        flags |= KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        flags |= KittyKeyboardFlags::REPORT_EVENT_TYPES;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        flags |= KittyKeyboardFlags::REPORT_ALTERNATE_KEYS;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        flags |= KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        flags |= KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
    }

    flags
}

fn arrow_scroll_input(lines: i32) -> Vec<u8> {
    let sequence = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::with_capacity(sequence.len() * repeat);
    for _ in 0..repeat {
        input.extend_from_slice(sequence);
    }
    input
}

fn sgr_mouse_wheel_input(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let button = if lines > 0 { 64 } else { 65 };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::new();
    for _ in 0..repeat {
        input.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
    }
    input
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn terminal_intro_mentions_backends() {
        let text = initial_terminal_text();
        assert!(text.contains("portable-pty"));
        assert!(text.contains("alacritty_terminal"));
        assert!(text.contains("termwiz"));
    }

    #[test]
    fn vt_stream_updates_snapshot() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"hello\r\n\x1b[31mred\x1b[0m");

        let snapshot = core.snapshot_text();
        assert!(snapshot.contains("hello"));
        assert!(snapshot.contains("red"));
    }

    #[test]
    fn kitty_keyboard_mode_switches_termwiz_encoding() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"\x1b[>1u");

        let encoded = core.encode_key(KeyCode::Escape, Modifiers::NONE, true);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn key_up_events_do_not_emit_legacy_input() {
        let core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        let encoded = core.encode_key(KeyCode::Char('a'), Modifiers::NONE, false);
        assert_eq!(encoded, "");
    }

    #[test]
    fn terminal_session_runs_shell_command() {
        let session = TerminalSession::spawn(TerminalSize { cols: 80, rows: 12 })
            .expect("terminal session should spawn");
        let tx = session.sender();
        let rx = session.updates();

        tx.send(TerminalCommand::Input(
            b"printf horizon-terminal-ok\\n\r".to_vec(),
        ))
        .expect("input should be sent");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_output = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(TerminalUpdate::Snapshot(snapshot)) => {
                    if snapshot.text.contains("horizon-terminal-ok") {
                        saw_output = true;
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }

        let _ = tx.send(TerminalCommand::Input(b"exit\r".to_vec()));
        let _ = tx.send(TerminalCommand::Shutdown);

        assert!(saw_output, "terminal session did not render shell output");
    }

    #[test]
    fn vt_stream_preserves_ansi_foreground_color() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"\x1b[31mred\x1b[0m plain");

        let frame = core.snapshot_frame();
        assert!(frame.text.contains("red plain"));
        let first_line = &frame.lines[0];
        assert!(first_line
            .spans
            .iter()
            .any(|span| { span.text == "r" && span.fg == [224, 108, 117] }));
        assert!(first_line
            .spans
            .iter()
            .any(|span| { span.text == "p" && span.fg == DEFAULT_FG }));
    }

    #[test]
    fn vt_stream_tracks_wide_character_columns() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt("日本語".as_bytes());

        let frame = core.snapshot_frame();
        assert!(frame.text.contains("日本語"));
        assert_eq!(frame.text.lines().next(), Some("日本語"));
        assert_eq!(frame.cursor.map(|cursor| cursor.col), Some(6));
        assert!(frame.lines[0]
            .spans
            .iter()
            .any(|span| span.text == "日" && span.columns == 2));
    }

    #[test]
    fn scroll_display_uses_alacritty_history() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");

        let bottom = core.snapshot_text();
        assert!(bottom.contains("seven"));
        assert_eq!(core.display_offset(), 0);

        assert_eq!(core.handle_scroll(test_scroll(3)), None);
        let history = core.snapshot_text();
        assert!(!history.contains("seven"));
        assert!(core.display_offset() > 0);

        assert_eq!(core.handle_scroll(test_scroll(-3)), None);
        assert_eq!(core.display_offset(), 0);
    }

    #[test]
    fn scroll_in_alternate_screen_sends_application_input() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"\x1b[?1049h");

        assert!(core.alternate_screen());
        assert_eq!(
            core.handle_scroll(test_scroll(2)),
            Some(b"\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(core.display_offset(), 0);
    }

    #[test]
    fn sgr_mouse_mode_scroll_sends_wheel_reports() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"\x1b[?1000h\x1b[?1006h");

        assert_eq!(
            core.handle_scroll(TerminalScroll {
                lines: -1,
                point: TerminalSelectionPoint { row: 4, col: 7 },
            }),
            Some(b"\x1b[<65;8;5M".to_vec())
        );
        assert_eq!(core.display_offset(), 0);
    }

    #[test]
    fn paste_is_plain_text_by_default() {
        let core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });

        assert_eq!(core.paste_input("hello\n"), b"hello\n".to_vec());
    }

    #[test]
    fn paste_wraps_text_when_bracketed_paste_is_enabled() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"\x1b[?2004h");

        assert_eq!(
            core.paste_input("hello\n"),
            b"\x1b[200~hello\n\x1b[201~".to_vec()
        );
    }

    #[test]
    fn selection_to_string_uses_alacritty_selection() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"hello world");

        core.start_selection(TerminalSelectionPoint { row: 0, col: 0 });
        core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });

        assert_eq!(core.selected_text(), Some("hello".to_string()));
    }

    #[test]
    fn terminal_command_sanitizes_emulator_environment() {
        let cmd = terminal_command("/bin/sh");

        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some("xterm-kitty")
        );
        assert_eq!(
            cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
            Some("truecolor")
        );
        assert_eq!(
            cmd.get_env("TERM_PROGRAM").and_then(|v| v.to_str()),
            Some("horizon")
        );
        assert_eq!(
            cmd.get_env("TERM_PROGRAM_VERSION").and_then(|v| v.to_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(cmd.get_env("GHOSTTY_RESOURCES_DIR"), None);
        assert_eq!(cmd.get_env("KITTY_LISTEN_ON"), None);
        assert_eq!(cmd.get_env("WEZTERM_PANE"), None);
        assert_eq!(cmd.get_env("TMUX"), None);
    }

    fn test_scroll(lines: i32) -> TerminalScroll {
        TerminalScroll {
            lines,
            point: TerminalSelectionPoint { row: 0, col: 0 },
        }
    }
}
