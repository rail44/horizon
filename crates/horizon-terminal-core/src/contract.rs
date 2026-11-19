use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use termwiz::input::{KeyCode, Modifiers};
use uuid::Uuid;

use crate::core::TerminalColorScheme;
use crate::types::{KeyEventKind, TerminalSize};

// The v10 remoc cutover deleted this module's envelope bindings — the
// `terminal_control`/`terminal_command`/`terminal_update` kind constants,
// their `encode_*`/`decode_*` helpers, and the request-id-correlated
// `TerminalControl`/`TerminalAttachResult` discovery/attach vocabulary.
// Discovery and attach are rtc calls on `crate::wire::TerminalHub` now
// (`list_terminals`/`create_terminal`/`attach_terminal`), and
// commands/updates ride a `TerminalAttachment`'s typed channels. This
// module still owns only the domain vocabulary itself — serde-plain and
// remoc-free, per `docs/remoc-adoption-design.md` §2; remoc enters this
// crate in `wire` alone.

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSpawnSpec {
    pub shell: String,
    pub args: Vec<String>,
    pub term: String,
    pub scrollback_lines: usize,
    pub color_scheme: TerminalColorScheme,
    pub control_socket: PathBuf,
    pub fallback_cwd: PathBuf,
    #[serde(default)]
    pub spawn_source_session_id: Option<Uuid>,
    pub initial_size: TerminalSize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSummary {
    pub session_id: Uuid,
}

/// A structured key event: key identity plus the platform-produced text,
/// if any, that this key generated. A view's terminal writes it
/// (`TerminalCore::key_stroke`); it does not cross the wire.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalKeyInput {
    /// termwiz's own serde shape, pinned by the termwiz version — an
    /// external type the schema artifact cannot introspect, so it appears
    /// there as "any value". A termwiz bump that changes this encoding is
    /// invisible to the schema checker and must be reviewed as the wire
    /// change it is.
    #[schemars(with = "serde_json::Value")]
    pub key: KeyCode,
    /// See `key`: termwiz-owned serde shape, "any value" in the schema.
    #[schemars(with = "serde_json::Value")]
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,
    /// Platform-produced text for this key event, copied from GPUI's
    /// `Keystroke::key_char`. `None` means the key generated no text or the
    /// platform did not provide it.
    pub text: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalCommand {
    /// Bytes for the PTY: what a view's terminal wrote for a key, a paste,
    /// a report to the program. The session loop writes them after what
    /// the PTY has already said is answered (`docs/terminal-stream-design.md`).
    Input(Vec<u8>),
    Resize(TerminalSize),
    /// Re-pushes the host's live theme-derived color scheme into a
    /// *running* session's `TerminalCore` (`TerminalCore::set_color_scheme`),
    /// so a subsequent OSC 10/11/12 query reply reflects a live theme apply
    /// (`Reload Config` / the theme settings view) instead of the stale
    /// spawn-time snapshot (`TerminalSpawnSpec::color_scheme`). App-set
    /// live overrides (an OSC 4/10/11/12 write the attached app made
    /// itself) still win over this, exactly as they do over the spawn-time
    /// scheme: the engine answers from its palette, and the scheme is its
    /// default palette (`TerminalCore::set_color_scheme`).
    SetColorScheme(TerminalColorScheme),
    Shutdown,
    /// Client → daemon: a view is here and has nothing: the
    /// terminal's state, then what the PTY writes from there on.
    WantState,
    /// Client → daemon: no view is here any more: nothing of the
    /// state or the PTY's bytes is to be sent until one asks again.
    StopState,
    /// Client → daemon: this many bytes of `Output` have been
    /// taken in. The daemon holds what it sends against these: a view that
    /// falls too far behind is sent no more until it has caught up, and
    /// then the state again.
    OutputTaken(u64),
}

/// A desktop-notification request a terminal app posted through the PTY
/// stream — OSC 9 (iTerm2-style: body only) or OSC 777 `notify`
/// (rxvt-style: optional summary + body). Extracted from the raw bytes
/// before the engine sees them (`core::osc_notify`) and delivered to the
/// client as [`TerminalUpdate::Notification`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalNotification {
    /// The summary line (OSC 777's `notify ; <title> ; <body>` first
    /// argument); `None` for OSC 9, which carries only a body.
    pub title: Option<String>,
    /// The message body (OSC 9's whole payload, OSC 777's second argument).
    pub body: String,
}

