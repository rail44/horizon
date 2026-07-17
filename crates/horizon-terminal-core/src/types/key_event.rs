use serde::{Deserialize, Serialize};

/// Classifies a key event per the Kitty keyboard protocol's "event types"
/// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>, gated on the
/// `REPORT_EVENT_TYPES` progressive-enhancement flag): an initial press, an
/// OS/winit-synthesized repeat while the key is held, or a release.
///
/// Threaded from `src/terminal/mod.rs`'s key handling (where gpui's
/// `KeyDownEvent`/`KeyUpEvent` and its `is_held` repeat flag are classified)
/// down through `TerminalCommand::Key` to
/// `terminal::core::TerminalCore::encode_key` and
/// `terminal::protocol::kitty_keyboard`, which is the only place a
/// non-`Press` kind changes the bytes actually sent — see that module's doc
/// for how (and when) it does.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
