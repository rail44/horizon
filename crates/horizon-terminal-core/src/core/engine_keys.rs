//! SPIKE: the engine's own key encoder beside Horizon's, key by key.
//!
//! Not a test of either: a count of where the two write the same bytes
//! and where they do not, over keys, modifiers, kinds of event and the
//! modes a program can ask for.
//! `cargo nextest run -p horizon-terminal-core --run-ignored only
//! the_engine_s_key_encoder_beside_horizon_s --no-capture`

use libghostty_vt::key::{Action, Encoder, Event, Key, Mods};
use termwiz::input::{KeyCode, Modifiers};

use super::TerminalCore;
use crate::types::{KeyEventKind, TerminalSize};

/// The engine's key for one of the wire's, with the character an
/// unshifted press of it makes.
fn engine_key(key: KeyCode) -> Option<(Key, Option<char>)> {
    let letters = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    let digits = [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ];
    let functions = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    Some(match key {
        KeyCode::Char(c) if c.is_ascii_alphabetic() => {
            let lower = c.to_ascii_lowercase();
            (letters[(lower as u8 - b'a') as usize], Some(lower))
        }
        KeyCode::Char(c) if c.is_ascii_digit() => (digits[(c as u8 - b'0') as usize], Some(c)),
        KeyCode::Char(' ') => (Key::Space, Some(' ')),
        KeyCode::Char('-') => (Key::Minus, Some('-')),
        KeyCode::Char('[') => (Key::BracketLeft, Some('[')),
        KeyCode::Char(';') => (Key::Semicolon, Some(';')),
        KeyCode::Char('/') => (Key::Slash, Some('/')),
        KeyCode::Enter => (Key::Enter, None),
        KeyCode::Tab => (Key::Tab, None),
        KeyCode::Backspace => (Key::Backspace, None),
        KeyCode::Escape => (Key::Escape, None),
        KeyCode::Delete => (Key::Delete, None),
        KeyCode::Insert => (Key::Insert, None),
        KeyCode::UpArrow => (Key::ArrowUp, None),
        KeyCode::DownArrow => (Key::ArrowDown, None),
        KeyCode::LeftArrow => (Key::ArrowLeft, None),
        KeyCode::RightArrow => (Key::ArrowRight, None),
        KeyCode::Home => (Key::Home, None),
        KeyCode::End => (Key::End, None),
        KeyCode::PageUp => (Key::PageUp, None),
        KeyCode::PageDown => (Key::PageDown, None),
        KeyCode::Function(n @ 1..=12) => (functions[n as usize - 1], None),
        _ => return None,
    })
}

impl TerminalCore {
    /// What the engine's encoder writes for a key, the modes read from the
    /// terminal as the engine has them.
    fn engine_key_input(
        &self,
        key: KeyCode,
        mods: Modifiers,
        kind: KeyEventKind,
        text: Option<&str>,
    ) -> Option<Vec<u8>> {
        let (engine, unshifted) = engine_key(key)?;
        let mut encoder = Encoder::new().ok()?;
        encoder.set_options_from_terminal(&self.terminal);
        let mut event = Event::new().ok()?;
        event.set_action(match kind {
            KeyEventKind::Press => Action::Press,
            KeyEventKind::Repeat => Action::Repeat,
            KeyEventKind::Release => Action::Release,
        });
        event.set_key(engine);
        let mut held = Mods::empty();
        for (wire, engine) in [
            (Modifiers::SHIFT, Mods::SHIFT),
            (Modifiers::CTRL, Mods::CTRL),
            (Modifiers::ALT, Mods::ALT),
            (Modifiers::SUPER, Mods::SUPER),
        ] {
            if mods.contains(wire) {
                held |= engine;
            }
        }
        event.set_mods(held);
        // Shift that went into making the text is not a modifier of the
        // key any more: the platform says so, and so does this.
        if text.is_some() && held.contains(Mods::SHIFT) {
            event.set_consumed_mods(Mods::SHIFT);
        }
        event.set_utf8(text);
        if let Some(unshifted) = unshifted {
            event.set_unshifted_codepoint(unshifted);
        }
        let mut written = Vec::new();
        encoder.encode_to_vec(&event, &mut written).ok()?;
        Some(written)
    }
}

