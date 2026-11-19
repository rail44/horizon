//! SPIKE Y. A key written for the PTY by the engine's own encoder.
//!
//! The encoder takes a key as a platform reports it: the physical key,
//! the modifiers held, the text the press made and which modifiers went
//! into making it, and the character the key makes with no Shift. A view
//! has those from its key event and gives them here ([`KeyStroke`]).

use libghostty_vt::key::{Action, Encoder, Event, Key, Mods};
use termwiz::input::{KeyCode, Modifiers};

use super::TerminalCore;
use crate::types::KeyEventKind;

/// A key event as a view has it.
pub struct KeyStroke<'a> {
    /// The physical key by its name in the UI Events specification
    /// (`KeyA`, `Digit1`, `ArrowUp`), or empty where the platform did not
    /// say.
    pub code: &'a str,
    /// The key as the layout has it.
    pub key: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,
    /// The text the key makes with the Shift that is held, whatever else
    /// is held with it.
    pub made: Option<&'a str>,
}

/// The engine's key for a physical key's name.
fn key_coded(code: &str) -> Option<Key> {
    Some(match code {
        "KeyA" => Key::A,
        "KeyB" => Key::B,
        "KeyC" => Key::C,
        "KeyD" => Key::D,
        "KeyE" => Key::E,
        "KeyF" => Key::F,
        "KeyG" => Key::G,
        "KeyH" => Key::H,
        "KeyI" => Key::I,
        "KeyJ" => Key::J,
        "KeyK" => Key::K,
        "KeyL" => Key::L,
        "KeyM" => Key::M,
        "KeyN" => Key::N,
        "KeyO" => Key::O,
        "KeyP" => Key::P,
        "KeyQ" => Key::Q,
        "KeyR" => Key::R,
        "KeyS" => Key::S,
        "KeyT" => Key::T,
        "KeyU" => Key::U,
        "KeyV" => Key::V,
        "KeyW" => Key::W,
        "KeyX" => Key::X,
        "KeyY" => Key::Y,
        "KeyZ" => Key::Z,
        "Backquote" => Key::Backquote,
        "Backslash" => Key::Backslash,
        "BracketLeft" => Key::BracketLeft,
        "BracketRight" => Key::BracketRight,
        "Comma" => Key::Comma,
        "Digit0" => Key::Digit0,
        "Digit1" => Key::Digit1,
        "Digit2" => Key::Digit2,
        "Digit3" => Key::Digit3,
        "Digit4" => Key::Digit4,
        "Digit5" => Key::Digit5,
        "Digit6" => Key::Digit6,
        "Digit7" => Key::Digit7,
        "Digit8" => Key::Digit8,
        "Digit9" => Key::Digit9,
        "Equal" => Key::Equal,
        "IntlBackslash" => Key::IntlBackslash,
        "IntlRo" => Key::IntlRo,
        "IntlYen" => Key::IntlYen,
        "Minus" => Key::Minus,
        "Period" => Key::Period,
        "Quote" => Key::Quote,
        "Semicolon" => Key::Semicolon,
        "Slash" => Key::Slash,
        "AltLeft" => Key::AltLeft,
        "AltRight" => Key::AltRight,
        "Backspace" => Key::Backspace,
        "CapsLock" => Key::CapsLock,
        "ContextMenu" => Key::ContextMenu,
        "ControlLeft" => Key::ControlLeft,
        "ControlRight" => Key::ControlRight,
        "Enter" => Key::Enter,
        "MetaLeft" => Key::MetaLeft,
        "MetaRight" => Key::MetaRight,
        "ShiftLeft" => Key::ShiftLeft,
        "ShiftRight" => Key::ShiftRight,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "Convert" => Key::Convert,
        "KanaMode" => Key::KanaMode,
        "NonConvert" => Key::NonConvert,
        "Delete" => Key::Delete,
        "End" => Key::End,
        "Help" => Key::Help,
        "Home" => Key::Home,
        "Insert" => Key::Insert,
        "PageDown" => Key::PageDown,
        "PageUp" => Key::PageUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "ArrowUp" => Key::ArrowUp,
        "NumLock" => Key::NumLock,
        "Numpad0" => Key::Numpad0,
        "Numpad1" => Key::Numpad1,
        "Numpad2" => Key::Numpad2,
        "Numpad3" => Key::Numpad3,
        "Numpad4" => Key::Numpad4,
        "Numpad5" => Key::Numpad5,
        "Numpad6" => Key::Numpad6,
        "Numpad7" => Key::Numpad7,
        "Numpad8" => Key::Numpad8,
        "Numpad9" => Key::Numpad9,
        "NumpadAdd" => Key::NumpadAdd,
        "NumpadBackspace" => Key::NumpadBackspace,
        "NumpadClear" => Key::NumpadClear,
        "NumpadClearEntry" => Key::NumpadClearEntry,
        "NumpadComma" => Key::NumpadComma,
        "NumpadDecimal" => Key::NumpadDecimal,
        "NumpadDivide" => Key::NumpadDivide,
        "NumpadEnter" => Key::NumpadEnter,
        "NumpadEqual" => Key::NumpadEqual,
        "NumpadMultiply" => Key::NumpadMultiply,
        "NumpadParenLeft" => Key::NumpadParenLeft,
        "NumpadParenRight" => Key::NumpadParenRight,
        "NumpadSubtract" => Key::NumpadSubtract,
        "Escape" => Key::Escape,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "F13" => Key::F13,
        "F14" => Key::F14,
        "F15" => Key::F15,
        "F16" => Key::F16,
        "F17" => Key::F17,
        "F18" => Key::F18,
        "F19" => Key::F19,
        "F20" => Key::F20,
        "F21" => Key::F21,
        "F22" => Key::F22,
        "F23" => Key::F23,
        "F24" => Key::F24,
        "PrintScreen" => Key::PrintScreen,
        "ScrollLock" => Key::ScrollLock,
        "Pause" => Key::Pause,
        _ => return None,
    })
}

