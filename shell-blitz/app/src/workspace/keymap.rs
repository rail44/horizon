//! The chords the configuration binds (`[keybindings]`: a chord to a
//! command's id), and the one that turns workspace mode on and off. The
//! chord syntax and the two reserved ids are the GPUI shell's
//! (`src/keymap.rs`): modifiers joined by `+`, ending in the key, in any
//! case.

use horizon_config::RawConfig;
use horizon_ui::blitz_traits::events::BlitzKeyEvent;
use horizon_workspace::commands::CommandId;
use keyboard_types::{Key, NamedKey};

/// Binds a chord to opening the palette, which is not a command the
/// palette lists.
const OPEN_PALETTE: &str = "open-palette";
/// Binds the chord that turns workspace mode on and off, in place of the
/// built-in one.
const WORKSPACE_MODE: &str = "workspace-mode";
const DEFAULT_WORKSPACE_MODE: &str = "ctrl+'";

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

/// What a bound chord does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    OpenPalette,
    Command(CommandId),
}

pub struct Keymap {
    pub workspace_mode: Chord,
    bindings: Vec<(Chord, Bound)>,
}

/// The chords bound with no configuration: the font size's.
const BUILT_IN: [(&str, CommandId); 4] = [
    ("ctrl+=", CommandId::IncreaseFontSize),
    ("ctrl++", CommandId::IncreaseFontSize),
    ("ctrl+-", CommandId::DecreaseFontSize),
    ("ctrl+0", CommandId::ResetFontSize),
];

impl Default for Keymap {
    fn default() -> Self {
        let chord = |chord| Chord::parse(chord).expect("a chord");
        Self {
            workspace_mode: chord(DEFAULT_WORKSPACE_MODE),
            bindings: (BUILT_IN.iter())
                .map(|(keys, command)| (chord(keys), Bound::Command(*command)))
                .collect(),
        }
    }
}

impl Keymap {
    /// The configuration's bindings. An entry that is not a chord, or
    /// names no command, is left out with a line on stderr.
    pub fn from_config(config: &RawConfig) -> Self {
        let mut keymap = Self::default();
        // The configuration's come first: one of them on a built-in chord
        // takes it.
        let built_in = std::mem::take(&mut keymap.bindings);
        for (chord, command) in &config.keybindings {
            let skipped = |why: &str| {
                eprintln!("horizon config: skipping keybinding `{chord}` = `{command}`: {why}");
            };
            let Some(parsed) = Chord::parse(chord) else {
                skipped("unrecognized chord");
                continue;
            };
            let named = |id: &&CommandId| id.spec().name == command;
            match command.as_str() {
                WORKSPACE_MODE => keymap.workspace_mode = parsed,
                OPEN_PALETTE => keymap.bindings.push((parsed, Bound::OpenPalette)),
                _ => match CommandId::ALL.iter().find(named) {
                    Some(id) => keymap.bindings.push((parsed, Bound::Command(*id))),
                    None => skipped("unknown command id"),
                },
            }
        }
        keymap.bindings.extend(built_in);
        keymap
    }

    /// What the configuration binds `event` to.
    pub fn bound(&self, event: &BlitzKeyEvent) -> Option<Bound> {
        let binding = self.bindings.iter().find(|(chord, _)| chord.matches(event));
        binding.map(|(_, bound)| *bound)
    }
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
            state: horizon_ui::blitz_traits::events::KeyState::Pressed,
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

    #[test]
    fn the_configuration_binds_commands_the_palette_and_the_mode() {
        let config = horizon_config::parse(
            r#"
            [keybindings]
            "Ctrl+Shift+D" = "split-down"
            "Ctrl+Shift+P" = "open-palette"
            "Ctrl+Space" = "workspace-mode"
            "Ctrl+Shift+X" = "no-such-command"
            "#,
        )
        .unwrap();
        let keymap = Keymap::from_config(&config);
        let chord = Modifiers::CONTROL | Modifiers::SHIFT;
        assert_eq!(
            keymap.bound(&character("D", chord)),
            Some(Bound::Command(CommandId::SplitDown))
        );
        assert_eq!(
            keymap.bound(&character("P", chord)),
            Some(Bound::OpenPalette)
        );
        assert_eq!(keymap.bound(&character("X", chord)), None);
        assert!(
            keymap
                .workspace_mode
                .matches(&character(" ", Modifiers::CONTROL))
        );
        assert!(
            !keymap
                .workspace_mode
                .matches(&character("'", Modifiers::CONTROL))
        );
    }
}
