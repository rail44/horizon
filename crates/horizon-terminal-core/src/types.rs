mod color;
mod frame;
mod key_event;
mod mouse;
mod size;

pub use color::TerminalColor;
pub use frame::{TerminalCursor, TerminalFrame, TerminalLine, TerminalSpan};
pub use key_event::KeyEventKind;
pub use mouse::{
    TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport,
    TerminalScroll, TerminalSelectionPoint,
};
pub use size::TerminalSize;
