use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};
use termwiz::input::{KeyCode as TermKeyCode, Modifiers as TermModifiers};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AgentDraftAction {
    Insert(String),
    Backspace,
    Submit,
}

pub(crate) fn pop_last_grapheme_approx(text: &mut String) {
    while let Some(ch) = text.pop() {
        if !is_combining_mark(ch) {
            break;
        }
    }
}

pub(crate) fn agent_draft_action(key: &Key, modifiers: Modifiers) -> Option<AgentDraftAction> {
    match key {
        Key::Named(NamedKey::Enter) => Some(AgentDraftAction::Submit),
        Key::Named(NamedKey::Backspace) => Some(AgentDraftAction::Backspace),
        Key::Named(NamedKey::Space) if agent_accepts_text_input(modifiers) => {
            Some(AgentDraftAction::Insert(" ".to_string()))
        }
        Key::Character(text) if agent_accepts_text_input(modifiers) => {
            Some(AgentDraftAction::Insert(text.to_string()))
        }
        _ => None,
    }
}

pub(crate) fn terminal_input_from_key(event: &KeyEvent) -> Option<Vec<u8>> {
    match &event.key.logical_key {
        Key::Character(text) => character_input(text.as_str(), event.modifiers),
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

pub(crate) fn terminal_key_from_key(event: &KeyEvent) -> Option<TermKeyCode> {
    terminal_key_from_input(&event.key.logical_key)
}

pub(crate) fn terminal_key_from_input(key: &Key) -> Option<TermKeyCode> {
    match key {
        Key::Named(NamedKey::Enter) => Some(TermKeyCode::Enter),
        Key::Named(NamedKey::Tab) => Some(TermKeyCode::Tab),
        Key::Named(NamedKey::Backspace) => Some(TermKeyCode::Backspace),
        Key::Named(NamedKey::Escape) => Some(TermKeyCode::Escape),
        Key::Named(NamedKey::ArrowUp) => Some(TermKeyCode::UpArrow),
        Key::Named(NamedKey::ArrowDown) => Some(TermKeyCode::DownArrow),
        Key::Named(NamedKey::ArrowRight) => Some(TermKeyCode::RightArrow),
        Key::Named(NamedKey::ArrowLeft) => Some(TermKeyCode::LeftArrow),
        Key::Named(NamedKey::Home) => Some(TermKeyCode::Home),
        Key::Named(NamedKey::End) => Some(TermKeyCode::End),
        Key::Named(NamedKey::PageUp) => Some(TermKeyCode::PageUp),
        Key::Named(NamedKey::PageDown) => Some(TermKeyCode::PageDown),
        Key::Named(NamedKey::Delete) => Some(TermKeyCode::Delete),
        _ => None,
    }
}

pub(crate) fn termwiz_modifiers(modifiers: Modifiers) -> TermModifiers {
    let mut term_modifiers = TermModifiers::NONE;
    if modifiers.shift() {
        term_modifiers |= TermModifiers::SHIFT;
    }
    if modifiers.control() {
        term_modifiers |= TermModifiers::CTRL;
    }
    if modifiers.alt() {
        term_modifiers |= TermModifiers::ALT;
    }
    if modifiers.meta() {
        term_modifiers |= TermModifiers::SUPER;
    }
    term_modifiers
}

pub(crate) fn is_terminal_paste_key(event: &KeyEvent) -> bool {
    is_terminal_paste_input(&event.key.logical_key, event.modifiers)
}

pub(crate) fn is_palette_open_key(event: &KeyEvent) -> bool {
    match &event.key.logical_key {
        Key::Character(text) => event.modifiers.control() && text.eq_ignore_ascii_case("p"),
        _ => false,
    }
}

pub(crate) fn palette_accepts_text_input(modifiers: Modifiers) -> bool {
    !modifiers.control() && !modifiers.alt() && !modifiers.meta()
}

pub(crate) fn is_terminal_copy_key(event: &KeyEvent) -> bool {
    is_terminal_copy_input(&event.key.logical_key, event.modifiers)
}

fn agent_accepts_text_input(modifiers: Modifiers) -> bool {
    !modifiers.control() && !modifiers.alt() && !modifiers.meta()
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    )
}

fn is_terminal_paste_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Paste) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("v")
        }
        _ => false,
    }
}

fn is_terminal_copy_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Copy) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("c")
        }
        _ => false,
    }
}

fn character_input(text: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let single_char = chars.next().is_none();

    if modifiers.control() && single_char {
        return control_input(first);
    }

    if modifiers.meta() {
        return None;
    }

    let mut bytes = Vec::new();
    if modifiers.alt() {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

fn control_input(c: char) -> Option<Vec<u8>> {
    let c = c.to_ascii_lowercase();
    let byte = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(vec![byte])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_input_keeps_space() {
        assert_eq!(
            character_input(" ", Modifiers::default()),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn character_input_keeps_utf8_text() {
        assert_eq!(
            character_input("日本語", Modifiers::default()),
            Some("日本語".as_bytes().to_vec())
        );
    }

    #[test]
    fn control_space_input_is_nul() {
        assert_eq!(control_input(' '), None);
    }

    #[test]
    fn agent_draft_accepts_plain_text() {
        assert_eq!(
            agent_draft_action(&Key::Character("hello".into()), Modifiers::default()),
            Some(AgentDraftAction::Insert("hello".to_string()))
        );
    }

    #[test]
    fn agent_draft_accepts_submit_and_backspace() {
        assert_eq!(
            agent_draft_action(&Key::Named(NamedKey::Enter), Modifiers::default()),
            Some(AgentDraftAction::Submit)
        );
        assert_eq!(
            agent_draft_action(&Key::Named(NamedKey::Backspace), Modifiers::default()),
            Some(AgentDraftAction::Backspace)
        );
    }

    #[test]
    fn agent_draft_keeps_control_shortcuts_available() {
        assert_eq!(
            agent_draft_action(&Key::Character("p".into()), Modifiers::CONTROL),
            None
        );
    }

    #[test]
    fn ctrl_shift_v_is_terminal_paste() {
        assert!(is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_v_remains_terminal_control_input() {
        assert!(!is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("v", Modifiers::CONTROL), Some(vec![0x16]));
    }

    #[test]
    fn ctrl_shift_c_is_terminal_copy() {
        assert!(is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_c_remains_terminal_control_input() {
        assert!(!is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("c", Modifiers::CONTROL), Some(vec![0x03]));
    }

    #[test]
    fn named_arrow_uses_termwiz_key_path() {
        assert_eq!(
            terminal_key_from_input(&Key::Named(NamedKey::ArrowUp)),
            Some(TermKeyCode::UpArrow)
        );
    }

    #[test]
    fn modifiers_convert_to_termwiz() {
        assert_eq!(
            termwiz_modifiers(Modifiers::CONTROL | Modifiers::SHIFT),
            TermModifiers::CTRL | TermModifiers::SHIFT
        );
    }
}
