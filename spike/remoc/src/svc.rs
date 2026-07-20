//! Item 4 (ergonomics): an `#[rtc::remote]` trait whose method returns a
//! struct bundling live channel halves, mirroring the shape horizon's
//! `attach_terminal` would take.

use horizon_terminal_core::TerminalFrame;
use remoc::prelude::*;
use serde::{Deserialize, Serialize};

/// What the UI sends back on the input path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TerminalInput {
    Bytes(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

/// The bundle `attach_terminal` hands to a client: a live frame stream
/// plus an input channel, both remoted by value inside one struct.
#[derive(Serialize, Deserialize)]
pub struct TerminalAttachment {
    pub frames: rch::watch::Receiver<TerminalFrame>,
    pub input: rch::mpsc::Sender<TerminalInput>,
}

/// Error type demonstrating the required `From<rtc::CallError>` plumbing.
#[derive(Debug, Serialize, Deserialize)]
pub enum AttachError {
    SessionGone,
    Call(rtc::CallError),
}

impl From<rtc::CallError> for AttachError {
    fn from(err: rtc::CallError) -> Self {
        Self::Call(err)
    }
}

#[rtc::remote]
pub trait TerminalService {
    async fn attach_terminal(&mut self) -> Result<TerminalAttachment, AttachError>;
}
