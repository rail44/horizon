//! Paired command channels owned by the core loop and the PTY writer.

use crossbeam_channel::{Receiver, Sender};

use crate::core::TerminalColorScheme;
use crate::types::TerminalSize;

/// What a view says of the stream it is sent.
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
    /// A client's bytes for the PTY, in the order they came.
    pub(super) input_rx: Receiver<Vec<u8>>,
    /// Demuxed `TerminalCommand::SetColorScheme` -- a live theme apply's
    /// re-push of the host's color scheme into this already-running
    /// session (see that variant's doc comment).
    pub(super) color_scheme_rx: Receiver<TerminalColorScheme>,
    pub(super) stream_rx: Receiver<StreamCommand>,
}

pub struct CoreSenders {
    pub resize_tx: Sender<TerminalSize>,
    pub input_tx: Sender<Vec<u8>>,
    pub color_scheme_tx: Sender<TerminalColorScheme>,
    pub stream_tx: Sender<StreamCommand>,
}

/// Create the matching command halves for one terminal core. The writer must
/// retain every sender: disconnecting any command input ends the core loop.
pub fn core_channels() -> (CoreSenders, CoreReceivers) {
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();
    let (stream_tx, stream_rx) = crossbeam_channel::unbounded();
    (
        CoreSenders {
            resize_tx,
            input_tx,
            color_scheme_tx,
            stream_tx,
        },
        CoreReceivers {
            resize_rx,
            input_rx,
            color_scheme_rx,
            stream_rx,
        },
    )
}
