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
