//! Paired command channels owned by the core loop and the PTY writer.

use crossbeam_channel::{Receiver, Sender};

use crate::contract::{ScrollWindowRequest, SelectionCommand, TerminalKeyInput};
use crate::core::TerminalColorScheme;
use crate::types::{TerminalMouseReport, TerminalScroll, TerminalSize};

/// Ordered keyboard deliveries after the daemon has decoded wire commands.
/// Keep key, committed text and paste in one FIFO: separate receivers let a
/// submitting Enter overtake the final character or pasted command.
pub enum CoreInput {
    Key(TerminalKeyInput),
    Text(String),
    Paste(String),
}

pub struct CoreReceivers {
    pub(super) resize_rx: Receiver<TerminalSize>,
    pub(super) scroll_rx: Receiver<TerminalScroll>,
    pub(super) mouse_rx: Receiver<TerminalMouseReport>,
    pub(super) input_rx: Receiver<CoreInput>,
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
    pub input_tx: Sender<CoreInput>,
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
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
    let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();
    let (window_tx, window_rx) = crossbeam_channel::unbounded();
    (
        CoreSenders {
            resize_tx,
            scroll_tx,
            mouse_tx,
            input_tx,
            selection_tx,
            focus_tx,
            color_scheme_tx,
            window_tx,
        },
        CoreReceivers {
            resize_rx,
            scroll_rx,
            mouse_rx,
            input_rx,
            selection_rx,
            focus_rx,
            color_scheme_rx,
            window_rx,
        },
    )
}
