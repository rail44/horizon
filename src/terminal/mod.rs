//! The terminal pane view: grid-positioned span painting, key routing
//! through `TerminalCommand::Key`, and IME via `EntityInputHandler`.
//! Patterns and their provenance are recorded in
//! docs/gpui-migration-design.md and
//! docs/research/gpui-terminal-implementations.md.
//!
//! Headless verification taps (promoted from the spike):
//! - `HORIZON_GPUI_DUMP=<path>` mirrors every snapshot (text + span
//!   color table) to a file.
//! - `HORIZON_GPUI_DRIVE=<bytes>` types bytes into the session shortly
//!   after startup; `HORIZON_GPUI_DRIVE_ENTER=1` sends the trailing
//!   newline as a `TerminalCommand::Key` to exercise the core encoder.
//!   Note this bypasses `handle_key`/`replace_text_in_range` below
//!   entirely (it writes straight to the session's command channel), so
//!   it cannot exercise or verify the real input pipeline the rest of
//!   this file implements.
//! - `HORIZON_INPUT_TRACE=1` (or a file path) traces every hop of the
//!   real input pipeline instead — see `crate::input_trace`.

mod input;
mod session;
#[cfg(test)]
mod tests;

pub use session::TerminalSession;

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use horizon_terminal_core::{
    KeyEventKind, TerminalCommand, TerminalFrame, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseReport, TerminalScroll, TerminalSize,
};

use self::input::{
    cell_from_position, scroll_lines_from_wheel, term_key_code, term_modifiers,
    terminal_mouse_button, terminal_mouse_modifiers,
};
use crate::input_trace::input_trace;
use crate::theme;

// Font values come from config.toml ([ui].font_family, [terminal].
// font_size/line_height) and are startup-only, like the Floem shell.
//
// `font_family` is a CSS-style comma-separated font stack (as used by the
// retired Floem shell): the first entry is the primary family, the rest
// are fallbacks. gpui's `Font::fallbacks` (backed by cosmic-text on Linux)
// tries each fallback in order when the primary is missing a glyph. Note
// this matching is by *exact* family-name string against a font file's
// embedded name -- fontconfig generic aliases like "monospace" are not
// resolved the way `fc-match` would resolve them, so only literal family
// names (e.g. "DejaVu Sans Mono") actually work as stack entries; unknown
// names are silently dropped rather than causing a resolution failure.
#[cfg(target_os = "macos")]
const DEFAULT_FONT_FAMILY: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// Parse a comma-separated font stack into a `gpui::Font`: the first
/// non-empty, trimmed entry becomes the primary family, any remaining
/// entries become `Font::fallbacks`. Falls back to [`DEFAULT_FONT_FAMILY`]
/// if `raw` has no usable entries.
fn font_from_stack(raw: &str) -> Font {
    let mut entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
    let primary = entries.next().unwrap_or(DEFAULT_FONT_FAMILY).to_string();
    let fallbacks: Vec<String> = entries.map(str::to_string).collect();
    let mut resolved = font(primary);
    if !fallbacks.is_empty() {
        resolved.fallbacks = Some(FontFallbacks::from_fonts(fallbacks));
    }
    resolved
}

fn resolved_font() -> Font {
    static FONT: std::sync::OnceLock<Font> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let raw = horizon_config::load()
            .ui
            .font_family
            .clone()
            .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string());
        font_from_stack(&raw)
    })
    .clone()
}

fn font_size() -> f32 {
    static SIZE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| horizon_config::load().terminal.font_size.unwrap_or(13.0))
}

fn line_height() -> f32 {
    static HEIGHT: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *HEIGHT.get_or_init(|| {
        horizon_config::load()
            .terminal
            .line_height
            .map(|value| value as f32)
            .unwrap_or_else(|| (font_size() * 17.0 / 13.0).round())
    })
}

