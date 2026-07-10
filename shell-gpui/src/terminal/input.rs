//! GPUI keystroke → termwiz key-code mapping. Encoding — legacy escape
//! sequences AND negotiated kitty state — is horizon-terminal-core's job
//! (`protocol/kitty_keyboard`); this layer only names the key. Plain
//! printable text deliberately does NOT map here: on macOS it arrives
//! through the text-input pipeline (EntityInputHandler), and routing it
//! through Key too would double-feed every keypress. M1 revisits this
//! with kitty-flags-on-frame mode routing (docs/gpui-migration-design.md).

use gpui::{Keystroke, Modifiers};

/// Named/function keys always map; character keys map only when Ctrl is
/// held (otherwise they are text and belong to the input-handler
/// pipeline). Alt-held characters are left to macOS option-composition
/// pending the option-as-alt policy decision (M1).
pub(crate) fn term_key_code(keystroke: &Keystroke) -> Option<termwiz::input::KeyCode> {
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

pub(crate) fn term_modifiers(modifiers: &Modifiers) -> termwiz::input::Modifiers {
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
