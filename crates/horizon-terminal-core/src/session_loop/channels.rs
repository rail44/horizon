//! Paired command channels owned by the core loop and the PTY writer.

use crossbeam_channel::{Receiver, Sender};

use crate::contract::TerminalKeyInput;
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

/// SPIKE Y. What a view says of the stream it is sent.
pub enum StreamCommand {
    /// It has nothing: the state, then the bytes.
    Want,
    /// No view is there: nothing more.
    Stop,
    /// It has taken this many bytes in.
    Taken(u64),
}

pub struct CoreReceivers {
    pub(super) resize_rx: Receiver<TerminalSize>,
    pub(super) scroll_rx: Receiver<TerminalScroll>,
    pub(super) mouse_rx: Receiver<TerminalMouseReport>,
    pub(super) input_rx: Receiver<CoreInput>,
    pub(super) focus_rx: Receiver<bool>,
    /// Demuxed `TerminalCommand::SetColorScheme` -- a live theme apply's
    /// re-push of the host's color scheme into this already-running
    /// session (see that variant's doc comment).
    pub(super) color_scheme_rx: Receiver<TerminalColorScheme>,
    pub(super) stream_rx: Receiver<StreamCommand>,
}

pub struct CoreSenders {
    pub resize_tx: Sender<TerminalSize>,
    pub scroll_tx: Sender<TerminalScroll>,
    pub mouse_tx: Sender<TerminalMouseReport>,
    pub input_tx: Sender<CoreInput>,
    pub focus_tx: Sender<bool>,
    pub color_scheme_tx: Sender<TerminalColorScheme>,
    pub stream_tx: Sender<StreamCommand>,
}

/// Create the matching command halves for one terminal core. The writer must
/// retain every sender: disconnecting any command input ends the core loop.
pub fn core_channels() -> (CoreSenders, CoreReceivers) {
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
    let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();
    let (stream_tx, stream_rx) = crossbeam_channel::unbounded();
    (
        CoreSenders {
            resize_tx,
            scroll_tx,
            mouse_tx,
            input_tx,
            focus_tx,
            color_scheme_tx,
            stream_tx,
        },
        CoreReceivers {
            resize_rx,
            scroll_rx,
            mouse_rx,
            input_rx,
            focus_rx,
            color_scheme_rx,
            stream_rx,
        },
    )
}