/// Paint-time geometry shared with event handlers (which need to convert
/// window-relative pixel positions into cell coordinates). Written every
/// paint, read by the mouse handlers.
#[derive(Clone, Copy)]
struct PaintMetrics {
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

pub struct TerminalView {
    // The pane's session — owned by the shell's session store, not this
    // view, so a closed pane detaches rather than terminates.
    session: Entity<TerminalSession>,
    tx: crossbeam_channel::Sender<TerminalCommand>,
    focus_handle: FocusHandle,
    // Shared with the paint closure (which only gets &mut App, not the
    // entity) so bounds-driven resize can be deduped without an update.
    last_size: Rc<Cell<TerminalSize>>,
    metrics: Rc<Cell<Option<PaintMetrics>>>,
    // IME preedit — client-side only, never sent to the PTY. The commit
    // path (replace_text_in_range) writes raw UTF-8 bytes instead.
    ime_marked_text: Option<String>,
    // Guards against the "phantom" physical Enter that Wayland's
    // text-input-v3 delivers independently of an IME commit (see
    // docs/tasks/backlog.md #30).
    ime_commit_guard: ImeCommitGuard,
    // Recognizes `replace_text_in_range`'s copy of a plain keystroke
    // `handle_key` already sent via the Key path (kitty "report all
    // keys" mode) so it can be dropped without dropping a commit that has
    // no matching physical key — the case an IME "direct"/ASCII input
    // mode produces when it consumes the physical key itself and only
    // ever forwards `commit_string`. See `KeyTextDedup`'s doc.
    key_text_dedup: KeyTextDedup,
    // Local text selection drag in progress (mouse-reporting off).
    selecting: bool,
    // The button held while the app has mouse reporting on, so drags and
    // the release report the same button the press did.
    reporting_button: Option<TerminalMouseButton>,
    _session_observation: Subscription,
}

impl TerminalView {
    pub fn new(
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tx = session.read(cx).sender();
        let observation = cx.observe(&session, |_, _, cx| cx.notify());
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            session,
            tx,
            focus_handle,
            last_size: Rc::new(Cell::new(TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            })),
            metrics: Rc::new(Cell::new(None)),
            ime_marked_text: None,
            ime_commit_guard: ImeCommitGuard::default(),
            key_text_dedup: KeyTextDedup::default(),
            selecting: false,
            reporting_button: None,
            _session_observation: observation,
        }
    }

    fn mouse_reporting(&self, cx: &App) -> bool {
        self.session
            .read(cx)
            .frame
            .as_ref()
            .is_some_and(|frame| frame.mouse_reporting)
    }

    fn cell_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<horizon_terminal_core::TerminalSelectionPoint> {
        let metrics = self.metrics.get()?;
        Some(cell_from_position(
            position,
            metrics.origin,
            metrics.cell_width,
            metrics.line_height,
        ))
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent, cx: &App) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_reporting(cx) {
            let Some(button) = terminal_mouse_button(event.button) else {
                return;
            };
            self.reporting_button = Some(button);
            let _ = self.tx.send(TerminalCommand::Mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Press,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            }));
        } else if event.button == MouseButton::Left {
            self.selecting = true;
            let _ = self.tx.send(TerminalCommand::SelectionStart(point));
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &App) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_reporting(cx) {
            let Some(button) = self.reporting_button else {
                return;
            };
            let _ = self.tx.send(TerminalCommand::Mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Drag,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            }));
        } else if self.selecting {
            let _ = self.tx.send(TerminalCommand::SelectionUpdate(point));
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &App) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_reporting(cx) {
            let button = self
                .reporting_button
                .take()
                .or_else(|| terminal_mouse_button(event.button));
            let Some(button) = button else {
                return;
            };
            let _ = self.tx.send(TerminalCommand::Mouse(TerminalMouseReport {
                kind: TerminalMouseKind::Release,
                button,
                point,
                modifiers: terminal_mouse_modifiers(&event.modifiers),
            }));
        } else if event.button == MouseButton::Left && self.selecting {
            self.selecting = false;
            let _ = self.tx.send(TerminalCommand::SelectionUpdate(point));
        }
    }

    fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent) {
        let Some(point) = self.cell_at(event.position) else {
            return;
        };
        if let Some(lines) = scroll_lines_from_wheel(&event.delta) {
            let _ = self
                .tx
                .send(TerminalCommand::Scroll(TerminalScroll { lines, point }));
        }
    }

    // Release events have a wire representation only under kitty
    // REPORT_EVENT_TYPES; the core decides emission, so the view maps
    // every key it can name (passing `true` skips the text-vs-key
    // routing gate — releases never ride the text pipeline).
    fn handle_key_up(&mut self, event: &KeyUpEvent) {
        if self.ime_marked_text.is_some() {
            return;
        }
        let Some(key) = term_key_code(&event.keystroke, true) else {
            return;
        };
        let _ = self.tx.send(TerminalCommand::Key {
            key,
            modifiers: term_modifiers(&event.keystroke.modifiers),
            event: KeyEventKind::Release,
        });
    }

    fn keys_as_escape_codes(&self, cx: &App) -> bool {
        self.session
            .read(cx)
            .frame
            .as_ref()
            .is_some_and(|frame| frame.keys_as_escape_codes)
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
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
        if self.ime_marked_text.is_some() {
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
        if self.ime_commit_guard.should_suppress(&keystroke.key) {
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
                    let _ = self.tx.send(TerminalCommand::CopySelection);
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
                        let _ = self.tx.send(TerminalCommand::Paste(text));
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
                self.key_text_dedup.note_key_sent(text);
            }
        }
        let kind = if event.is_held {
            KeyEventKind::Repeat
        } else {
            KeyEventKind::Press
        };
        input_trace!(
            "handle_key key={:?} sent: TerminalCommand::Key kind={:?}",
            keystroke.key,
            kind
        );
        let _ = self.tx.send(TerminalCommand::Key {
            key,
            modifiers: term_modifiers(&keystroke.modifiers),
            event: kind,
        });
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
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
        self.ime_marked_text.clone()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.ime_marked_text.as_deref().map(utf16_len).unwrap_or(0);
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
        self.ime_marked_text
            .as_deref()
            .map(|marked| 0..utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime_marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_composing = self.ime_marked_text.take().is_some();
        input_trace!(
            "replace_text_in_range entry {} was_composing={} keys_as_escape_codes={}",
            crate::input_trace::describe_text(text),
            was_composing,
            self.keys_as_escape_codes(cx)
        );
        self.ime_commit_guard.note_commit(was_composing);
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
            && self.key_text_dedup.is_duplicate_of_recent_key(text)
        {
            input_trace!("replace_text_in_range dropped: duplicate of a key-path send");
            cx.notify();
            return;
        }
        input_trace!(
            "replace_text_in_range sent: TerminalCommand::Input {}",
            crate::input_trace::describe_text(text)
        );
        let _ = self
            .tx
            .send(TerminalCommand::Input(text.as_bytes().to_vec()));
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
        self.ime_marked_text = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_string())
        };
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

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let tx = self.tx.clone();
        let last_size = self.last_size.clone();
        let metrics = self.metrics.clone();
        let focus_handle = self.focus_handle.clone();
        fn on_down(
            view: &mut TerminalView,
            event: &MouseDownEvent,
            _window: &mut Window,
            cx: &mut Context<TerminalView>,
        ) {
            view.handle_mouse_down(event, cx);
        }
        fn on_up(
            view: &mut TerminalView,
            event: &MouseUpEvent,
            _window: &mut Window,
            cx: &mut Context<TerminalView>,
        ) {
            view.handle_mouse_up(event, cx);
        }
        div()
            .size_full()
            .bg(rgb(theme::background()))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                view.handle_key(event, cx);
            }))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _window, _cx| {
                view.handle_key_up(event);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(on_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(on_down))
            .on_mouse_up(MouseButton::Left, cx.listener(on_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(on_up))
            .on_mouse_up(MouseButton::Right, cx.listener(on_up))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                view.handle_mouse_move(event, cx);
            }))
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _window, _cx| {
                view.handle_scroll_wheel(event);
            }))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        paint_terminal(bounds, &entity, &tx, &last_size, &metrics, window, cx);
                    },
                )
                .size_full(),
            )
    }
}