/// The engine's key for a key of the layout's, where no physical key was
/// given: the key that makes it on a US layout.
fn key_of(key: KeyCode) -> Option<Key> {
    const LETTERS: [Key; 26] = [
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
    const DIGITS: [Key; 10] = [
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
    const FUNCTIONS: [Key; 12] = [
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
            LETTERS[(c.to_ascii_lowercase() as u8 - b'a') as usize]
        }
        KeyCode::Char(c) if c.is_ascii_digit() => DIGITS[(c as u8 - b'0') as usize],
        KeyCode::Char('`') => Key::Backquote,
        KeyCode::Char('\\') => Key::Backslash,
        KeyCode::Char('[') => Key::BracketLeft,
        KeyCode::Char(']') => Key::BracketRight,
        KeyCode::Char(',') => Key::Comma,
        KeyCode::Char('=') => Key::Equal,
        KeyCode::Char('-') => Key::Minus,
        KeyCode::Char('.') => Key::Period,
        KeyCode::Char('\'') => Key::Quote,
        KeyCode::Char(';') => Key::Semicolon,
        KeyCode::Char('/') => Key::Slash,
        KeyCode::Char(' ') => Key::Space,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Escape => Key::Escape,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::UpArrow => Key::ArrowUp,
        KeyCode::DownArrow => Key::ArrowDown,
        KeyCode::LeftArrow => Key::ArrowLeft,
        KeyCode::RightArrow => Key::ArrowRight,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Function(n @ 1..=12) => FUNCTIONS[n as usize - 1],
        _ => return None,
    })
}

/// The character a key makes with no Shift on a US layout.
fn us_unshifted(key: Key) -> Option<char> {
    Some(match key {
        Key::Backquote => '`',
        Key::Backslash => '\\',
        Key::BracketLeft => '[',
        Key::BracketRight => ']',
        Key::Comma => ',',
        Key::Equal => '=',
        Key::Minus => '-',
        Key::Period => '.',
        Key::Quote => '\'',
        Key::Semicolon => ';',
        Key::Slash => '/',
        Key::Space => ' ',
        Key::Digit0 => '0',
        Key::Digit1 => '1',
        Key::Digit2 => '2',
        Key::Digit3 => '3',
        Key::Digit4 => '4',
        Key::Digit5 => '5',
        Key::Digit6 => '6',
        Key::Digit7 => '7',
        Key::Digit8 => '8',
        Key::Digit9 => '9',
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        _ => return None,
    })
}