#[test]
#[ignore = "a count for the spike's record, not a check"]
fn the_engine_s_key_encoder_beside_horizon_s() {
    let keys = [
        KeyCode::Char('a'),
        KeyCode::Char('c'),
        KeyCode::Char('m'),
        KeyCode::Char('z'),
        KeyCode::Char('1'),
        KeyCode::Char('0'),
        KeyCode::Char(' '),
        KeyCode::Char('-'),
        KeyCode::Char('['),
        KeyCode::Char(';'),
        KeyCode::Char('/'),
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::Backspace,
        KeyCode::Escape,
        KeyCode::Delete,
        KeyCode::Insert,
        KeyCode::UpArrow,
        KeyCode::DownArrow,
        KeyCode::LeftArrow,
        KeyCode::RightArrow,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Function(1),
        KeyCode::Function(5),
        KeyCode::Function(12),
    ];
    let mods = [
        Modifiers::NONE,
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::CTRL | Modifiers::SHIFT,
        Modifiers::ALT | Modifiers::SHIFT,
        Modifiers::CTRL | Modifiers::ALT,
        Modifiers::SUPER,
    ];
    let kinds = [
        KeyEventKind::Press,
        KeyEventKind::Repeat,
        KeyEventKind::Release,
    ];
    // What a program asks for before the keys: nothing, application
    // cursor keys, and the kitty protocol's flags in the sets programs use.
    let modes: [(&str, &[u8]); 7] = [
        ("legacy", b""),
        ("legacy, application cursor keys", b"\x1b[?1h"),
        ("kitty 1 (disambiguate)", b"\x1b[>1u"),
        ("kitty 3 (+ event types)", b"\x1b[>3u"),
        ("kitty 7 (+ alternate keys)", b"\x1b[>7u"),
        ("kitty 15 (+ all keys as escape codes)", b"\x1b[>15u"),
        ("kitty 31 (+ associated text)", b"\x1b[>31u"),
    ];

    let (mut all, mut same) = (0, 0);
    for (name, asked) in modes {
        let mut core = TerminalCore::new(TerminalSize::new(80, 24));
        core.write_vt(asked);
        let (mut of_mode, mut same_in_mode) = (0, 0);
        let mut differing = Vec::new();
        for key in keys {
            for held in mods {
                for kind in kinds {
                    // The text a press makes: the character, shifted where
                    // Shift is held, and none where Ctrl, Alt or Super is.
                    let chord =
                        held.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER);
                    let text = match key {
                        KeyCode::Char(c) if !chord && kind != KeyEventKind::Release => {
                            Some(match held.contains(Modifiers::SHIFT) {
                                true => c.to_ascii_uppercase().to_string(),
                                false => c.to_string(),
                            })
                        }
                        _ => None,
                    };
                    let own = core.key_input(key, held, kind, text.as_deref());
                    let Some(engine) = core.engine_key_input(key, held, kind, text.as_deref())
                    else {
                        continue;
                    };
                    of_mode += 1;
                    if own == engine {
                        same_in_mode += 1;
                    } else if differing.len() < 12 {
                        differing.push(format!(
                            "      {key:?} {held:?} {kind:?}: Horizon {:?}, engine {:?}",
                            String::from_utf8_lossy(&own),
                            String::from_utf8_lossy(&engine)
                        ));
                    }
                }
            }
        }
        println!("{name}: {same_in_mode} of {of_mode} the same");
        for line in differing {
            println!("{line}");
        }
        all += of_mode;
        same += same_in_mode;
    }
    println!("in all: {same} of {all} the same");
}

