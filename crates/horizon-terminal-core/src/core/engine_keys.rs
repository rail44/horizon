//! SPIKE: the engine's own key encoder beside Horizon's, key by key.
//!
//! Not a test of either: a count of where the two write the same bytes
//! and where they do not, over keys, modifiers, kinds of event and the
//! modes a program can ask for.
//! `cargo nextest run -p horizon-terminal-core --run-ignored only
//! the_engine_s_key_encoder_beside_horizon_s --no-capture`

use termwiz::input::{KeyCode, Modifiers};

use super::TerminalCore;
use crate::types::{KeyEventKind, TerminalSize};

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
        self.key_stroke(&super::keys::KeyStroke {
            code: "",
            key,
            modifiers: mods,
            kind,
            made: text,
        })
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
        type Screen = Box<dyn Fn(usize, usize) -> String>;
        let screens: [(&str, Screen); 3] = [
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

#[test]
#[ignore = "an observation for the spike's record, not a check"]
fn a_state_handed_over_and_the_bytes_after_it_make_the_same_terminal() {
    use libghostty_vt::Terminal;
    use libghostty_vt::fmt::{Extras, Format, Formatter};
    use libghostty_vt::mode::Mode;
    use libghostty_vt::snapshot::Decoder;

    type Engine = Terminal<'static, 'static>;

    // What a program might write: text in colors, the cursor moved about,
    // the screen cleared, the other screen entered and left, a title,
    // wide characters, a scroll region. The same bytes every run.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |below: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % below
    };
    let mut script = Vec::new();
    for line in 0..6000u64 {
        match next(40) {
            0 => script.extend_from_slice(b"\x1b[2J\x1b[H"),
            1 => script.extend_from_slice(b"\x1b[?1049h\x1b[2;2Hon the other screen\x1b[?1049l"),
            2 => script.extend_from_slice(format!("\x1b]0;title {line}\x07").as_bytes()),
            3 => script.extend_from_slice(b"\x1b[3;10r\x1b[10;1H\n\n\x1b[r"),
            4 => script.extend_from_slice("日本語の行 ｶﾅ 🙂 e\u{301}".as_bytes()),
            5 => script.extend_from_slice(b"\x1b[?2004h\x1b[?1h\x1b[>5u"),
            6 => script.extend_from_slice(b"\x1b[?2004l\x1b[?1l\x1b[<u"),
            7 => script
                .extend_from_slice(format!("\x1b[{};{}H", 1 + next(24), 1 + next(80)).as_bytes()),
            _ => {}
        }
        for word in 0..next(12) {
            script.extend_from_slice(
                format!(
                    "\x1b[{};4{}m word{word} \x1b[38;2;{};{};{}m{line}\x1b[0m",
                    30 + next(8),
                    next(8),
                    next(256),
                    next(256),
                    next(256)
                )
                .as_bytes(),
            );
        }
        script.extend_from_slice(b"\r\n");
    }

    let new = || -> Engine {
        let mut terminal = Terminal::new(80, 24).unwrap();
        terminal.set_scrollback_max_bytes(Some(usize::MAX)).unwrap();
        terminal.set_scrollback_max_lines(Some(2000)).unwrap();
        // So that a state can be taken with an escape sequence half read.
        terminal.set_continuation_max_bytes(4096).unwrap();
        terminal
    };
    // Everything of a terminal that can be said in text.
    let said = |terminal: &Engine| {
        let mut all = Vec::new();
        for format in [Format::Vt, Format::Plain] {
            let mut formatter = Formatter::new(terminal).unwrap();
            formatter.set_format(format).unwrap();
            formatter.set_extras(Extras::all()).unwrap();
            let length = formatter.format_len().unwrap();
            let mut text = vec![0u8; length];
            let written = formatter.format_buf(&mut text).unwrap();
            text.truncate(written);
            all.push(text);
        }
        (
            all,
            terminal.cursor_x().unwrap(),
            terminal.cursor_y().unwrap(),
            terminal.scrollback_rows().unwrap(),
            terminal.title().unwrap().to_vec(),
            terminal.kitty_keyboard_flags().unwrap().bits(),
            [Mode::DECCKM, Mode::BRACKETED_PASTE, Mode::ALT_SCREEN_SAVE]
                .map(|mode| terminal.mode(mode).unwrap()),
        )
    };

    let mut daemon = new();
    let mut views: Vec<(usize, bool, usize, Engine)> = Vec::new();
    let mut at = 0;
    let mut until_handover = 0u64;
    while at < script.len() {
        // A read of the PTY ends anywhere: in the middle of an escape
        // sequence, in the middle of a character.
        let length = (1 + next(300) as usize).min(script.len() - at);
        let chunk = &script[at..at + length];
        daemon.vt_write(chunk);
        for (.., view) in &mut views {
            view.vt_write(chunk);
        }
        at += length;
        if until_handover == 0 {
            until_handover = 40 + next(200);
            let mut handed = Vec::new();
            let half_read = !daemon.is_vt_ground().unwrap();
            daemon.encode_snapshot(&mut handed).unwrap();
            let mut decoder = Decoder::new_buf(&handed).unwrap();
            decoder.set_retain_continuation(true).unwrap();
            let view = decoder.decode().unwrap();
            views.push((at, half_read, handed.len(), view));
        }
        until_handover -= 1;
    }

    let expected = said(&daemon);
    let same = views
        .iter()
        .filter(|(.., view)| said(view) == expected)
        .count();
    let half_read = views.iter().filter(|(_, half_read, ..)| *half_read).count();
    let same_half_read = (views.iter())
        .filter(|(_, half_read, _, view)| *half_read && said(view) == expected)
        .count();
    println!(
        "{} bytes written in reads of 1 to 300; a state handed over {} times, {half_read} of them with an escape sequence or a character half read",
        script.len(),
        views.len(),
    );
    println!(
        "at the end, {same} of {} views say what the daemon says ({same_half_read} of the {half_read} half-read ones)",
        views.len()
    );
    // Where they differ: is it only in how much of the oldest history is
    // kept? The text both have, from the end back, is then the same.
    let same_where_both_have = (views.iter())
        .filter(|(.., view)| {
            let got = said(view);
            let (a, b) = (&expected.0[1], &got.0[1]);
            let common = a.len().min(b.len());
            a[a.len() - common..] == b[b.len() - common..]
                && (got.1, got.2, &got.4, got.5, got.6)
                    == (expected.1, expected.2, &expected.4, expected.5, expected.6)
        })
        .count();
    println!(
        "{same_where_both_have} of {} views say the same as the daemon for the text both have, the cursor, the title and the modes",
        views.len()
    );
    for (at, half_read, _, view) in views
        .iter()
        .filter(|(.., view)| said(view) != expected)
        .take(0)
    {
        let got = said(view);
        let first_difference = |a: &[u8], b: &[u8]| {
            let at = a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .unwrap_or(a.len().min(b.len()));
            let around = |text: &[u8]| {
                String::from_utf8_lossy(&text[at.saturating_sub(30)..(at + 40).min(text.len())])
                    .into_owned()
            };
            format!(
                "at {at} of {}/{}: daemon {:?}, view {:?}",
                a.len(),
                b.len(),
                around(a),
                around(b)
            )
        };
        println!(
            "handed over at byte {at} (half read: {half_read}): as VT {}; as text {}; cursor {}; history {} against {}; title {}; kitty flags {}; modes {}",
            got.0[0] == expected.0[0],
            got.0[1] == expected.0[1],
            (got.1, got.2) == (expected.1, expected.2),
            got.3,
            expected.3,
            got.4 == expected.4,
            got.5 == expected.5,
            got.6 == expected.6,
        );
        if got.0[1] != expected.0[1] {
            println!("   text: {}", first_difference(&expected.0[1], &got.0[1]));
        } else if got.0[0] != expected.0[0] {
            println!("   VT: {}", first_difference(&expected.0[0], &got.0[0]));
        }
    }

    // How much of a handed-over state is the screen, which a view can draw
    // from before the history has come.
    let mut handed = Vec::new();
    daemon.encode_snapshot(&mut handed).unwrap();
    let decoder = Decoder::new_buf(&handed).unwrap();
    let mut incremental = decoder.ready().unwrap();
    let screen = incremental.decoder().source_offset().unwrap();
    let rows_at_ready = incremental.terminal().scrollback_rows().unwrap();
    let mut pages = 0;
    while incremental.next().unwrap().is_some() {
        pages += 1;
    }
    println!(
        "a state of {} bytes: ready to draw after {screen} of them, with {rows_at_ready} rows of history; the rest in {pages} steps, to {} rows",
        handed.len(),
        incremental.terminal().scrollback_rows().unwrap(),
    );
    // And cut off there: is what was ready a terminal that goes on?
    let cut = &handed[..screen];
    match Decoder::new_buf(cut).and_then(Decoder::ready) {
        Ok(ready) => {
            let mut terminal = ready.into_terminal();
            terminal.vt_write(b"after the cut");
            println!(
                "cut off after the screen: a terminal with {} rows of history that takes bytes",
                terminal.scrollback_rows().unwrap()
            );
        }
        Err(error) => println!("cut off after the screen: {error:?}"),
    }
}