/// What a program in the terminal says of its own state, read from the
/// root record of its OSC 7501 reports (the Program Status Protocol,
/// `core::program_status`) and delivered to the client as
/// [`TerminalUpdate::ProgramStatus`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalProgramStatus {
    pub state: ProgramState,
    /// What the program waits for. Only with [`ProgramState::Blocked`],
    /// and not always then.
    pub blocked_on: Option<ProgramBlockedOn>,
    /// The program's name for itself, such as `claude-code`.
    pub app: Option<String>,
    /// Why, or what about, in the program's words.
    pub message: Option<String>,
}

/// The protocol's states.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum ProgramState {
    /// At rest, waiting for the next instruction.
    Idle,
    Working,
    /// Finished a piece of work the user has not looked at yet.
    Done,
    /// Cannot go on until the user does something.
    Blocked,
    /// Failed and stopped.
    Error,
}

/// What a blocked program is waiting for (the protocol's `kind`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum ProgramBlockedOn {
    Permission,
    Question,
    Auth,
}

/// What the daemon sends a client, on the attachment's `events` mpsc
/// channel (`TerminalAttachment::events`): the terminal's state and the
/// bytes after it, to a client that asked (`docs/terminal-stream-design.md`),
/// and what the session says of itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalUpdate {
    Title(Option<String>),
    Bell,
    Clipboard {
        text: String,
        destination: ClipboardDestination,
    },
    /// Daemon → client: a desktop-notification request (OSC 9 / OSC 777
    /// `notify`) a terminal app posted — see [`TerminalNotification`].
    /// Whether it actually surfaces is the client's call: Horizon escalates
    /// to the OS notification center only when the session isn't already
    /// the focused pane of the focused window.
    Notification(TerminalNotification),
    Exited,
    Error(String),
    /// Daemon → client: the terminal's whole state, as its engine
    /// writes it out, in pieces: the last has `more` false. What follows is
    /// the PTY's bytes from that state on.
    State {
        bytes: Vec<u8>,
        more: bool,
    },
    /// Daemon → client: what the PTY wrote, as the daemon's own
    /// terminal took it in.
    Output(Vec<u8>),
    /// Daemon → client: the terminal was resized here, between
    /// the bytes before and the bytes after.
    Resized(TerminalSize),
    /// Daemon → client: the daemon's clock let a synchronized
    /// update's hold go here, the program never having let it go. No byte
    /// says so: a view's terminal lets its own hold go at this place in
    /// the stream, and keeps no clock of its own.
    HoldReleased,
    /// Daemon → client: the session's current working directory, read from
    /// its shell process (board #69). Sent when the value changes and once to
    /// every newly installed subscriber, so a reattaching client learns the
    /// current value without a request (`docs/terminald-split-design.md`).
    WorkingDirectory(PathBuf),
    /// Daemon → client: what the program in the session says of its own
    /// state ([`TerminalProgramStatus`]), or `None` once it has taken its
    /// report back, as one does on its way out. Sent when it changes and,
    /// while there is one, once to every newly installed subscriber, as
    /// the working directory is.
    ProgramStatus(Option<TerminalProgramStatus>),
}

/// Which OS clipboard buffer a [`TerminalUpdate::Clipboard`] targets.
/// `Clipboard` is the explicit-copy path (Cmd/Ctrl+C, OSC 52 writes);
/// `Primary` is the X11/Wayland middle-click-paste buffer, written
/// automatically as selection completes/updates (Linux convention -- select
/// = copy to primary). One update type with a destination discriminator,
/// rather than a second full clipboard pathway.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum ClipboardDestination {
    Clipboard,
    Primary,
}