#[test]
#[ignore = "a timing for the spike's record, not a check"]
fn how_long_a_frame_takes_to_read_out_of_the_engine() {
    use std::time::Instant;

    for (columns, rows) in [(120u16, 40u16), (240, 70)] {
        let mut core = TerminalCore::new(TerminalSize::new(columns, rows));
        // Every cell written, a color change every few cells, and enough
        // of it that there is history behind the screen.
        let mut text = String::new();
        for line in 0..(rows as usize * 4) {
            for column in 0..columns as usize {
                if column % 7 == 0 {
                    text += &format!("\x1b[3{}m", (line + column) % 8);
                }
                text.push((b'!' + ((line * 31 + column * 7) % 90) as u8) as char);
            }
            text += "\r\n";
        }
        let started = Instant::now();
        core.write_vt(text.as_bytes());
        let written = started.elapsed();

        let median = |core: &mut TerminalCore, before: &dyn Fn(&mut TerminalCore, usize)| {
            let mut times = Vec::new();
            for round in 0..41 {
                before(core, round);
                let started = Instant::now();
                let frame = core.snapshot_frame();
                times.push(started.elapsed());
                assert_eq!(frame.lines.len(), rows as usize);
            }
            times.sort();
            times[20]
        };
        // Nothing written between frames; one row of the screen written
        // over; and every row written again.
        let unchanged = median(&mut core, &|_, _| {});
        let one_row = median(&mut core, &|core, round| {
            core.write_vt(format!("\x1b[5;1Hrow five, round {round}").as_bytes());
        });
        let every_row = median(&mut core, &|core, round| {
            let mut screen = String::from("\x1b[H");
            for line in 0..rows as usize {
                screen += &format!("\x1b[{};1H", line + 1);
                for column in 0..columns as usize {
                    screen.push((b'!' + ((round + line * 31 + column * 7) % 90) as u8) as char);
                }
            }
            core.write_vt(screen.as_bytes());
        });
        let times = [unchanged; 21];
        let mut window = Vec::new();
        for _ in 0..41 {
            let started = Instant::now();
            let served = core.snapshot_window(rows as usize, rows as usize * 3);
            window.push(started.elapsed());
            assert!(served.lines.len() >= rows as usize);
        }
        window.sort();
        println!(
            "{columns}x{rows}: writing {} bytes {:?}; a frame with nothing changed {:?}, one row {one_row:?}, every row {every_row:?} (medians of 41); a window of {} rows {:?}",
            text.len(),
            written,
            times[20],
            core.snapshot_window(rows as usize, rows as usize * 3)
                .lines
                .len(),
            window[20],
        );
    }
}

#[test]
#[ignore = "an observation for the spike's record, not a check"]
fn what_the_engine_says_of_rows_as_output_scrolls() {
    use libghostty_vt::Terminal;
    use libghostty_vt::render::{RenderState, RowIterator};

    let mut terminal: Terminal<'static, 'static> = Terminal::new(40, 10).unwrap();
    let mut state: RenderState<'static> = RenderState::new().unwrap();
    let mut rows: RowIterator<'static> = RowIterator::new().unwrap();
    // The ids of the viewport's rows top to bottom, and which are marked
    // changed; everything is then marked clean.
    let mut read = |terminal: &Terminal<'static, 'static>| {
        let snapshot = state.update(terminal).unwrap();
        let whole = snapshot.dirty().unwrap();
        let mut seen = Vec::new();
        let mut iteration = rows.update(&snapshot).unwrap();
        while let Some(row) = iteration.next() {
            seen.push((row.id().unwrap(), row.dirty().unwrap()));
            row.set_dirty(false).unwrap();
        }
        snapshot
            .set_dirty(libghostty_vt::render::Dirty::Clean)
            .unwrap();
        (whole, seen)
    };
    let describe = |what: &str,
                    before: &[(libghostty_vt::render::RowId, bool)],
                    after: &(
        libghostty_vt::render::Dirty,
        Vec<(libghostty_vt::render::RowId, bool)>,
    )| {
        let (whole, rows) = after;
        let changed = rows.iter().filter(|(_, changed)| *changed).count();
        let kept = (rows.iter())
            .filter(|(id, _)| before.iter().any(|(old, _)| old == id))
            .count();
        // Whether the rows kept are the old ones moved up by one.
        let moved_up = rows.len() > 1
            && (0..rows.len() - 1).all(|at| {
                before
                    .get(at + 1)
                    .is_some_and(|(old, _)| *old == rows[at].0)
            });
        let in_place =
            (0..rows.len()).all(|at| before.get(at).is_some_and(|(old, _)| *old == rows[at].0));
        println!(
            "{what}: whole {whole:?}; {changed} of {} rows marked changed; {kept} ids seen before; same places {in_place}; moved up one {moved_up}",
            rows.len()
        );
    };

    for line in 0..10 {
        terminal.vt_write(format!("line {line}\r\n").as_bytes());
    }
    let (_, first) = read(&terminal);

    let nothing = read(&terminal);
    describe("nothing written", &first, &nothing);

    terminal.vt_write(b"\x1b[3;1Hrow three written over");
    let one = read(&terminal);
    describe("one row written over", &nothing.1, &one);

    terminal.vt_write(b"\x1b[10;1H\r\none more line");
    let scrolled = read(&terminal);
    describe(
        "one line of output, the screen moves up one",
        &one.1,
        &scrolled,
    );

    terminal.vt_write(b"\r\nand another");
    let again = read(&terminal);
    describe("another line of output", &scrolled.1, &again);

    for line in 0..5 {
        terminal.vt_write(format!("\r\nburst {line}").as_bytes());
    }
    let burst = read(&terminal);
    describe("five lines of output at once", &again.1, &burst);

    terminal.vt_write(b"\x1b[2;8r\x1b[8;1H\r\nwithin a region\x1b[r");
    let region = read(&terminal);
    describe("a scroll within rows 2 to 8", &burst.1, &region);
}

