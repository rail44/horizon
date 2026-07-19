mod color;
mod frame;
mod key_event;
mod mouse;
mod size;

pub use color::{NamedColor, TerminalColor};
pub(crate) use frame::frame_text;
pub use frame::{
    apply_frame_diff, compute_frame_diff, TerminalCursor, TerminalCursorShape, TerminalFrame,
    TerminalFrameDiff, TerminalLine, TerminalRowDiff, TerminalSelection, TerminalSpan,
    TerminalUnderline,
};
pub use key_event::KeyEventKind;
pub use mouse::{
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
    TerminalScroll, TerminalSelectionKind, TerminalSelectionPoint,
};
pub use size::TerminalSize;
