//! Workspace mode's key interpretation -- the "small judge that interprets
//! a key sequence" `docs/workspace-mode-design.md` calls for, structured so
//! a future vim vocabulary (counts, motions) can grow it without a
//! rewrite: [`interpret_mode_keys`] is a pure function from an input
//! sequence to an [`Interpretation`], not a flat one-key-one-action match.
//! v1 only ever feeds it a single key at a time (see
//! [`handle_workspace_mode_key`]) and every recognized key already
//! resolves in that one step, so [`Interpretation::Pending`] is not
//! reachable yet -- it exists as the seam a later `"2j"`-style count would
//! use, once a real key buffer sits above this function.
//!
//! Layering mirrors the pre-existing approval-key-routing precedent
//! (`approval_key_action`/`handle_agent_approval_key` in `workspace::input`):
//! this module only classifies -- it never calls `execute_command` itself.
//! The pane view (`workspace::view::pane`) and the app-level key fallback
//! (`app::input::AppInput::handle_key_down`) each match on the returned
//! [`ModeAction`] and dispatch through the command model themselves, per
//! AGENTS.md's "operations go through the command model" convention.

use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};
use floem::prelude::*;

use super::input::composing_guard_swallows;
use super::mode::Direction;

/// One key, normalized down to exactly the vocabulary workspace mode
/// understands today, plus a catch-all for everything else -- which is
/// still swallowed by the mode (see [`interpret_mode_keys`]), just not
/// part of its recognized vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModeKey {
    H,
    J,
    K,
    L,
    Enter,
    Escape,
    Colon,
    Other,
}

/// What one recognized workspace-mode key requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModeAction {
    Move(Direction),
    Commit,
    Cancel,
    OpenPalette,
}

/// The result of interpreting a key sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Interpretation {
    Action(ModeAction),
    /// The sequence so far could still complete into an action if more
    /// keys arrive (e.g. a future leading count digit). Never produced by
    /// v1's vocabulary -- see this module's doc comment -- hence the
    /// `allow`: this variant documents the seam for a future multi-key
    /// vocabulary rather than dead code to remove.
    #[allow(dead_code)]
    Pending,
    /// The sequence cannot complete into any action -- either it's not a
    /// recognized key, or (once counts/motions exist) a recognized prefix
    /// followed by a key that doesn't continue it.
    Invalid,
}

/// Normalizes a raw key press into a [`ModeKey`]. Any held Ctrl/Alt/Meta
/// disqualifies a key from the plain `hjkl`/Enter/Esc/`:` vocabulary
/// (`Other`) -- Shift is not checked, since it's how `:` itself is
/// typically produced (Shift+`;` on a US layout) and an accidentally
/// shift-held letter is harmless to still treat as its plain lowercase
/// self.
fn mode_key_from_key(key: &Key, modifiers: Modifiers) -> ModeKey {
    if modifiers.control() || modifiers.alt() || modifiers.meta() {
        return ModeKey::Other;
    }

    match key {
        Key::Character(text) => match text.to_ascii_lowercase().as_str() {
            "h" => ModeKey::H,
            "j" => ModeKey::J,
            "k" => ModeKey::K,
            "l" => ModeKey::L,
            ":" => ModeKey::Colon,
            _ => ModeKey::Other,
        },
        Key::Named(NamedKey::Enter) => ModeKey::Enter,
        Key::Named(NamedKey::Escape) => ModeKey::Escape,
        _ => ModeKey::Other,
    }
}

/// Interprets a key sequence. See this module's doc comment for why this
/// takes a slice (a seam for future multi-key sequences) even though v1
/// only ever calls it with exactly one key.
pub(crate) fn interpret_mode_keys(keys: &[ModeKey]) -> Interpretation {
    match keys {
        [ModeKey::H] => Interpretation::Action(ModeAction::Move(Direction::Left)),
        [ModeKey::J] => Interpretation::Action(ModeAction::Move(Direction::Down)),
        [ModeKey::K] => Interpretation::Action(ModeAction::Move(Direction::Up)),
        [ModeKey::L] => Interpretation::Action(ModeAction::Move(Direction::Right)),
        [ModeKey::Enter] => Interpretation::Action(ModeAction::Commit),
        [ModeKey::Escape] => Interpretation::Action(ModeAction::Cancel),
        [ModeKey::Colon] => Interpretation::Action(ModeAction::OpenPalette),
        _ => Interpretation::Invalid,
    }
}

/// `Event::KeyDown` entry point for a pane while workspace mode is already
/// active. Returns `None` both for a key still mid-IME-composition (the
/// same composing guard the message box/terminal use) and for any key
/// [`interpret_mode_keys`] doesn't resolve to an action -- either way, the
/// caller must still swallow the key (return `EventPropagation::Stop`)
/// rather than let it fall through to the terminal/agent draft, per
/// `docs/workspace-mode-design.md`'s "everything else is swallowed".
pub(crate) fn handle_workspace_mode_key(
    key_event: &KeyEvent,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
) -> Option<ModeAction> {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return None;
    }

    let mode_key = mode_key_from_key(&key_event.key.logical_key, key_event.modifiers);
    match interpret_mode_keys(&[mode_key]) {
        Interpretation::Action(action) => Some(action),
        Interpretation::Pending | Interpretation::Invalid => None,
    }
}