impl TerminalCore {
    /// The bytes the engine's encoder writes for a key, in the modes the
    /// program has asked for. A key the engine has no name for is written
    /// as one it does not know, with the text it made.
    pub fn key_stroke(&self, stroke: &KeyStroke<'_>) -> Vec<u8> {
        let key =
            (key_coded(stroke.code).or_else(|| key_of(stroke.key))).unwrap_or(Key::Unidentified);
        self.encoded(key, stroke).unwrap_or_default()
    }

    /// Text with no key behind it: an input method's commit.
    pub fn key_text(&self, text: &str) -> Vec<u8> {
        let stroke = KeyStroke {
            code: "",
            key: KeyCode::Char('\0'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
            made: Some(text),
        };
        self.encoded(Key::Unidentified, &stroke).unwrap_or_default()
    }

    fn encoded(&self, key: Key, stroke: &KeyStroke<'_>) -> Option<Vec<u8>> {
        let mut encoder = Encoder::new().ok()?;
        encoder.set_options_from_terminal(&self.terminal);
        let mut event = Event::new().ok()?;
        event.set_action(match stroke.kind {
            KeyEventKind::Press => Action::Press,
            KeyEventKind::Repeat => Action::Repeat,
            KeyEventKind::Release => Action::Release,
        });
        event.set_key(key);
        let mut held = Mods::empty();
        for (wire, engine) in [
            (Modifiers::SHIFT, Mods::SHIFT),
            (Modifiers::CTRL, Mods::CTRL),
            (Modifiers::ALT, Mods::ALT),
            (Modifiers::SUPER, Mods::SUPER),
        ] {
            if stroke.modifiers.contains(wire) {
                held |= engine;
            }
        }
        event.set_mods(held);
        // A character key sent with no text (the wire's chords carry none)
        // makes its own character: the encoder writes a printable key from
        // its text, and a chord's from the key and the modifiers.
        let own = match stroke.key {
            KeyCode::Char(c) if !c.is_control() => Some(c.to_string()),
            _ => None,
        };
        let made =
            (stroke.made.or(own.as_deref())).filter(|made| !made.chars().any(char::is_control));
        let one = made.and_then(|made| {
            let mut characters = made.chars();
            characters.next().filter(|_| characters.next().is_none())
        });
        // The character the key makes with no Shift: what it made, where
        // no Shift is held; a letter's lower case; and otherwise what the
        // key makes on a US layout, the layout's own not being known here.
        // (A key with no name has none.)
        let shift = held.contains(Mods::SHIFT);
        let unshifted = match one {
            Some(one) if !shift => one.to_lowercase().next(),
            Some(one) if one.is_alphabetic() => one.to_lowercase().next(),
            _ if key == Key::Unidentified => None,
            _ => us_unshifted(key),
        };
        // Shift that went into making the text is not a modifier of the
        // key any more.
        if shift && one.is_some() && one != unshifted {
            event.set_consumed_mods(Mods::SHIFT);
        }
        event.set_utf8(made);
        if let Some(unshifted) = unshifted {
            event.set_unshifted_codepoint(unshifted);
        }
        let mut written = Vec::new();
        encoder.encode_to_vec(&event, &mut written).ok()?;
        Some(written)
    }
}

#[cfg(test)]
mod tests {
    use termwiz::input::{KeyCode, Modifiers};

    use super::KeyStroke;
    use crate::core::TerminalCore;
    use crate::types::{KeyEventKind, TerminalSize};

    fn written(
        core: &TerminalCore,
        code: &str,
        key: KeyCode,
        modifiers: Modifiers,
        made: Option<&str>,
    ) -> String {
        let bytes = core.key_stroke(&KeyStroke {
            code,
            key,
            modifiers,
            kind: KeyEventKind::Press,
            made,
        });
        String::from_utf8(bytes).unwrap().escape_debug().to_string()
    }

