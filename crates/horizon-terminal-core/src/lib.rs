//! The terminal's UI-agnostic brain: the byte-channel-driven session loop
//! (`run_terminal_core` — the stream to a view, the synchronized-update
//! failsafe) plus the contract types it speaks, and the terminal itself
//! (`TerminalCore`, over libghostty-vt), which the daemon's loop and a
//! view each have one of (`docs/terminal-stream-design.md`). No UI
//! dependency — see
//! `docs/session-daemon-design.md` decisions 8 and 9, and
//! `docs/agent-runtime-split-design.md` for the sibling split
//! `crates/horizon-agent` already went through.
//!
//! Out of scope, deliberately: PTY ownership (`portable-pty`, threads,
//! environment setup) stays in `horizon-terminald`, while color *resolution*
//! against a live theme stays in Horizon's `theme::resolve`
//! (`src/theme/ansi.rs`). This crate only ever sees bytes in, and hands
//! back logical colors/commands/updates over plain channels.
//!
//! One exception to "plain channels", added by
//! `docs/runtime-crate-alignment-design.md` phase 2: [`wire`] holds this
//! runtime's hub trait, its version pair, and its schema artifact — the
//! terminal wire now lives with the terminal runtime instead of in a union
//! protocol crate shared with the agent wire. remoc is confined to that one
//! module; everything else here stays transport-free.

mod contract;
mod core;
mod session_loop;
mod types;
pub mod wire;

pub use contract::{
    ClipboardDestination, ProgramBlockedOn, ProgramState, TerminalCommand, TerminalKeyInput,
    TerminalNotification, TerminalProgramStatus, TerminalSpawnSpec, TerminalSummary,
    TerminalUpdate,
};
pub use core::{DEFAULT_SCROLLBACK_LINES, KeyStroke, TerminalColorScheme, TerminalCore};
pub use session_loop::{
    CoreReceivers, CoreSenders, StreamCommand, TerminalCoreOptions, core_channels,
    run_terminal_core,
};
pub use types::{
    KeyEventKind, NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame,
    TerminalLine, TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers,
    TerminalMouseReport, TerminalRun, TerminalScroll, TerminalScrollWindow, TerminalSelection,
    TerminalSelectionKind, TerminalSelectionPoint, TerminalSize, TerminalSpan, TerminalStyle,
    TerminalUnderline, fixed_palette_rgb,
};

#[cfg(test)]
mod program_status_probe;
#[cfg(test)]
mod tests;