fn paint_terminal(
    bounds: Bounds<Pixels>,
    entity: &Entity<TerminalView>,
    tx: &crossbeam_channel::Sender<TerminalCommand>,
    last_size: &Rc<Cell<TerminalSize>>,
    metrics: &Rc<Cell<Option<PaintMetrics>>>,
    window: &mut Window,
    cx: &mut App,
) {
    let text_system = window.text_system().clone();
    let font = resolved_font();
    let font_size = px(font_size());
    let line_height = px(line_height());
    let font_id = text_system.resolve_font(&font);
    let cell_width = text_system
        .advance(font_id, font_size, 'M')
        .map(|size| size.width)
        .unwrap_or(px(8.0));
    metrics.set(Some(PaintMetrics {
        origin: bounds.origin,
        cell_width,
        line_height,
    }));

    let cols = (f32::from(bounds.size.width) / f32::from(cell_width)).floor() as u16;
    let rows = (f32::from(bounds.size.height) / f32::from(line_height)).floor() as u16;
    let size = TerminalSize {
        cols: cols.max(2),
        rows: rows.max(2),
        pixel_width: (f32::from(cell_width) * cols.max(2) as f32) as u16,
        pixel_height: (f32::from(line_height) * rows.max(2) as f32) as u16,
    };
    if last_size.get() != size {
        last_size.set(size);
        let _ = tx.send(TerminalCommand::Resize(size));
    }

    let (session, marked_text) = {
        let view = entity.read(cx);
        (view.session.clone(), view.ime_marked_text.clone())
    };
    let Some(frame) = session.read(cx).frame.clone() else {
        return;
    };

    let default_bg = theme::to_hsla(theme::resolve(
        horizon_terminal_core::TerminalColor::Named(
            alacritty_terminal::vte::ansi::NamedColor::Background,
        ),
        &frame.palette_overrides,
    ));

    // Grid-positioned painting (the pattern every surveyed GPUI terminal
    // converges on): each span is painted at its computed column offset,
    // never left to shaped-text flow, and glyph advances are snapped to
    // the cell grid via shape_line's force_width when the span is
    // width-uniform.
    for (row, line) in frame.lines.iter().enumerate() {
        if row >= size.rows as usize {
            break;
        }
        let y = line_height * row as f32;
        let mut col = 0_usize;
        for span in &line.spans {
            let x = cell_width * col as f32;
            col += span.columns;
            if span.text.is_empty() {
                continue;
            }
            let origin = bounds.origin + point(x, y);

            let bg = theme::to_hsla(theme::resolve(span.bg, &frame.palette_overrides));
            if bg != default_bg {
                window.paint_quad(fill(
                    Bounds::new(
                        origin,
                        gpui::size(cell_width * span.columns as f32, line_height),
                    ),
                    bg,
                ));
            }

            if span.text.trim().is_empty() {
                continue;
            }
            let fg = theme::to_hsla(theme::resolve(span.fg, &frame.palette_overrides));
            let run = TextRun {
                len: span.text.len(),
                font: font.clone(),
                color: fg,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // Snap glyphs to the cell grid only when every char in the
            // span occupies the same number of columns; a mixed-width
            // span keeps natural shaping (positioned correctly at its
            // start column, with only intra-span drift possible).
            let chars = span.text.chars().count();
            let force_width = if span.columns == chars {
                Some(cell_width)
            } else if span.columns == chars * 2 {
                Some(cell_width * 2.0)
            } else {
                None
            };
            let shaped =
                text_system.shape_line(span.text.clone().into(), font_size, &[run], force_width);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
    }

    // IME preedit overlay: paint the composing text at the cursor cell,
    // underlined, over an opaque background quad that hides whatever the
    // grid has there. The regular cursor is suppressed while composing.
    if let Some(marked) = marked_text.filter(|marked| !marked.is_empty()) {
        if let Some(cursor) = frame.cursor {
            let origin = bounds.origin
                + point(
                    cell_width * cursor.col as f32,
                    line_height * cursor.row as f32,
                );
            let columns: usize = marked.chars().map(|ch| char_columns(ch).max(1)).sum();
            window.paint_quad(fill(
                Bounds::new(origin, gpui::size(cell_width * columns as f32, line_height)),
                default_bg,
            ));
            let fg = theme::to_hsla(theme::resolve(
                horizon_terminal_core::TerminalColor::Named(
                    alacritty_terminal::vte::ansi::NamedColor::Foreground,
                ),
                &frame.palette_overrides,
            ));
            let run = TextRun {
                len: marked.len(),
                font: font.clone(),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let shaped = text_system.shape_line(marked.into(), font_size, &[run], None);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
        return;
    }

    if let Some(cursor) = frame.cursor {
        let origin = bounds.origin
            + point(
                cell_width * cursor.col as f32,
                line_height * cursor.row as f32,
            );
        let mut color = theme::to_hsla(theme::resolve(
            horizon_terminal_core::TerminalColor::Named(
                alacritty_terminal::vte::ansi::NamedColor::Cursor,
            ),
            &frame.palette_overrides,
        ));
        color.a = 0.6;
        window.paint_quad(fill(
            Bounds::new(origin, gpui::size(cell_width, line_height)),
            color,
        ));
    }
}

/// East-Asian width of a char in terminal columns (mirrors the
/// `char_width` helper in horizon-terminal-core's frame module).
fn char_columns(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    ch.width().unwrap_or(0)
}

/// Plain text plus a per-line span/color table (logical colors as
/// parsed, cursor position) — the headless half of visual verification;
/// actual pixel output still needs eyes on the window.
fn dump_frame(frame: &TerminalFrame) -> String {
    use std::fmt::Write as _;

    let mut out = frame.text.clone();
    out.push_str("\n--- spans ---\n");
    if let Some(cursor) = frame.cursor {
        let _ = writeln!(out, "cursor: row={} col={}", cursor.row, cursor.col);
    }
    for (row, line) in frame.lines.iter().enumerate() {
        for span in &line.spans {
            if span.text.trim().is_empty() {
                continue;
            }
            let _ = writeln!(
                out,
                "row {row}: {:?} fg={:?} bg={:?}",
                span.text, span.fg, span.bg
            );
        }
    }
    out
}
