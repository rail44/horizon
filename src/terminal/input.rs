//! GPUI keystroke → termwiz key-code mapping. Encoding — legacy escape
//! sequences AND negotiated kitty state — is horizon-terminal-core's job
//! (`protocol/kitty_keyboard`); this layer only names the key. Plain
//! printable text deliberately does NOT map here: on macOS it arrives
//! through the text-input pipeline (EntityInputHandler), and routing it
//! through Key too would double-feed every keypress. M1 revisits this
//! with kitty-flags-on-frame mode routing (docs/gpui-migration-design.md).

use gpui::{Keystroke, Modifiers, MouseButton, Pixels, Point, ScrollDelta};
use horizon_terminal_core::{TerminalMouseButton, TerminalMouseModifiers, TerminalSelectionPoint};

/// Pixel position (window coordinates) → cell coordinates, given the
/// paint-time metrics. Mirrors the Floem shell's `cell_from_point`.
pub(crate) fn cell_from_position(
    position: Point<Pixels>,
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
) -> TerminalSelectionPoint {
    let col = (f32::from(position.x - origin.x) / f32::from(cell_width))
        .max(0.0)
        .floor() as usize;
    let row = (f32::from(position.y - origin.y) / f32::from(line_height))
        .max(0.0)
        .floor() as usize;
    TerminalSelectionPoint { row, col }
}

/// Wheel delta → scroll step, mirroring the Floem shell's fixed ±3-line
/// step. Positive `TerminalScroll::lines` scrolls toward history
/// (alacritty `Scroll::Delta` convention).
pub(crate) fn scroll_lines_from_wheel(delta: &ScrollDelta) -> Option<i32> {
    let y = match delta {
        ScrollDelta::Lines(lines) => lines.y,
        ScrollDelta::Pixels(pixels) => f32::from(pixels.y),
    };
    if y.abs() < f32::EPSILON {
        return None;
    }
    Some(if y > 0.0 { 3 } else { -3 })
}

pub(crate) fn terminal_mouse_button(button: MouseButton) -> Option<TerminalMouseButton> {
    match button {
        MouseButton::Left => Some(TerminalMouseButton::Left),
        MouseButton::Middle => Some(TerminalMouseButton::Middle),
        MouseButton::Right => Some(TerminalMouseButton::Right),
        _ => None,
    }
}

pub(crate) fn terminal_mouse_modifiers(modifiers: &Modifiers) -> TerminalMouseModifiers {
    TerminalMouseModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    }
}

/// Named/function keys always map; character keys map when Ctrl is held
/// (never text) or when the session negotiated kitty's "report all keys
/// as escape codes" (`keys_as_escape_codes`, mirrored on the frame) —
/// otherwise they are text and belong to the input-handler pipeline.
/// Alt-held characters are left to macOS option-composition pending the
/// option-as-alt policy decision (M1).
pub(crate) fn term_key_code(
    keystroke: &Keystroke,
    keys_as_escape_codes: bool,
) -> Option<termwiz::input::KeyCode> {
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
    (keystroke.modifiers.control || keys_as_escape_codes).then_some(KeyCode::Char(ch))
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
