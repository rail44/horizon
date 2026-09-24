//! Physical key and IME delivery, including the state shared by both paths.

use super::{
    font::{font_size, line_height, resolved_font},
    input::{term_key_code, term_modifiers},
    TerminalView,
};
use crate::input_trace::input_trace;
use gpui::*;
use horizon_terminal_core::KeyEventKind;

#[derive(Default)]
pub(super) struct KeyboardInput {
    ime_marked_text: Option<String>,
    ime_commit_guard: ImeCommitGuard,
    key_text_dedup: KeyTextDedup,
}

impl KeyboardInput {
    pub(super) fn marked_text(&self) -> Option<&str> {
        self.ime_marked_text.as_deref()
    }
}

impl TerminalView {
    // Release events have a wire representation only under kitty
    // REPORT_EVENT_TYPES; the core decides emission, so the view maps
    // every key it can name (passing `true` skips the text-vs-key
    // routing gate — releases never ride the text pipeline).
    pub(super) fn handle_key_up(&mut self, event: &KeyUpEvent, cx: &App) {
        if self.keyboard.ime_marked_text.is_some() {
            return;
        }
        let Some(key) = term_key_code(&event.keystroke, true) else {
            return;
        };
        // Release events never carry associated text.
        self.session.read(cx).send_key_with_text(
            key,
            term_modifiers(&event.keystroke.modifiers),
            KeyEventKind::Release,
            None,
        );
    }

    fn keys_as_escape_codes(&self, cx: &App) -> bool {
        self.session
            .read(cx)
            .frame
            .as_ref()
            .is_some_and(|frame| frame.keys_as_escape_codes)
    }

    pub(super) fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        input_trace!(
            "handle_key entry key={:?} modifiers={:?} is_held={}",
            keystroke.key,
            keystroke.modifiers,
            event.is_held
        );
        // While the IME is composing, every keystroke belongs to the IME
        // (candidate selection etc.) — letting it through would
        // double-feed the terminal. The composed result arrives via
        // replace_text_in_range instead.
        if self.keyboard.ime_marked_text.is_some() {
            input_trace!("handle_key key={:?} dropped: ime composing", keystroke.key);
            return;
        }
        // A physical Enter that confirmed an IME composition arrives as
        // an independent KeyDownEvent right after the commit already
        // cleared ime_marked_text above (Wayland's text-input-v3 never
        // lets the compositor consume keys on the client's behalf — see
        // docs/tasks/backlog.md #30). The guard is consumed by the very
        // next key event regardless of outcome, so it can't leak into a
        // later, unrelated keystroke.
        if self
            .keyboard
            .ime_commit_guard
            .should_suppress(&keystroke.key)
        {
            input_trace!(
                "handle_key key={:?} dropped: ime_commit_guard suppressed (phantom enter)",
                keystroke.key
            );
            return;
        }
        // Cmd+C / Cmd+V are host shortcuts, not terminal input (the
        // command-model binding arrives with M3; these are the M1 stand-in).
        if keystroke.modifiers.platform && !keystroke.modifiers.control {
            match keystroke.key.as_str() {
                "c" => {
                    self.session.read(cx).send_copy_selection();
                    input_trace!("handle_key key={:?} sent: CopySelection", keystroke.key);
                    return;
                }
                "v" => {
                    if let Some(text) = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text().map(|text| text.to_string()))
                    {
                        input_trace!(
                            "handle_key key={:?} sent: Paste {}",
                            keystroke.key,
                            crate::input_trace::describe_text(&text)
                        );
                        self.session.read(cx).send_paste(text);
                    } else {
                        input_trace!(
                            "handle_key key={:?} dropped: clipboard empty",
                            keystroke.key
                        );
                    }
                    return;
                }
                _ => {}
            }
        }
        let Some(key) = term_key_code(keystroke, self.keys_as_escape_codes(cx)) else {
            input_trace!(
                "handle_key key={:?} dropped: unmapped (keys_as_escape_codes={})",
                keystroke.key,
                self.keys_as_escape_codes(cx)
            );
            return;
        };
        // Record a plain-char send so `replace_text_in_range` can
        // recognize its own copy of *this* keystroke as the double-feed
        // `KeyTextDedup`'s doc describes, rather than an IME's only
        // delivery. Control combos are excluded: an IME never echoes them
        // as commit text, so they'd only pollute the record with a
        // mismatch.
        if !keystroke.modifiers.control {
            if let (termwiz::input::KeyCode::Char(_), Some(text)) =
                (&key, keystroke.key_char.as_deref())
            {
                self.keyboard.key_text_dedup.note_key_sent(text);
            }
        }
        let kind = if event.is_held {
            KeyEventKind::Repeat
        } else {
            KeyEventKind::Press
        };
        // Preserve GPUI's generated text (Keystroke::key_char) so the daemon
        // can include it as the associated-text subfield when the terminal
        // has negotiated that flag.
        let text = keystroke.key_char.clone();
        input_trace!(
            "handle_key key={:?} sent: structured key kind={:?} text={}",
            keystroke.key,
            kind,
            crate::input_trace::describe_text(text.as_deref().unwrap_or(""))
        );
        self.session.read(cx).send_key_with_text(
            key,
            term_modifiers(&keystroke.modifiers),
            kind,
            text,
        );
    }
}
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The `EntityInputHandler::replace_and_mark_text_in_range` state update:
/// `ime_marked_text` always mirrors the IME's current preedit text
/// exactly, including shrinking all the way to empty (which clears the
/// overlay, same as `unmark_text` would) — see
/// docs/issues/004-ime-preedit-backspace-ghost-head-char.md. Pulled out
/// as a pure function so the exact backspace-to-empty repro is testable
/// without a live gpui `Context`; the original custom-platform bug failed
/// to deliver an empty preedit update, leaving whatever this function last
/// returned stranded as a paint-time "ghost" until the next update.
fn ime_marked_text_for(new_text: &str) -> Option<String> {
    if new_text.is_empty() {
        None
    } else {
        Some(new_text.to_string())
    }
}

