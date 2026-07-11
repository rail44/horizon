//! The terminal's UI-agnostic brain: VT emulation (`TerminalCore`,
//! wrapping `alacritty_terminal`) and the byte-channel-driven session loop
//! (`run_terminal_core` — 16ms coalescing, the synchronized-update
//! failsafe). No `floem`/`ui` dependency — see
//! `docs/session-daemon-design.md` decisions 8 and 9, and
//! `docs/agent-runtime-split-design.md` for the sibling split
//! `crates/horizon-agent` already went through.
//!
//! Out of scope, deliberately: PTY ownership (`portable-pty`, threads,
//! environment setup) and color *resolution* against a live theme both stay
//! on the host side (`horizon`'s `terminal::session`/`terminal::view`) —
//! this crate only ever sees bytes in, and hands back logical
//! colors/commands/updates over plain channels.

mod contract;
mod core;
mod protocol;
mod session_loop;
mod types;

pub use contract::{SelectionCommand, TerminalCommand, TerminalUpdate};
pub use core::{TerminalColorScheme, TerminalCore};
pub use session_loop::{run_terminal_core, CoreReceivers, CoreSenders, TerminalCoreOptions};
pub use types::{
    apply_frame_diff, compute_frame_diff, KeyEventKind, TerminalColor, TerminalCursor,
    TerminalFrame, TerminalFrameDiff, TerminalLine, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseModifiers, TerminalMouseReport, TerminalRowDiff, TerminalScroll,
    TerminalSelectionPoint, TerminalSize, TerminalSpan,
};

#[cfg(test)]
mod tests;
