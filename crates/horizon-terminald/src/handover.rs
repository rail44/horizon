//! What one image of this daemon gives the next when it hands its
//! terminals over (`docs/terminald-handover-design.md`): written to an
//! anonymous file whose descriptor the next image is started with.
//!
//! A session's descriptor, process id, id and size are what the next
//! image must always read; the rest is read where it can be, and a
//! terminal whose state is not read starts empty.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use horizon_terminal_core::{HandedCore, TerminalColorScheme, TerminalProgramStatus, TerminalSize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pty::Pty;

/// The format of the file. A version the reader does not know is still
/// read for what it can.
const VERSION: u32 = 1;

/// One terminal as this image gives it.
pub(crate) struct Handed {
    pub(crate) session_id: Uuid,
    /// Its master side, open until the next image starts.
    pub(crate) pty: Arc<Pty>,
    pub(crate) pid: u32,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) program_status: Option<TerminalProgramStatus>,
    pub(crate) core: HandedCore,
}

/// One terminal as the next image takes it.
pub(crate) struct Taken {
    pub(crate) session_id: Uuid,
    pub(crate) master: OwnedFd,
    pub(crate) pid: u32,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) program_status: Option<TerminalProgramStatus>,
    pub(crate) core: HandedCore,
}

#[derive(Serialize, Deserialize)]
struct FileContents {
    version: u32,
    sessions: Vec<Session>,
}

#[derive(Serialize, Deserialize)]
struct Session {
    session_id: Uuid,
    master: RawFd,
    pid: u32,
    size: TerminalSize,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    program_status: Option<TerminalProgramStatus>,
    #[serde(default)]
    scrollback_lines: Option<usize>,
    #[serde(default)]
    color_scheme: Option<TerminalColorScheme>,
    /// The engine's state, base64.
    #[serde(default)]
    state: String,
    /// The bytes the core held back, base64.
    #[serde(default)]
    unread: String,
}

/// Writes `sessions` to an anonymous file, and returns its descriptor
/// positioned at the start, closed on `exec` until the caller says
/// otherwise.
pub(crate) fn write(sessions: &[Handed]) -> io::Result<OwnedFd> {
    let contents = FileContents {
        version: VERSION,
        sessions: (sessions.iter())
            .map(|handed| Session {
                session_id: handed.session_id,
                master: handed.pty.master_fd(),
                pid: handed.pid,
                size: handed.core.size,
                cwd: handed.cwd.clone(),
                program_status: handed.program_status.clone(),
                scrollback_lines: Some(handed.core.scrollback_lines),
                color_scheme: Some(handed.core.color_scheme),
                state: STANDARD.encode(&handed.core.state),
                unread: STANDARD.encode(&handed.core.unread),
            })
            .collect(),
    };
    let mut file = anonymous_file()?;
    serde_json::to_writer(&mut file, &contents)?;
    file.flush()?;
    file.rewind()?;
    Ok(OwnedFd::from(file))
}

/// Reads what the previous image wrote to `fd`. A session whose
/// descriptor is not open is left out.
pub(crate) fn read(fd: OwnedFd) -> io::Result<Vec<Taken>> {
    let mut text = Vec::new();
    File::from(fd).read_to_end(&mut text)?;
    let contents: FileContents = serde_json::from_slice(&text)?;
    if contents.version != VERSION {
        eprintln!(
            "horizon-terminald: the handover is version {}, this build reads {VERSION}: \
             reading what it can",
            contents.version
        );
    }
    let mut taken = Vec::new();
    for session in contents.sessions {
        // SAFETY: `fcntl` with this command takes no pointers.
        if unsafe { libc::fcntl(session.master, libc::F_GETFD) } == -1 {
            eprintln!(
                "horizon-terminald: terminal {} came without its PTY",
                session.session_id
            );
            continue;
        }
        // SAFETY: the previous image kept this descriptor open for this
        // session alone, and it is taken once.
        let master = unsafe { OwnedFd::from_raw_fd(session.master) };
        let decoded = |text: &str| STANDARD.decode(text).unwrap_or_default();
        taken.push(Taken {
            session_id: session.session_id,
            master,
            pid: session.pid,
            cwd: session.cwd,
            program_status: session.program_status,
            core: HandedCore {
                size: session.size,
                scrollback_lines: session
                    .scrollback_lines
                    .unwrap_or(horizon_terminal_core::DEFAULT_SCROLLBACK_LINES),
                color_scheme: session.color_scheme.unwrap_or_default(),
                state: decoded(&session.state),
                unread: decoded(&session.unread),
            },
        });
    }
    Ok(taken)
}

/// A file with no name: a `memfd` on Linux, an unlinked temporary file
/// elsewhere.
fn anonymous_file() -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the name is a NUL-terminated string; the flags take no
        // pointers.
        let fd = unsafe {
            libc::memfd_create(c"horizon-terminald-handover".as_ptr(), libc::MFD_CLOEXEC)
        };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new descriptor nothing else owns.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path = std::env::temp_dir().join(format!(
            "horizon-terminald-handover-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::fs::remove_file(&path)?;
        Ok(file)
    }
}

impl Handed {
    /// The terminal as this image takes it back, when the next image did
    /// not start.
    pub(crate) fn take_back(self) -> io::Result<Taken> {
        Ok(Taken {
            session_id: self.session_id,
            master: self.pty.master()?,
            pid: self.pid,
            cwd: self.cwd,
            program_status: self.program_status,
            core: self.core,
        })
    }
}

#[cfg(test)]
mod tests;
