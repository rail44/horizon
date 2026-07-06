//! Pure key classifier for the session manager modal
//! (`control_surface::view::session_manager`) -- mirrors
//! `workspace::mode_input`'s layering exactly: a normalization step from a
//! raw `Key`/`Modifiers` pair to a small enum, then a pure decision step
//! from that enum (plus whatever bit of state the decision actually needs)
//! to a [`SessionManagerAction`]. Neither step calls `execute_command` --
//! the view (`control_surface::view::session_manager`) matches the returned
//! action and dispatches through the command model itself, per AGENTS.md's
//! "operations go through the command model" convention.

use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};

/// What one recognized session-manager key requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionManagerAction {
    MoveSelection(isize),
    /// `Enter`: attach a detached row (diving) or jump to an attached row's
    /// pane.
    Activate,
    /// First `x` on a row with no pending termination yet.
    RequestTerminate,
    /// Second `x` on the row already marked pending.
    ConfirmTerminate,
    /// `Esc` while a termination is pending -- cancels the pending mark
    /// without closing the modal.
    CancelPendingTerminate,
    /// `Esc` with nothing pending -- closes the modal.
    Close,
}

/// One key, normalized to exactly the vocabulary the session manager
/// understands, plus a catch-all for everything else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionManagerKey {
    MoveDown,
    MoveUp,
    Enter,
    Escape,
    X,
    Other,
}

/// Normalizes a raw key press. Any held Ctrl/Alt/Meta disqualifies a key
/// from the plain vocabulary (`Other`) -- the same rule
/// `workspace::mode_input::mode_key_from_key` uses, so a config-level
/// keybinding chord (which always holds a modifier) can never be
/// accidentally shadowed by this modal's single-key vocabulary.
fn session_manager_key_from_key(key: &Key, modifiers: Modifiers) -> SessionManagerKey {
    if modifiers.control() || modifiers.alt() || modifiers.meta() {
        return SessionManagerKey::Other;
    }

    match key {
        Key::Character(text) => match text.to_ascii_lowercase().as_str() {
            "j" => SessionManagerKey::MoveDown,
            "k" => SessionManagerKey::MoveUp,
            "x" => SessionManagerKey::X,
            _ => SessionManagerKey::Other,
        },
        Key::Named(NamedKey::ArrowDown) => SessionManagerKey::MoveDown,
        Key::Named(NamedKey::ArrowUp) => SessionManagerKey::MoveUp,
        Key::Named(NamedKey::Enter) => SessionManagerKey::Enter,
        Key::Named(NamedKey::Escape) => SessionManagerKey::Escape,
        _ => SessionManagerKey::Other,
    }
}

/// Decides the action for a normalized key, given whether the *currently
/// selected row* is the one already marked pending termination -- both `x`
/// and `Esc` branch on this (see [`SessionManagerAction`]'s doc comments).
fn action_for_key(
    key: SessionManagerKey,
    pending_terminate_active: bool,
) -> Option<SessionManagerAction> {
    match key {
        SessionManagerKey::MoveDown => Some(SessionManagerAction::MoveSelection(1)),
        SessionManagerKey::MoveUp => Some(SessionManagerAction::MoveSelection(-1)),
        SessionManagerKey::Enter => Some(SessionManagerAction::Activate),
        SessionManagerKey::X => Some(if pending_terminate_active {
            SessionManagerAction::ConfirmTerminate
        } else {
            SessionManagerAction::RequestTerminate
        }),
        SessionManagerKey::Escape => Some(if pending_terminate_active {
            SessionManagerAction::CancelPendingTerminate
        } else {
            SessionManagerAction::Close
        }),
        SessionManagerKey::Other => None,
    }
}

