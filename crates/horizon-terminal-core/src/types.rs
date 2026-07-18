mod color;
mod frame;
mod key_event;
mod mouse;
mod size;

pub use color::TerminalColor;
pub(crate) use frame::frame_text;
pub use frame::{
    apply_frame_diff, compute_frame_diff, TerminalCursor, TerminalFrame, TerminalFrameDiff,
    TerminalLine, TerminalRowDiff, TerminalSpan,
};
pub use key_event::KeyEventKind;
pub use mouse::{
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
    TerminalScroll, TerminalSelectionKind, TerminalSelectionPoint,
};
pub use size::TerminalSize;
