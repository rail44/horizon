mod config;
mod core;
mod session;
mod types;
pub(crate) mod view;

#[cfg(test)]
pub(crate) use core::TerminalCore;
pub(crate) use session::{initial_terminal_text, TerminalCommand, TerminalSession, TerminalUpdate};
pub(crate) use types::{
    TerminalFrame, TerminalLine, TerminalMouseButton, TerminalMouseKind, TerminalMouseModifiers,
    TerminalMouseReport, TerminalScroll, TerminalSelectionPoint, TerminalSize, TerminalSpan,
};

#[cfg(test)]
pub(crate) use session::terminal_command;

#[cfg(test)]
mod tests;