/// `Event::KeyDown` entry point for the session manager modal.
/// `pending_terminate_active` is whether the row currently selected is the
/// one already awaiting a second `x` press -- the caller (`control_surface::
/// view::session_manager`) computes this by comparing its own
/// `pending_terminate` signal against the selected row's session id, since
/// this module has no notion of rows at all.
pub(crate) fn interpret_session_manager_key(
    key_event: &KeyEvent,
    pending_terminate_active: bool,
) -> Option<SessionManagerAction> {
    let key = session_manager_key_from_key(&key_event.key.logical_key, key_event.modifiers);
    action_for_key(key, pending_terminate_active)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: Key, modifiers: Modifiers) -> SessionManagerKey {
        session_manager_key_from_key(&k, modifiers)
    }

    // --- raw key normalization --------------------------------------------

    #[test]
    fn jk_and_arrows_normalize_to_move() {
        assert_eq!(
            key(Key::Character("j".into()), Modifiers::default()),
            SessionManagerKey::MoveDown
        );
        assert_eq!(
            key(Key::Character("k".into()), Modifiers::default()),
            SessionManagerKey::MoveUp
        );
        assert_eq!(
            key(Key::Named(NamedKey::ArrowDown), Modifiers::default()),
            SessionManagerKey::MoveDown
        );
        assert_eq!(
            key(Key::Named(NamedKey::ArrowUp), Modifiers::default()),
            SessionManagerKey::MoveUp
        );
    }

    #[test]
    fn x_normalizes_regardless_of_case() {
        assert_eq!(
            key(Key::Character("X".into()), Modifiers::SHIFT),
            SessionManagerKey::X
        );
    }

    #[test]
    fn enter_and_escape_normalize_from_named_keys() {
        assert_eq!(
            key(Key::Named(NamedKey::Enter), Modifiers::default()),
            SessionManagerKey::Enter
        );
        assert_eq!(
            key(Key::Named(NamedKey::Escape), Modifiers::default()),
            SessionManagerKey::Escape
        );
    }

    #[test]
    fn unassigned_keys_normalize_to_other() {
        assert_eq!(
            key(Key::Character("a".into()), Modifiers::default()),
            SessionManagerKey::Other
        );
    }

    #[test]
    fn a_held_ctrl_alt_or_meta_downgrades_the_vocabulary_to_other() {
        assert_eq!(
            key(Key::Character("j".into()), Modifiers::CONTROL),
            SessionManagerKey::Other
        );
        assert_eq!(
            key(Key::Character("x".into()), Modifiers::ALT),
            SessionManagerKey::Other
        );
        assert_eq!(
            key(Key::Named(NamedKey::Enter), Modifiers::META),
            SessionManagerKey::Other
        );
    }

    // --- action decision (`action_for_key`) -------------------------------

    #[test]
    fn move_and_activate_are_independent_of_pending_state() {
        assert_eq!(
            action_for_key(SessionManagerKey::MoveDown, false),
            Some(SessionManagerAction::MoveSelection(1))
        );
        assert_eq!(
            action_for_key(SessionManagerKey::MoveUp, true),
            Some(SessionManagerAction::MoveSelection(-1))
        );
        assert_eq!(
            action_for_key(SessionManagerKey::Enter, true),
            Some(SessionManagerAction::Activate)
        );
    }

    #[test]
    fn x_requests_then_confirms_termination() {
        assert_eq!(
            action_for_key(SessionManagerKey::X, false),
            Some(SessionManagerAction::RequestTerminate)
        );
        assert_eq!(
            action_for_key(SessionManagerKey::X, true),
            Some(SessionManagerAction::ConfirmTerminate)
        );
    }

    #[test]
    fn escape_cancels_pending_or_else_closes() {
        assert_eq!(
            action_for_key(SessionManagerKey::Escape, true),
            Some(SessionManagerAction::CancelPendingTerminate)
        );
        assert_eq!(
            action_for_key(SessionManagerKey::Escape, false),
            Some(SessionManagerAction::Close)
        );
    }

    #[test]
    fn an_unassigned_key_resolves_to_no_action() {
        assert_eq!(action_for_key(SessionManagerKey::Other, false), None);
    }

    // `interpret_session_manager_key` itself isn't unit-tested directly: it
    // takes a real `floem::keyboard::KeyEvent`, which wraps a private
    // platform-specific winit field and cannot be constructed from a plain
    // struct literal outside floem -- the same limitation
    // `workspace::mode_input`'s tests document for `handle_workspace_mode_key`.
    // Its logic is exhaustively covered through the pure pieces above.
}
