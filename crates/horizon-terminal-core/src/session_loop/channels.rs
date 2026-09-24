//! Paired command channels owned by the core loop and the PTY writer.

use crossbeam_channel::{Receiver, Sender};
use termwiz::input::{KeyCode, Modifiers};

use crate::contract::{ScrollWindowRequest, SelectionCommand};
use crate::core::TerminalColorScheme;
use crate::types::{KeyEventKind, TerminalMouseReport, TerminalScroll, TerminalSize};

pub struct CoreReceivers {
    pub(super) resize_rx: Receiver<TerminalSize>,
    pub(super) scroll_rx: Receiver<TerminalScroll>,
    pub(super) mouse_rx: Receiver<TerminalMouseReport>,
    pub(super) paste_rx: Receiver<String>,
    pub(super) key_rx: Receiver<(KeyCode, Modifiers, KeyEventKind, Option<String>)>,
    /// Committed text for which no key identity is available, most notably
    /// an IME commit (`TerminalCommand::TextInput`). Encoded by the core
    /// according to the live Kitty keyboard mode.
    pub(super) text_rx: Receiver<String>,
    pub(super) selection_rx: Receiver<SelectionCommand>,
    pub(super) focus_rx: Receiver<bool>,
    /// Demuxed `TerminalCommand::SetColorScheme` -- a live theme apply's
    /// re-push of the host's color scheme into this already-running
    /// session (see that variant's doc comment).
    pub(super) color_scheme_rx: Receiver<TerminalColorScheme>,
    /// Demuxed `TerminalCommand::RequestScrollWindow` -- a client's request
    /// for a scrollback window (`docs/terminal-scrollback-design.md` §7.1).
    /// The loop answers by calling `TerminalCore::snapshot_window` and
    /// putting the window on the events mpsc as
    /// `TerminalUpdate::ScrollWindow`, never moving the live `display_offset`.
    pub(super) window_rx: Receiver<ScrollWindowRequest>,
}

pub struct CoreSenders {
    pub resize_tx: Sender<TerminalSize>,
    pub scroll_tx: Sender<TerminalScroll>,
    pub mouse_tx: Sender<TerminalMouseReport>,
    pub paste_tx: Sender<String>,
    pub key_tx: Sender<(KeyCode, Modifiers, KeyEventKind, Option<String>)>,
    /// Committed text for which no key identity is available, most notably
    /// an IME commit (`TerminalCommand::TextInput`).
    pub text_tx: Sender<String>,
    pub selection_tx: Sender<SelectionCommand>,
    pub focus_tx: Sender<bool>,
    pub color_scheme_tx: Sender<TerminalColorScheme>,
    pub window_tx: Sender<ScrollWindowRequest>,
}

/// Create the matching command halves for one terminal core. The writer must
/// retain every sender: disconnecting any command input ends the core loop.
pub fn core_channels() -> (CoreSenders, CoreReceivers) {
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
    let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
    let (key_tx, key_rx) = crossbeam_channel::unbounded();
    let (text_tx, text_rx) = crossbeam_channel::unbounded();
    let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
    let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();
    let (window_tx, window_rx) = crossbeam_channel::unbounded();
    (
        CoreSenders {
            resize_tx,
            scroll_tx,
            mouse_tx,
            paste_tx,
            key_tx,
            text_tx,
            selection_tx,
            focus_tx,
            color_scheme_tx,
            window_tx,
        },
        CoreReceivers {
            resize_rx,
            scroll_rx,
            mouse_rx,
            paste_rx,
            key_rx,
            text_rx,
            selection_rx,
            focus_rx,
            color_scheme_rx,
            window_rx,
        },
    )
}
