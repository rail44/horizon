// `super::*` is avoided here: `terminal/mod.rs` does `use gpui::*`, which
// glob-exports gpui's own `test` attribute macro and shadows `std::test`,
// sending plain `#[test]` fns through gpui's async test harness instead
// (see `src/terminal/session.rs`'s tests module for the same workaround).
use super::{font_from_stack, ImeCommitGuard, DEFAULT_FONT_FAMILY, IME_COMMIT_PHANTOM_WINDOW};

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