/// A composition confirmed via a physical key (Enter) redelivers that
/// key as an independent `KeyDownEvent` essentially in the same input
/// burst as the commit — observed at microseconds-to-low-single-digit
/// milliseconds in the spike's logs. A composition confirmed by mouse
/// click on the candidate window produces no phantom key at all, so the
/// very next keydown may be a genuine, unrelated Enter arriving well
/// after this window (e.g. "compose → click candidate → press Enter to
/// send the line", a natural terminal flow). 100ms is a generous ceiling
/// above the phantom case and comfortably below any plausible human
/// reaction time, so it distinguishes the two without needing to know
/// which one committed the composition.
const IME_COMMIT_PHANTOM_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

/// The pure decision behind the IME "phantom Enter" guard
/// (docs/tasks/backlog.md #30): Wayland's text-input-v3 delivers the
/// physical key that confirmed an IME composition as an independent
/// `KeyDownEvent` *after* the commit already cleared marked text, so a
/// naive `ime_marked_text.is_some()` check can't tell that keydown apart
/// from an ordinary, unrelated keystroke.
///
/// `note_commit` arms the guard (recording when) when a composition was
/// just committed. `should_suppress` is then called with the very next
/// key event's name (regardless of what that key is) and unconditionally
/// disarms the guard — so it can only ever affect the one keydown
/// immediately following a commit, never a later one. It reports
/// "suppress" only when that key is Enter/Return *and* it arrived within
/// [`IME_COMMIT_PHANTOM_WINDOW`] of the commit; every other key (a
/// phantom Space redelivery, ordinary typing, a late genuine Enter after
/// a mouse-click commit, ...) passes through unaffected.
#[derive(Default)]
struct ImeCommitGuard {
    armed_at: Option<std::time::Instant>,
}

impl ImeCommitGuard {
    fn note_commit(&mut self, was_composing: bool) {
        if was_composing {
            self.armed_at = Some(std::time::Instant::now());
        }
    }

    fn should_suppress(&mut self, key: &str) -> bool {
        self.should_suppress_at(key, std::time::Instant::now())
    }

    /// Clock-injected core so the decision stays pure and testable
    /// without sleeping.
    fn should_suppress_at(&mut self, key: &str, now: std::time::Instant) -> bool {
        let Some(armed_at) = self.armed_at.take() else {
            return false;
        };
        key == "enter" && now.saturating_duration_since(armed_at) < IME_COMMIT_PHANTOM_WINDOW
    }
}

/// Generous vs. the gap between `handle_key` sending a `TerminalCommand::Key`
/// and the platform's text-input pipeline independently echoing the same
/// keystroke as `replace_text_in_range` text (observed same-burst, well
/// under a millisecond in practice), comfortably below any plausible
/// human-typing interval — so a *stale* match past this window is treated
/// as a fresh, unrelated commit rather than silently swallowed.
const KEY_TEXT_DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

