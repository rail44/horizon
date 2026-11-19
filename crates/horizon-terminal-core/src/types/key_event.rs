use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Classifies a key event per the Kitty keyboard protocol's "event types"
/// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>, gated on the
/// `REPORT_EVENT_TYPES` progressive-enhancement flag): an initial press, an
/// OS/winit-synthesized repeat while the key is held, or a release.
///
/// Threaded from the view's key handling to `TerminalCore::key_stroke`,
/// where the engine's encoder writes a repeat or a release differently
/// from a press only where the program asked for event types.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}