    /// What the engine's encoder writes for the keys a shell is driven
    /// with, as a view gives them: with the physical key's name, and
    /// without it (a key event that names none).
    #[test]
    fn the_keys_a_shell_is_driven_with_are_written_as_terminals_write_them() {
        let core = TerminalCore::new(TerminalSize::new(80, 24));
        let (none, shift, ctrl, alt) = (
            Modifiers::NONE,
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
        );
        let keys: [(&str, KeyCode, Modifiers, Option<&str>, &str); 24] = [
            ("KeyA", KeyCode::Char('a'), none, Some("a"), "a"),
            ("KeyA", KeyCode::Char('A'), shift, Some("A"), "A"),
            ("KeyA", KeyCode::Char('a'), ctrl, Some("a"), "\\u{1}"),
            ("KeyC", KeyCode::Char('c'), ctrl, Some("c"), "\\u{3}"),
            (
                "KeyA",
                KeyCode::Char('a'),
                ctrl | shift,
                Some("A"),
                // (The engine's own way: Shift held with a Control chord
                // is told, in the form the kitty protocol has, with no
                // mode asked for. Horizon's encoder wrote the control
                // character.)
                "\\u{1b}[97;6u",
            ),
            ("KeyA", KeyCode::Char('a'), alt, Some("a"), "\\u{1b}a"),
            (
                "KeyA",
                KeyCode::Char('a'),
                ctrl | alt,
                Some("a"),
                "\\u{1b}\\u{1}",
            ),
            ("Digit1", KeyCode::Char('!'), shift, Some("!"), "!"),
            ("Space", KeyCode::Char(' '), none, Some(" "), " "),
            ("Space", KeyCode::Char(' '), ctrl, Some(" "), "\\0"),
            ("Enter", KeyCode::Enter, none, None, "\\r"),
            ("Tab", KeyCode::Tab, none, None, "\\t"),
            ("Tab", KeyCode::Tab, shift, None, "\\u{1b}[Z"),
            ("Backspace", KeyCode::Backspace, none, None, "\\u{7f}"),
            ("Escape", KeyCode::Escape, none, None, "\\u{1b}"),
            ("ArrowUp", KeyCode::UpArrow, none, None, "\\u{1b}[A"),
            ("ArrowUp", KeyCode::UpArrow, ctrl, None, "\\u{1b}[1;5A"),
            ("ArrowLeft", KeyCode::LeftArrow, alt, None, "\\u{1b}[1;3D"),
            ("Home", KeyCode::Home, none, None, "\\u{1b}[H"),
            ("End", KeyCode::End, none, None, "\\u{1b}[F"),
            ("Delete", KeyCode::Delete, none, None, "\\u{1b}[3~"),
            ("PageUp", KeyCode::PageUp, none, None, "\\u{1b}[5~"),
            ("F1", KeyCode::Function(1), none, None, "\\u{1b}OP"),
            ("F5", KeyCode::Function(5), none, None, "\\u{1b}[15~"),
        ];
        let mut differing = Vec::new();
        for (code, key, modifiers, made, expected) in keys {
            for code in [code, ""] {
                let got = written(&core, code, key, modifiers, made);
                if got != expected {
                    differing.push(format!(
                        "{key:?} {modifiers:?} ({code:?}): {got} for {expected}"
                    ));
                }
            }
        }
        assert!(differing.is_empty(), "{}", differing.join("\n"));
    }

    /// A key let go writes nothing where the program has not asked for
    /// that, and the modes a program asks for change what is written.
    #[test]
    fn the_modes_a_program_asks_for_change_what_a_key_writes() {
        let mut core = TerminalCore::new(TerminalSize::new(80, 24));
        let up =
            |core: &TerminalCore| written(core, "ArrowUp", KeyCode::UpArrow, Modifiers::NONE, None);
        let release = core.key_stroke(&KeyStroke {
            code: "KeyA",
            key: KeyCode::Char('a'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Release,
            made: None,
        });
        assert!(release.is_empty());
        assert_eq!(up(&core), "\\u{1b}[A");
        // Application cursor keys.
        core.write_vt(b"\x1b[?1h");
        assert_eq!(up(&core), "\\u{1b}OA");
        core.write_vt(b"\x1b[?1l");
        // The kitty protocol's first flag: a chord that was a control
        // character is told apart from it.
        core.write_vt(b"\x1b[>1u");
        let key = |code, key, modifiers, made| written(&core, code, key, modifiers, made);
        assert_eq!(
            key("KeyA", KeyCode::Char('a'), Modifiers::CTRL, Some("a")),
            "\\u{1b}[97;5u"
        );
        assert_eq!(
            key("Escape", KeyCode::Escape, Modifiers::NONE, None),
            "\\u{1b}[27u"
        );
        assert_eq!(
            key("KeyA", KeyCode::Char('a'), Modifiers::NONE, Some("a")),
            "a"
        );
    }
}
