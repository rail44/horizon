//! `TerminalCore`/emulation, the session command/update contract
//! (`TerminalCommand`/`TerminalUpdate`), and the byte-channel-driven session
//! loop (16ms coalescing, the synchronized-update failsafe) live in
//! `horizon-terminal-core` now (`docs/session-daemon-design.md`, decision 9,
//! migration order step 0) -- a UI-agnostic library crate with no `floem`
//! dependency, mirroring `crates/horizon-agent`. This module re-exports its
//! public surface so existing `crate::terminal::X` call sites throughout the
//! app are unaffected by the move.
//!
//! What stays here is the spawn layer: PTY ownership (`portable-pty`),
//! thread spawning, environment setup, and the `HORIZON_PTY_TRACE`
//! diagnostic tap (`session::{environment, runtime, trace}`) -- PTY
//! ownership is a host/process concern per decision 9, not something the
//! extracted crate takes on.
pub(crate) mod config;
mod session;
pub(crate) mod view;

pub(crate) use horizon_terminal_core::{
    KeyEventKind, TerminalCommand, TerminalFrame, TerminalLine, TerminalMouseButton,
    TerminalMouseKind, TerminalMouseModifiers, TerminalMouseReport, TerminalScroll,
    TerminalSelectionPoint, TerminalSize, TerminalSpan, TerminalUpdate,
};
pub(crate) use session::{initial_terminal_text, sample_cwd, TerminalSession};

#[cfg(test)]
pub(crate) use session::terminal_command;

#[cfg(test)]
mod tests;
