// `super::*` is avoided here: `terminal/mod.rs` does `use gpui::*`, which
// glob-exports gpui's own `test` attribute macro and shadows `std::test`,
// sending plain `#[test]` fns through gpui's async test harness instead
// (see `src/terminal/session.rs`'s tests module for the same workaround).
use super::{
    font_from_stack, ime_marked_text_for, ImeCommitGuard, KeyTextDedup, DEFAULT_FONT_FAMILY,
    IME_COMMIT_PHANTOM_WINDOW, KEY_TEXT_DEDUP_WINDOW,
};

#[test]
fn stack_parses_primary_and_fallbacks() {
    let resolved =
        font_from_stack("Iosevka Nerd Font Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    let fallbacks = resolved.fallbacks.expect("fallbacks should be set");
    assert_eq!(
        fallbacks.fallback_list(),
        &[
            "Symbols Nerd Font Mono".to_string(),
            "Noto Sans Mono CJK JP".to_string(),
        ]
    );
}

#[test]
fn single_family_has_no_fallbacks() {
    let resolved = font_from_stack("Iosevka Nerd Font Mono");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    assert!(resolved.fallbacks.is_none());
}

#[test]
fn trims_whitespace_around_entries() {
    let resolved = font_from_stack("  Iosevka Nerd Font Mono ,  monospace  ");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    assert_eq!(
        resolved.fallbacks.unwrap().fallback_list(),
        &["monospace".to_string()]
    );
}

#[test]
fn drops_empty_entries_between_commas() {
    let resolved = font_from_stack("Iosevka Nerd Font Mono,, monospace");
    assert_eq!(
        resolved.fallbacks.unwrap().fallback_list(),
        &["monospace".to_string()]
    );
}

#[test]
fn empty_string_falls_back_to_default_family() {
    let resolved = font_from_stack("");
    assert_eq!(resolved.family, DEFAULT_FONT_FAMILY);
    assert!(resolved.fallbacks.is_none());
}

#[test]
fn blank_string_falls_back_to_default_family() {
    let resolved = font_from_stack("   ,  ,  ");
    assert_eq!(resolved.family, DEFAULT_FONT_FAMILY);
    assert!(resolved.fallbacks.is_none());
}

// backlog-30: confirming a Japanese IME composition with Enter must not
// send an extra `\r` to the PTY. These drive `ImeCommitGuard` directly —
// the pure decision extracted from `TerminalView::replace_text_in_range`
// (commit) and `TerminalView::handle_key` (the following KeyDownEvent) —
// mirroring the real call sequence: replace_and_mark_text_in_range
// (compose) sets `ime_marked_text`, then replace_text_in_range (commit)
// calls `note_commit(was_composing)`, then the physical key event calls
// `should_suppress(key)`.

#[test]
fn no_prior_composition_never_suppresses() {
    let mut guard = ImeCommitGuard::default();
    // No replace_and_mark_text_in_range / replace_text_in_range happened
    // (was_composing == false), so an ordinary Enter is untouched.
    guard.note_commit(false);
    assert!(!guard.should_suppress("enter"));
}

#[test]
fn enter_confirming_a_composition_is_suppressed_exactly_once() {
    let mut guard = ImeCommitGuard::default();
    // replace_and_mark_text_in_range("えっ…") then replace_text_in_range
    // (commit) with marked text present at entry.
    guard.note_commit(true);
    // The phantom physical Enter that confirmed the composition.
    assert!(guard.should_suppress("enter"));
    // A second, deliberate Enter press right after (e.g. to submit a
    // command) must go through normally — the guard only ever covers
    // the one keydown immediately following a commit.
    assert!(!guard.should_suppress("enter"));
}

#[test]
fn rapid_typing_after_commit_is_not_suppressed() {
    let mut guard = ImeCommitGuard::default();
    guard.note_commit(true);
    // The very next key is an ordinary printable character, not Enter —
    // must pass through, and it consumes the guard.
    assert!(!guard.should_suppress("a"));
    // A later Enter (unrelated to the commit) is unaffected too.
    assert!(!guard.should_suppress("enter"));
}

#[test]
fn commit_via_space_does_not_swallow_a_later_enter() {
    let mut guard = ImeCommitGuard::default();
    // Composition committed by Space/candidate selection rather than
    // Enter. Wayland still redelivers the physical key that triggered
    // the commit as an independent KeyDownEvent, just as it does for
    // Enter — here that's Space, which the guard doesn't treat as a
    // plausible confirming key, so it passes through unaffected and
    // consumes the guard.
    guard.note_commit(true);
    assert!(!guard.should_suppress("space"));
    // The user's next, genuinely separate Enter press (e.g. to submit
    // the command) must not be eaten — the guard was already consumed.
    assert!(!guard.should_suppress("enter"));
}

#[test]
fn consecutive_compositions_each_suppress_independently() {
    let mut guard = ImeCommitGuard::default();
    guard.note_commit(true);
    assert!(guard.should_suppress("enter"));
    // A second, independent composition later in the same session.
    guard.note_commit(true);
    assert!(guard.should_suppress("enter"));
}

#[test]
fn phantom_enter_within_the_window_is_suppressed() {
    let mut guard = ImeCommitGuard::default();
    let before = std::time::Instant::now();
    guard.note_commit(true);
    // The phantom Enter arrives in the same input burst as the commit —
    // a few ms later is a realistic delay, comfortably inside the window.
    let shortly_after = before + std::time::Duration::from_millis(5);
    assert!(guard.should_suppress_at("enter", shortly_after));
}

#[test]
fn enter_after_the_window_passes_through_a_mouse_click_commit() {
    let mut guard = ImeCommitGuard::default();
    let before = std::time::Instant::now();
    guard.note_commit(true);
    // A composition committed by mouse click on the candidate window
    // produces no phantom key at all, so the guard stays armed until the
    // next keydown. If that next keydown is a genuine, unrelated Enter
    // arriving well after the phantom-key window (compose -> click
    // candidate -> press Enter to send the line), it must not be eaten.
    let well_after_the_window = before + IME_COMMIT_PHANTOM_WINDOW * 3;
    assert!(!guard.should_suppress_at("enter", well_after_the_window));
}

// docs/issues/004-ime-preedit-backspace-ghost-head-char.md: the owner's
// exact dogfooding repro is backspacing a composition down one character
// at a time -- "あいう" -> "あい" -> "あ" -> "" -- with no Commit in
// between (composition continues, awaiting more kana). `ime_marked_text_for`
// is the pure state update behind `TerminalView::replace_and_mark_text_in_range`;
// the actual bug lived upstream (`crates/horizon-winit-platform`'s
// `handle_ime` never called it at all for an empty preedit update), but
// this pins the overlay's own contract -- it always mirrors the current
// preedit exactly, including the final empty step -- so a future change
// here can't reintroduce the ghost from this side either.

#[test]
fn preedit_backspace_to_empty_clears_the_marked_text() {
    assert_eq!(ime_marked_text_for("あいう"), Some("あいう".to_string()));
    assert_eq!(ime_marked_text_for("あい"), Some("あい".to_string()));
    assert_eq!(ime_marked_text_for("あ"), Some("あ".to_string()));
    // The final backspace shrinks the preedit to nothing -- this must
    // clear the overlay, not retain the last non-empty value ("あ") as a
    // ghost.
    assert_eq!(ime_marked_text_for(""), None);
}

#[test]
fn cleared_marked_text_paints_nothing() {
    // Mirrors the paint site's own guard in `paint_terminal`
    // (`marked_text.filter(|marked| !marked.is_empty())`): once the
    // preedit has shrunk to empty, there is nothing left to paint at the
    // cursor cell.
    let marked_text = ime_marked_text_for("");
    assert!(marked_text
        .as_deref()
        .filter(|marked| !marked.is_empty())
        .is_none());
}

// `KeyTextDedup` drives `TerminalView::replace_text_in_range`'s decision to
// drop its copy of a keystroke `handle_key` already sent via the Key path
// (kitty "report all keys" mode) -- without dropping a commit that has no
// matching physical key, which is what an IME "direct"/ASCII input mode
// produces (it consumes the physical key itself and only ever forwards
// `commit_string`; see docs/winit-backend-design.md's "Resolved incidents"
// -> "Keyboard input pipeline" -> Stage 2). The three cases every change
// here must keep correct:
//
// 1. Ordinary kitty-mode typing: both `handle_key` and
//    `replace_text_in_range` fire for the same keystroke -- the second
//    copy must still be dropped (`kitty_mode_typing_drops_the_duplicate_echo`).
// 2. An IME composition commit: never went through the Key path at all --
//    must always pass through, matched or not
//    (`composition_commit_with_no_key_send_is_never_a_duplicate`; the real
//    call site also short-circuits this via `was_composing`, but the
//    dedup itself must be safe standalone too).
// 3. A direct-mode IME commit with no matching physical key: must pass
//    through, not be silently dropped -- the bug this type fixes
//    (`direct_mode_commit_with_no_prior_key_is_not_a_duplicate`).

#[test]
fn kitty_mode_typing_drops_the_duplicate_echo() {
    let mut dedup = KeyTextDedup::default();
    // handle_key sent 'a' via TerminalCommand::Key...
    dedup.note_key_sent("a");
    // ...and the text-input pipeline echoes the same keystroke moments
    // later -- recognized as the same delivery, so the second copy must
    // be dropped or the terminal double-feeds.
    assert!(dedup.is_duplicate_of_recent_key("a"));
}

#[test]
fn composition_commit_with_no_key_send_is_never_a_duplicate() {
    let mut dedup = KeyTextDedup::default();
    // No handle_key call happened for a composed IME commit (it never
    // goes through the Key path) -- nothing pending, so the multi-char
    // composed text is never mistaken for a duplicate.
    assert!(!dedup.is_duplicate_of_recent_key("えっ"));
}

#[test]
fn direct_mode_commit_with_no_prior_key_is_not_a_duplicate() {
    let mut dedup = KeyTextDedup::default();
    // The bug this type fixes: an IME "direct"/ASCII input mode consumes
    // the physical key and delivers *only* this commit -- handle_key
    // never ran, so nothing is pending, and the commit must pass through
    // as the sole delivery rather than being dropped as an assumed echo.
    assert!(!dedup.is_duplicate_of_recent_key("a"));
}

#[test]
fn mismatched_text_is_not_a_duplicate() {
    let mut dedup = KeyTextDedup::default();
    dedup.note_key_sent("a");
    // An unrelated commit landing right after an unrelated key send must
    // not be swallowed just because kitty mode is on.
    assert!(!dedup.is_duplicate_of_recent_key("b"));
}

#[test]
fn is_one_shot_like_ime_commit_guard() {
    let mut dedup = KeyTextDedup::default();
    dedup.note_key_sent("a");
    assert!(dedup.is_duplicate_of_recent_key("a"));
    // The match already consumed the pending record; a second, unrelated
    // commit of the same text right after (no new key send) must not
    // match again.
    assert!(!dedup.is_duplicate_of_recent_key("a"));
}

#[test]
fn stale_key_outside_the_window_is_not_a_duplicate() {
    let mut dedup = KeyTextDedup::default();
    let before = std::time::Instant::now();
    dedup.note_key_sent("a");
    // A pathologically delayed echo past the window is treated as an
    // unrelated, standalone commit rather than assumed-matched -- a
    // double-feed (visible duplicate character) is a far smaller cost
    // than the alternative failure mode (silently dropping real input).
    let well_after_the_window = before + KEY_TEXT_DEDUP_WINDOW * 3;
    assert!(!dedup.is_duplicate_of_recent_key_at("a", well_after_the_window));
}