/// Whether a bare (unmodified) `Esc` on an *agent* pane should enter
/// workspace mode -- the per-kind asymmetry
/// `docs/workspace-mode-design.md` calls for: an agent's message box, unlike
/// a terminal, has no protocol-level claim on raw `Esc`, so it doubles as a
/// second entry path alongside the configured chord
/// (`app::keymap::is_workspace_mode_enter_key`). Excluded during IME
/// composition, where `Esc` is left to the IME. Callers gate this on the
/// active pane actually being an agent pane themselves (see
/// `workspace::view::pane`'s `is_agent()`/`app::input`'s `active_agent`) --
/// this function only checks the key itself.
pub(crate) fn agent_escape_requests_workspace_mode(
    key_event: &KeyEvent,
    ime_composing: bool,
) -> bool {
    !ime_composing
        && key_event.modifiers.is_empty()
        && matches!(key_event.key.logical_key, Key::Named(NamedKey::Escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_key(key: Key, modifiers: Modifiers) -> ModeKey {
        mode_key_from_key(&key, modifiers)
    }

    // --- raw key normalization (`mode_key_from_key`) ---------------------

    #[test]
    fn plain_hjkl_normalize_to_their_mode_keys() {
        assert_eq!(
            mode_key(Key::Character("h".into()), Modifiers::default()),
            ModeKey::H
        );
        assert_eq!(
            mode_key(Key::Character("j".into()), Modifiers::default()),
            ModeKey::J
        );
        assert_eq!(
            mode_key(Key::Character("k".into()), Modifiers::default()),
            ModeKey::K
        );
        assert_eq!(
            mode_key(Key::Character("l".into()), Modifiers::default()),
            ModeKey::L
        );
    }

    #[test]
    fn colon_normalizes_regardless_of_the_shift_that_produced_it() {
        assert_eq!(
            mode_key(Key::Character(":".into()), Modifiers::SHIFT),
            ModeKey::Colon
        );
    }

    #[test]
    fn enter_and_escape_normalize_from_named_keys() {
        assert_eq!(
            mode_key(Key::Named(NamedKey::Enter), Modifiers::default()),
            ModeKey::Enter
        );
        assert_eq!(
            mode_key(Key::Named(NamedKey::Escape), Modifiers::default()),
            ModeKey::Escape
        );
    }

    #[test]
    fn unassigned_keys_normalize_to_other() {
        assert_eq!(
            mode_key(Key::Character("a".into()), Modifiers::default()),
            ModeKey::Other
        );
        assert_eq!(
            mode_key(Key::Named(NamedKey::ArrowLeft), Modifiers::default()),
            ModeKey::Other
        );
    }

    #[test]
    fn a_held_ctrl_alt_or_meta_downgrades_hjkl_to_other() {
        assert_eq!(
            mode_key(Key::Character("h".into()), Modifiers::CONTROL),
            ModeKey::Other
        );
        assert_eq!(
            mode_key(Key::Character("l".into()), Modifiers::ALT),
            ModeKey::Other
        );
        assert_eq!(
            mode_key(Key::Character("j".into()), Modifiers::META),
            ModeKey::Other
        );
    }

    // --- key-sequence interpretation (`interpret_mode_keys`) -------------

    #[test]
    fn hjkl_interpret_to_their_directions() {
        assert_eq!(
            interpret_mode_keys(&[ModeKey::H]),
            Interpretation::Action(ModeAction::Move(Direction::Left))
        );
        assert_eq!(
            interpret_mode_keys(&[ModeKey::J]),
            Interpretation::Action(ModeAction::Move(Direction::Down))
        );
        assert_eq!(
            interpret_mode_keys(&[ModeKey::K]),
            Interpretation::Action(ModeAction::Move(Direction::Up))
        );
        assert_eq!(
            interpret_mode_keys(&[ModeKey::L]),
            Interpretation::Action(ModeAction::Move(Direction::Right))
        );
    }

    #[test]
    fn enter_commits_and_escape_cancels() {
        assert_eq!(
            interpret_mode_keys(&[ModeKey::Enter]),
            Interpretation::Action(ModeAction::Commit)
        );
        assert_eq!(
            interpret_mode_keys(&[ModeKey::Escape]),
            Interpretation::Action(ModeAction::Cancel)
        );
    }

    #[test]
    fn colon_opens_the_palette() {
        assert_eq!(
            interpret_mode_keys(&[ModeKey::Colon]),
            Interpretation::Action(ModeAction::OpenPalette)
        );
    }

    #[test]
    fn an_unassigned_key_is_invalid() {
        assert_eq!(
            interpret_mode_keys(&[ModeKey::Other]),
            Interpretation::Invalid
        );
    }

    #[test]
    fn an_empty_sequence_is_invalid() {
        assert_eq!(interpret_mode_keys(&[]), Interpretation::Invalid);
    }

    #[test]
    fn a_multi_key_sequence_is_invalid_until_the_vocabulary_grows() {
        // v1 has no counts/motions yet, so any sequence longer than one key
        // must degrade safely to `Invalid` rather than panicking on an
        // unmatched slice pattern.
        assert_eq!(
            interpret_mode_keys(&[ModeKey::H, ModeKey::L]),
            Interpretation::Invalid
        );
    }

    // `handle_workspace_mode_key`/`agent_escape_requests_workspace_mode`
    // themselves aren't unit-tested here directly: both take a real
    // `floem::keyboard::KeyEvent`, which wraps a private platform-specific
    // winit field and cannot be constructed from a struct literal outside
    // floem (the same limitation `app::keymap`'s `Chord::matches` works
    // around). Their logic is exhaustively covered through the pure pieces
    // they're built from -- `mode_key_from_key`/`interpret_mode_keys` above,
    // plus `composing_guard_swallows`'s own tests in `workspace::input`.
}
