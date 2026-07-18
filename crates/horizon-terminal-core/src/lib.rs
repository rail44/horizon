//! The terminal's UI-agnostic brain: the byte-channel-driven session loop
//! (`run_terminal_core` — 16ms coalescing, the synchronized-update
//! failsafe) plus the contract types it speaks. `run_terminal_core` and
//! this contract are THE public seam; VT emulation itself (`TerminalCore`,
//! wrapping `alacritty_terminal`) is an internal engine the session loop
//! drives and is not exported. No `floem`/`ui` dependency — see
//! `docs/session-daemon-design.md` decisions 8 and 9, and
//! `docs/agent-runtime-split-design.md` for the sibling split
//! `crates/horizon-agent` already went through.
//!
//! Out of scope, deliberately: PTY ownership (`portable-pty`, threads,
//! environment setup) stays in `horizon-sessiond`, while color *resolution*
//! against a live theme stays in Horizon's `terminal::view`. This crate only
//! ever sees bytes in, and hands back logical colors/commands/updates over
//! plain channels.

mod contract;
mod core;
mod protocol;
mod session_loop;
mod types;

pub use contract::{
    decode_terminal_command, decode_terminal_control, decode_terminal_update,
    encode_terminal_command, encode_terminal_control, encode_terminal_update, ClipboardDestination,
    SelectionCommand, TerminalAttachResult, TerminalCommand, TerminalControl, TerminalSpawnSpec,
    TerminalSummary, TerminalUpdate, TERMINAL_COMMAND_KIND, TERMINAL_CONTROL_KIND,
    TERMINAL_UPDATE_KIND,
};
pub use core::{TerminalColorScheme, DEFAULT_SCROLLBACK_LINES};
pub use session_loop::{run_terminal_core, CoreReceivers, CoreSenders, TerminalCoreOptions};
pub use types::{
    apply_frame_diff, compute_frame_diff, KeyEventKind, NamedColor, TerminalColor, TerminalCursor,
    TerminalFrame, TerminalFrameDiff, TerminalLine, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseModifiers, TerminalMouseReport, TerminalRowDiff, TerminalScroll,
    TerminalSelectionKind, TerminalSelectionPoint, TerminalSize, TerminalSpan,
};

#[cfg(test)]
mod tests;