#[test]
#[ignore = "sizes for the spike's record, not a check"]
fn how_much_crosses_to_a_view() {
    use std::time::Instant;

    use libghostty_vt::Terminal;
    use libghostty_vt::snapshot::Decoder;

    // A frame as the wire has it today, in the wire's own codec.
    fn frame_bytes(frame: &crate::types::TerminalFrame) -> usize {
        postbag_len(frame)
    }
    fn postbag_len<T: serde::Serialize>(value: &T) -> usize {
        let mut bytes = Vec::new();
        let _outside = remoc::codec::allow_outside();
        <horizon_wire::WireCodec as remoc::codec::Codec>::serialize(&mut bytes, value).unwrap();
        bytes.len()
    }

    for (columns, rows) in [(120u16, 40u16), (240, 70)] {
        // Plain text; text that changes color every few cells; and a
        // color for every cell.
        let screens: [(&str, Box<dyn Fn(usize, usize) -> String>); 3] = [
            ("plain", Box::new(|_, _| String::new())),
            (
                "a color every 7 cells",
                Box::new(|line, column| match column % 7 {
                    0 => format!("\x1b[3{}m", (line + column) % 8),
                    _ => String::new(),
                }),
            ),
            (
                "a color for every cell",
                Box::new(|line, column| {
                    format!(
                        "\x1b[38;2;{};{};{}m",
                        line % 256,
                        column % 256,
                        (line + column) % 256
                    )
                }),
            ),
        ];
        for (name, style) in screens {
            let mut core = TerminalCore::new(TerminalSize::new(columns, rows));
            let mut one_screen = String::from("\x1b[H");
            for line in 0..rows as usize {
                one_screen += &format!("\x1b[{};1H", line + 1);
                for column in 0..columns as usize {
                    one_screen += &style(line, column);
                    one_screen.push((b'!' + ((line * 31 + column * 7) % 90) as u8) as char);
                }
            }
            core.write_vt(one_screen.as_bytes());
            println!(
                "{columns}x{rows}, {name}: the program wrote {} bytes for the screen; the frame is {} bytes on the wire",
                one_screen.len(),
                frame_bytes(&core.snapshot_frame()),
            );
        }
    }

    // The whole state of a terminal with a full history, as it would be
    // handed to a view that attaches.
    for (columns, rows, history) in [(120u16, 40u16, 10_000usize), (240, 70, 10_000)] {
        let mut terminal: Terminal<'static, 'static> = Terminal::new(columns, rows).unwrap();
        terminal.set_scrollback_max_bytes(Some(usize::MAX)).unwrap();
        terminal.set_scrollback_max_lines(Some(history)).unwrap();
        let mut written = 0usize;
        for line in 0..history + rows as usize {
            let mut text = String::new();
            for column in 0..(columns as usize * 2 / 3) {
                if column % 11 == 0 {
                    text += &format!("\x1b[3{}m", (line + column) % 8);
                }
                text.push((b'!' + ((line * 31 + column * 7) % 90) as u8) as char);
            }
            text += "\r\n";
            written += text.len();
            terminal.vt_write(text.as_bytes());
        }
        let started = Instant::now();
        let mut state = Vec::new();
        terminal.encode_snapshot(&mut state).unwrap();
        let encoded = started.elapsed();
        let started = Instant::now();
        let restored = Decoder::new_buf(&state).unwrap().decode().unwrap();
        let decoded = started.elapsed();
        println!(
            "{columns}x{rows} with {} rows of history ({written} bytes written): the state is {} bytes, written out in {encoded:?}, read back in {decoded:?}; {} rows of history read back",
            terminal.scrollback_rows().unwrap(),
            state.len(),
            restored.scrollback_rows().unwrap(),
        );
    }
}