/// The pure decision behind the direct-mode-IME-commit fix: under kitty
/// "report all keys", an ordinary printable keystroke is sent via the Key
/// path (`handle_key`) *and* independently echoed by the platform's
/// text-input pipeline (`replace_text_in_range`) — the latter must be
/// dropped or it double-feeds the terminal. The old code assumed *every*
/// non-composing commit under kitty mode was one of these echoes; that's
/// false for an IME "direct"/ASCII input mode, which can consume the
/// physical key itself and deliver *only* the commit, with no matching
/// `handle_key` call ever happening — the old assumption silently dropped
/// the only copy.
///
/// `note_key_sent` records the text a Key-path send just delivered.
/// `is_duplicate_of_recent_key` is then called with `replace_text_in_range`'s
/// text and reports "duplicate, drop it" only when that text exactly
/// matches a key-path send from within [`KEY_TEXT_DEDUP_WINDOW`] — an
/// unmatched commit (no recent key, or a mismatched one) passes through
/// untouched, exactly the direct-mode-IME-commit case this exists to fix.
/// One-shot like `ImeCommitGuard`: a lookup always consumes the pending
/// record, matched or not, so it can only ever affect the one commit
/// immediately following a key send.
#[derive(Default)]
struct KeyTextDedup {
    pending: Option<(String, std::time::Instant)>,
}

impl KeyTextDedup {
    fn note_key_sent(&mut self, text: &str) {
        self.pending = Some((text.to_string(), std::time::Instant::now()));
    }

    fn is_duplicate_of_recent_key(&mut self, text: &str) -> bool {
        self.is_duplicate_of_recent_key_at(text, std::time::Instant::now())
    }

    /// Clock-injected core so the decision stays pure and testable without
    /// sleeping.
    fn is_duplicate_of_recent_key_at(&mut self, text: &str, now: std::time::Instant) -> bool {
        let Some((pending_text, at)) = self.pending.take() else {
            return false;
        };
        pending_text == text && now.saturating_duration_since(at) < KEY_TEXT_DEDUP_WINDOW
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        self.keyboard.ime_marked_text.clone()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self
            .keyboard
            .ime_marked_text
            .as_deref()
            .map(utf16_len)
            .unwrap_or(0);
        Some(UTF16Selection {
            range: caret..caret,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.keyboard
            .ime_marked_text
            .as_deref()
            .map(|marked| 0..utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.keyboard.ime_marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_composing = self.keyboard.ime_marked_text.take().is_some();
        input_trace!(
            "replace_text_in_range entry {} was_composing={} keys_as_escape_codes={}",
            crate::input_trace::describe_text(text),
            was_composing,
            self.keys_as_escape_codes(cx)
        );
        self.keyboard.ime_commit_guard.note_commit(was_composing);
        // Under kitty "report all keys", an ordinary printable keypress is
        // sent as TerminalCommand::Key from on_key_down *and* independently
        // echoed here by the platform's text-input pipeline — that copy
        // must be dropped or it double-feeds. An IME composition commit is
        // one exception: it never went through the Key path (`was_composing`
        // covers that). The other: an IME "direct"/ASCII input mode can
        // consume the physical key itself and deliver *only* this commit,
        // with no matching `handle_key` call at all — `key_text_dedup`
        // tells the two apart by whether a matching key-path send actually
        // happened, instead of assuming kitty mode implies one always did
        // (see docs/winit-backend-design.md's "Resolved incidents" ->
        // "Keyboard input pipeline" -> Stage 2 for the bug this replaced;
        // Stage 3 in the same section is why this dedup is still live even
        // for a plain, non-IME echo — the winit-side text-input fallback
        // fires unconditionally alongside the Key path since propagation
        // never stops).
        if !was_composing
            && self.keys_as_escape_codes(cx)
            && self
                .keyboard
                .key_text_dedup
                .is_duplicate_of_recent_key(text)
        {
            input_trace!("replace_text_in_range dropped: duplicate of a key-path send");
            cx.notify();
            return;
        }
        input_trace!(
            "replace_text_in_range sent: structured text input {}",
            crate::input_trace::describe_text(text)
        );
        self.session.read(cx).send_text_input(text.to_string());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keyboard.ime_marked_text = ime_marked_text_for(new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.session.read(cx).frame.as_ref()?.cursor?;
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&resolved_font());
        let cell_width = text_system
            .advance(font_id, px(font_size()), 'M')
            .map(|size| size.width)
            .unwrap_or(px(8.0));
        let origin = element_bounds.origin
            + point(
                cell_width * cursor.col as f32 + cell_width * range_utf16.start as f32,
                px(line_height()) * cursor.row as f32,
            );
        Some(Bounds::new(
            origin,
            gpui::size(cell_width, px(line_height())),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ime_marked_text_for, ImeCommitGuard, KeyTextDedup, IME_COMMIT_PHANTOM_WINDOW,
        KEY_TEXT_DEDUP_WINDOW,
    };
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
    // the actual bug lived upstream (the retired custom platform's
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
}