#[test]
fn a_state_cut_off_after_the_screen_is_a_terminal_that_goes_on() {
    let mut core = TerminalCore::new(TerminalSize::new(80, 24));
    for line in 0..5000 {
        core.write_vt(format!("line {line}\r\n").as_bytes());
    }
    let whole = core.state();
    let screen = core.state_of_the_screen();
    assert!(
        screen.len() < whole.len(),
        "{} of {}",
        screen.len(),
        whole.len()
    );
    let mut view = TerminalCore::from_state(&screen).expect("a terminal from the screen's part");
    assert!(view.snapshot_text().contains("line 4999"));
    core.write_vt(b"and then more");
    view.take_in(b"and then more");
    assert_eq!(view.snapshot_text(), core.snapshot_text());
}

/// A state written out between any two bytes of what a program writes --
/// in the middle of an escape sequence, of a character -- and the bytes
/// after it make the terminal the whole makes.
#[test]
fn a_state_taken_between_any_two_bytes_goes_on_to_the_same_screen() {
    let written = "$ typed ahead\r\n\x1b[H\x1b[2J\x1b[4;58;2;255;0;0munders\x1b[0m\r\n\
                   \x1b[9;38;2;0;0;255mstruck\x1b[0m\r\n日本語 \u{2588}\u{2588}\r\n\
                   \x1b]0;a title\x07\x1b[4:3mcurled\x1b[0m\r\n$ "
        .as_bytes();
    let mut whole = TerminalCore::new(TerminalSize::new(80, 24));
    whole.write_vt(written);
    let (text, frame) = (whole.snapshot_text(), whole.snapshot_frame());
    let mut differing = Vec::new();
    for at in 0..=written.len() {
        let mut daemon = TerminalCore::new(TerminalSize::new(80, 24));
        daemon.write_vt(&written[..at]);
        let mut view = TerminalCore::from_state(&daemon.state()).expect("a view's terminal");
        // What the view is sent is what the daemon's engine took in: the
        // core holds the start of a sequence back until it has seen
        // whether it is one of its own (a notification), so the engine
        // has not had those bytes when the state is written, and has them
        // with the bytes after.
        let after = daemon.write_vt(&written[at..]).output;
        view.take_in(&after);
        if view.snapshot_text() != text || view.snapshot_frame().lines != frame.lines {
            differing.push(at);
        }
    }
    assert!(
        differing.is_empty(),
        "split at {differing:?} of {}",
        written.len()
    );
}
