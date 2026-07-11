use std::path::PathBuf;

use horizon_session_protocol::{Envelope as ProtocolEnvelope, WireError};
use serde::{Deserialize, Serialize};
use termwiz::input::{KeyCode, Modifiers};
use uuid::Uuid;

use crate::core::TerminalColorScheme;
use crate::types::{
    KeyEventKind, TerminalFrame, TerminalFrameDiff, TerminalMouseReport, TerminalScroll,
    TerminalSelectionPoint, TerminalSize,
};

pub const TERMINAL_CONTROL_KIND: &str = "terminal_control";
pub const TERMINAL_COMMAND_KIND: &str = "terminal_command";
pub const TERMINAL_UPDATE_KIND: &str = "terminal_update";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalSpawnSpec {
    pub shell: String,
    pub args: Vec<String>,
    pub term: String,
    pub scrollback_lines: usize,
    pub color_scheme: TerminalColorScheme,
    pub control_socket: PathBuf,
    pub fallback_cwd: PathBuf,
    pub spawn_source_session_id: Option<Uuid>,
    pub initial_size: TerminalSize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalControl {
    Create(Box<TerminalSpawnSpec>),
    Attach,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        key: KeyCode,
        modifiers: Modifiers,
        event: KeyEventKind,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    Mouse(TerminalMouseReport),
    SelectionStart(TerminalSelectionPoint),
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
    /// A pane focus transition (`true` = gained focus, `false` = lost it),
    /// forwarded to `TerminalCore::focus_input` so it can be reported to
    /// the attached app as `CSI I`/`CSI O` if it negotiated mode 1004. The
    /// source is `app::runtime::wire_focus_reporting`, which composes
    /// Horizon's own window focus with which visible pane is active.
    Focus(bool),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalUpdate {
    Snapshot(TerminalFrame),
    FrameDiff(TerminalFrameDiff),
    Title(Option<String>),
    Bell,
    Clipboard(String),
    Exited,
    Error(String),
}

pub fn encode_terminal_control(
    session_id: Uuid,
    control: &TerminalControl,
) -> Result<ProtocolEnvelope, WireError> {
    ProtocolEnvelope::from_typed(TERMINAL_CONTROL_KIND, Some(session_id), control)
}

pub fn encode_terminal_command(
    session_id: Uuid,
    command: &TerminalCommand,
) -> Result<ProtocolEnvelope, WireError> {
    ProtocolEnvelope::from_typed(TERMINAL_COMMAND_KIND, Some(session_id), command)
}

pub fn encode_terminal_update(
    session_id: Uuid,
    update: &TerminalUpdate,
) -> Result<ProtocolEnvelope, WireError> {
    ProtocolEnvelope::from_typed(TERMINAL_UPDATE_KIND, Some(session_id), update)
}

pub fn decode_terminal_control(envelope: &ProtocolEnvelope) -> Result<TerminalControl, WireError> {
    envelope.decode_payload(TERMINAL_CONTROL_KIND)
}

pub fn decode_terminal_command(envelope: &ProtocolEnvelope) -> Result<TerminalCommand, WireError> {
    envelope.decode_payload(TERMINAL_COMMAND_KIND)
}

pub fn decode_terminal_update(envelope: &ProtocolEnvelope) -> Result<TerminalUpdate, WireError> {
    envelope.decode_payload(TERMINAL_UPDATE_KIND)
}

/// Demuxed selection sub-commands (`TerminalCommand::SelectionStart`/
/// `SelectionUpdate`/`CopySelection`), routed onto their own channel by the
/// host's PTY writer thread — see [`crate::CoreReceivers`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionCommand {
    Start(TerminalSelectionPoint),
    Update(TerminalSelectionPoint),
    Copy,
}
