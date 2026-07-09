/// Classifies a key event per the Kitty keyboard protocol's "event types"
/// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>, gated on the
/// `REPORT_EVENT_TYPES` progressive-enhancement flag): an initial press, an
/// OS/winit-synthesized repeat while the key is held, or a release.
///
/// Threaded from `app::keymap`/`workspace::input` (where floem's
/// `Event::KeyDown`/`Event::KeyUp` and winit's `KeyEvent::repeat` flag are
/// classified — see `app::keymap::terminal_key_event_kind`) down through
/// `TerminalCommand::Key` to `terminal::core::TerminalCore::encode_key` and
/// `terminal::protocol::kitty_keyboard`, which is the only place a
/// non-`Press` kind changes the bytes actually sent — see that module's doc
/// for how (and when) it does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

impl KeyEventKind {
    /// Whether this event kind counts as the key being held down, for
    /// encoders that only ever distinguish down/up and have no concept of a
    /// *repeated* press — termwiz's legacy `KeyCode::encode`, and this
    /// module's own legacy fallbacks (`legacy_bytes`/`legacy_text_key`).
    /// `Repeat` counts as "down": without `REPORT_EVENT_TYPES` negotiated, a
    /// repeat must produce byte-for-byte the same output as an ordinary
    /// press (see `kitty_keyboard`'s module doc).
    pub fn is_down(self) -> bool {
        !matches!(self, KeyEventKind::Release)
    }
}
