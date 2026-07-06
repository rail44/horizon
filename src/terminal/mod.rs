// `pub(crate)`, not private: `ui::theme` builds `TerminalColors` directly
// (`ui::theme::compute_terminal_colors`, the `Reload Config`-safe home for
// what used to be a startup-only cache here — see `terminal::config::
// resolved_colors`'s doc comment), so it needs `terminal::config::
// TerminalColors` visible from outside this module tree.
pub(crate) mod config;
mod core;
mod protocol;
mod session;
mod types;
pub(crate) mod view;

#[cfg(test)]
pub(crate) use core::TerminalCore;
pub(crate) use session::{initial_terminal_text, TerminalCommand, TerminalSession, TerminalUpdate};
pub(crate) use types::{
    KeyEventKind, TerminalFrame, TerminalLine, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseModifiers, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize, TerminalSpan,
};

#[cfg(test)]
pub(crate) use session::terminal_command;

#[cfg(test)]
mod tests;
