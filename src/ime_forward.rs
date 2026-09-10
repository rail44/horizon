//! The macOS bridge that lets control-modified keys reach the input method.
//!
//! Root cause (gpui, zed @ 5f8a7413a31769e0882357f90dc424b3962ac72d,
//! `crates/gpui_macos/src/window.rs`'s key-down handler): gpui forwards a
//! native key event to the input context (the IME) only while composing,
//! for printable keys without control/function/platform modifiers, and for
//! modifier-less non-printing keys. Control-modified keys are explicitly
//! excluded ("we skip keys with control or the input handler adds
//! control-characters to the buffer"), and an unhandled key equivalent with
//! non-function modifiers returns without consulting the input context at
//! all — so the IME never sees ctrl+<letter> in any gpui app on macOS, and
//! IME-local shortcuts bound to one (SKK's C-j kana/latin toggle, most
//! prominently) cannot fire. Confirmed against Zed itself; see
//! docs/ime-control-key-forwarding-design.md for the full write-up.
//!
//! The lever this module pulls is the one thing gpui's public API does not
//! offer (no macOS surface — `NSView`/`inputContext`/native events — is
//! reachable from `crates/gpui/src`): during `TerminalView::handle_key` we
//! are still inside AppKit's synchronous dispatch of the original
//! `NSEvent`, so `+[NSApplication currentEvent]` returns the very event
//! gpui declined to forward. Handing that event to
//! `+[NSTextInputContext currentInputContext]` via its public `handleEvent:`
//! is exactly what gpui's IME-first branch does. If the IME claims the key
//! the caller drops it (the PTY never sees it); if not, key handling falls
//! through to the ordinary PTY encoding unchanged.
//!
//! On by default, with no switch (owner decision 2026-09-10): the
//! population this changes is exactly the input sources that claim
//! ctrl+<letter> — SKK-family, the users who want the claim — while every
//! other input source (ABC, Google Japanese Input, ...) passes the key
//! through and behaves exactly as before. The one accepted regression is
//! an IME claiming a ctrl+letter the *shell* should get (terminal emacs
//! with skk.el, where C-j belongs to emacs) — the same trade every
//! terminal-with-SKK macOS setup makes, recorded in
//! docs/ime-control-key-forwarding-design.md.

use gpui::Keystroke;

#[cfg(target_os = "macos")]
use crate::input_trace::input_trace;

/// The pure gate for which keystrokes the shim offers to the IME: control
/// plus exactly one ASCII lowercase letter, and nothing else. Shifted,
/// alt/cmd/function-modified, named, and non-letter keys stay on the
/// ordinary path — the target shortcuts (SKK's C-j, plus the C-a/C-e/C-k
/// chords SKK passes through untouched) are all plain ctrl+letter chords.
pub(crate) fn offerable_control_letter(keystroke: &Keystroke) -> Option<char> {
    let modifiers = &keystroke.modifiers;
    if !modifiers.control
        || modifiers.alt
        || modifiers.shift
        || modifiers.platform
        || modifiers.function
    {
        return None;
    }
    let mut chars = keystroke.key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_lowercase() {
        return None;
    }
    Some(ch)
}

/// Offers the original native key-down event to the input method.
///
/// Returns whether the IME consumed it — the caller must then not send the
/// same key to the PTY. A `false` can mean "not on the main thread", "no
/// native event", "not a key down", "no input context", or "the IME passed
/// the key through"; every one of those falls back to ordinary terminal
/// key handling.
#[cfg(target_os = "macos")]
pub(crate) fn forward_control_key_to_ime() -> bool {
    // `MainThreadMarker::new` is the checked constructor: `None` off the
    // main thread. gpui's event dispatch is main-thread, so this is
    // defensive rather than load-bearing.
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        input_trace!("ime_forward skipped: not on the main thread");
        return false;
    };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    // The original NSEvent gpui is dispatching right now. Under synthetic
    // input (HORIZON_GPUI_DRIVE) there is no native event and nothing to
    // forward.
    let Some(event) = app.currentEvent() else {
        input_trace!("ime_forward skipped: no current native event");
        return false;
    };
    // `currentEvent` can be any event type; only a key down is meaningful
    // to hand to the input context here.
    if event.r#type() != objc2_app_kit::NSEventType::KeyDown {
        input_trace!("ime_forward skipped: current event is not a key down");
        return false;
    }
    let Some(input_context) = objc2_app_kit::NSTextInputContext::currentInputContext(mtm) else {
        input_trace!("ime_forward skipped: no current input context");
        return false;
    };
    input_context.handleEvent(&event)
}

/// No-op off macOS: X11 routes every event through the XIM filter and
/// Wayland delivers keys through the compositor's input method before the
/// app sees them, so the gap this bridges is macOS-specific.
#[cfg(not(target_os = "macos"))]
pub(crate) fn forward_control_key_to_ime() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::offerable_control_letter;
    use gpui::{Keystroke, Modifiers};

    fn keystroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            key_char: None,
            modifiers,
        }
    }

    fn ctrl() -> Modifiers {
        Modifiers::control()
    }

    #[test]
    fn plain_ctrl_letter_is_offerable() {
        assert_eq!(offerable_control_letter(&keystroke("j", ctrl())), Some('j'));
        assert_eq!(offerable_control_letter(&keystroke("a", ctrl())), Some('a'));
    }

    #[test]
    fn any_other_modifier_disqualifies() {
        let combos: &[fn(&mut Modifiers)] = &[
            |m| m.alt = true,
            |m| m.shift = true,
            |m| m.platform = true,
            |m| m.function = true,
        ];
        for set in combos {
            let mut modifiers = ctrl();
            set(&mut modifiers);
            assert_eq!(
                offerable_control_letter(&keystroke("j", modifiers)),
                None,
                "ctrl+<other>+j must stay on the ordinary path"
            );
        }
    }

    #[test]
    fn named_keys_and_non_letters_are_not_offerable() {
        for key in ["enter", "tab", "space", "f1", "up", "1", "J", "あ"] {
            assert_eq!(
                offerable_control_letter(&keystroke(key, ctrl())),
                None,
                "key {key:?} must stay on the ordinary path"
            );
        }
    }
}
