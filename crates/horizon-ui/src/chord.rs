//! A chord as a key binding and a view's keys spell it: modifiers joined
//! by `+`, ending in the key, in any case (`ctrl+shift+t`, `esc`, `j`).

use blitz_traits::events::BlitzKeyEvent;
use keyboard_types::{Key, NamedKey};

#[derive(Clone, Debug, PartialEq, Eq)]
enum ChordKey {
    /// In lower case.
    Character(char),
    Named(NamedKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
    key: ChordKey,
}

impl Chord {
    /// `None` for what is not a chord: no key, a modifier that is not one,
    /// a key that is neither a character nor a key's name.
    pub fn parse(chord: &str) -> Option<Self> {
        let chord = chord.trim();
        // `+` as the key: alone, or after the `+` that joins it on.
        let (modifiers, key) = match chord.strip_suffix("++") {
            Some(modifiers) => (modifiers, "+"),
            None if chord == "+" => ("", "+"),
            None => match chord.rsplit_once('+') {
                Some((modifiers, key)) => (modifiers, key.trim()),
                None => ("", chord),
            },
        };
        let mut parsed = Chord {
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
            key: key_named(key)?,
        };
        for modifier in modifiers.split('+').map(str::trim) {
            match modifier.to_ascii_lowercase().as_str() {
                "" => {}
                "ctrl" | "control" => parsed.ctrl = true,
                "shift" => parsed.shift = true,
                "alt" | "option" => parsed.alt = true,
                "meta" | "cmd" | "command" | "super" | "win" => parsed.meta = true,
                _ => return None,
            }
        }
        Some(parsed)
    }

    /// Whether it is typed, not pressed, in a text field: a character with
    /// no modifier but Shift.
    pub fn typed(&self) -> bool {
        matches!(self.key, ChordKey::Character(_)) && !(self.ctrl || self.alt || self.meta)
    }

    /// Whether `event` is this chord. Shift is compared for letters and
    /// named keys only: for any other character it is part of how the
    /// character is typed, which differs between keyboards.
    pub fn matches(&self, event: &BlitzKeyEvent) -> bool {
        let modifiers = event.modifiers;
        if (self.ctrl, self.alt, self.meta) != (modifiers.ctrl(), modifiers.alt(), modifiers.meta())
        {
            return false;
        }
        match (&self.key, &event.key) {
            (ChordKey::Named(named), Key::Named(pressed)) => {
                named == pressed && self.shift == modifiers.shift()
            }
            (ChordKey::Character(character), Key::Character(pressed)) => {
                let mut pressed = pressed.chars().flat_map(char::to_lowercase);
                let same = pressed.next() == Some(*character) && pressed.next().is_none();
                same && (!character.is_alphabetic() || self.shift == modifiers.shift())
            }
            _ => false,
        }
    }
}

impl Chord {
    /// The chord as the bar at the bottom of the window spells it:
    /// `ctrl+shift+c`, `ctrl+'`, `esc`.
    pub fn label(&self) -> String {
        let mut label = String::new();
        for (held, name) in [
            (self.ctrl, "ctrl+"),
            (self.shift, "shift+"),
            (self.alt, "alt+"),
            (self.meta, "meta+"),
        ] {
            if held {
                label.push_str(name);
            }
        }
        match &self.key {
            ChordKey::Character(' ') => label.push_str("space"),
            ChordKey::Character(character) => label.push(*character),
            ChordKey::Named(named) => label.push_str(match named {
                NamedKey::Enter => "enter",
                NamedKey::Escape => "esc",
                NamedKey::Tab => "tab",
                NamedKey::Backspace => "backspace",
                NamedKey::Delete => "delete",
                NamedKey::ArrowUp => "↑",
                NamedKey::ArrowDown => "↓",
                NamedKey::ArrowLeft => "←",
                NamedKey::ArrowRight => "→",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::PageUp => "pgup",
                NamedKey::PageDown => "pgdn",
                _ => "?",
            }),
        }
        label
    }
}

fn key_named(token: &str) -> Option<ChordKey> {
    let lower = token.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => NamedKey::Enter,
        "escape" | "esc" => NamedKey::Escape,
        "tab" => NamedKey::Tab,
        "space" | "spacebar" => return Some(ChordKey::Character(' ')),
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "up" | "arrowup" => NamedKey::ArrowUp,
        "down" | "arrowdown" => NamedKey::ArrowDown,
        "left" | "arrowleft" => NamedKey::ArrowLeft,
        "right" | "arrowright" => NamedKey::ArrowRight,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" => NamedKey::PageUp,
        "pagedown" => NamedKey::PageDown,
        _ => {
            let mut characters = lower.chars();
            let character = characters.next()?;
            return characters
                .next()
                .is_none()
                .then_some(ChordKey::Character(character));
        }
    };
    Some(ChordKey::Named(named))
}

#[cfg(test)]
mod tests {
    use keyboard_types::Modifiers;

    use super::*;

    fn pressed(key: Key, modifiers: Modifiers) -> BlitzKeyEvent {
        BlitzKeyEvent {
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: blitz_traits::events::KeyState::Pressed,
            text: None,
        }
    }

    fn character(character: &str, modifiers: Modifiers) -> BlitzKeyEvent {
        pressed(Key::Character(character.into()), modifiers)
    }

    #[test]
    fn a_chord_is_its_modifiers_and_its_key_in_any_case() {
        let chord = Chord::parse("Ctrl+Shift+T").unwrap();
        assert!(chord.matches(&character("T", Modifiers::CONTROL | Modifiers::SHIFT)));
        assert!(!chord.matches(&character("t", Modifiers::CONTROL)));
        assert!(!chord.matches(&character("T", Modifiers::SHIFT)));
        assert!(!chord.matches(&character("y", Modifiers::CONTROL | Modifiers::SHIFT)));
        assert_eq!(Chord::parse("control + shift + t"), Some(chord));
    }

    #[test]
    fn a_named_key_and_a_symbol_are_keys() {
        let escape = Chord::parse("alt+Esc").unwrap();
        assert!(escape.matches(&pressed(Key::Named(NamedKey::Escape), Modifiers::ALT)));
        assert!(!escape.matches(&pressed(Key::Named(NamedKey::Enter), Modifiers::ALT)));
        // However the keyboard types the symbol.
        let plus = Chord::parse("ctrl++").unwrap();
        assert!(plus.matches(&character("+", Modifiers::CONTROL | Modifiers::SHIFT)));
        assert!(plus.matches(&character("+", Modifiers::CONTROL)));
        assert!(
            Chord::parse("ctrl+'")
                .unwrap()
                .matches(&character("'", Modifiers::CONTROL))
        );
    }

    #[test]
    fn what_is_not_a_chord_is_none() {
        for chord in ["", "ctrl+", "hyper+t", "ctrl+notakey", "t+ctrl"] {
            assert_eq!(Chord::parse(chord), None, "{chord:?}");
        }
    }
}
