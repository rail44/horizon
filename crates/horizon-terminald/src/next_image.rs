//! Starting the daemon's next image with its terminals
//! (`docs/terminald-handover-design.md`): the process executes its
//! executable again, so its id, its children, its PTYs and its listening
//! socket stay.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::handover::{self, Handed};
use crate::pty;
use crate::terminal::TerminalHost;

/// What starting the next image needs, known when this one starts.
pub(crate) struct NextImage {
    /// The executable this image was started from. Read at start: once a
    /// build replaces the file, the process's own link names the replaced
    /// one.
    pub(crate) executable: PathBuf,
    pub(crate) socket: PathBuf,
    /// The listening socket, kept open for the next image.
    pub(crate) listener: RawFd,
    /// The image this process runs, executed where `executable` does not
    /// start: on Linux `/proc/self/exe`, readable after a build replaced
    /// the file.
    pub(crate) again: Option<PathBuf>,
}

impl NextImage {
    /// The executable this process runs, as a path a later `exec` finds
    /// the rebuilt file at.
    pub(crate) fn executable_now() -> std::io::Result<PathBuf> {
        let executable = std::env::current_exe()?;
        // An image started from a replaced file (the fallback below) reads
        // its link with this suffix; the path itself is where a build
        // leaves the next file.
        Ok(
            match executable
                .to_str()
                .and_then(|path| path.strip_suffix(" (deleted)"))
            {
                Some(path) => PathBuf::from(path),
                None => executable,
            },
        )
    }
}

/// Gives every terminal to the next image, and returns only where it
/// could not start: why, with the terminals taken back here.
pub(crate) fn hand_over(terminals: &TerminalHost, next: &NextImage) -> String {
    let handed = terminals.hand_over();
    let reason = start(&handed, next);
    eprintln!("horizon-terminald: the next image did not start ({reason}); going on here");
    let taken = (handed.into_iter())
        .filter_map(|handed| match handed.take_back() {
            Ok(taken) => Some(taken),
            Err(error) => {
                eprintln!("horizon-terminald: a terminal not taken back: {error}");
                None
            }
        })
        .collect();
    terminals.take(taken);
    reason
}

/// Executes the next image with `handed`; returns why where it could not.
fn start(handed: &[Handed], next: &NextImage) -> String {
    let file = match handover::write(handed) {
        Ok(file) => file,
        Err(error) => return format!("the handover was not written: {error}"),
    };
    let mut kept = vec![next.listener, file.as_raw_fd()];
    kept.extend(handed.iter().map(|handed| handed.pty.master_fd()));
    for fd in &kept {
        if let Err(error) = pty::keep_across_exec(*fd) {
            restore(&kept);
            return format!("descriptor {fd} would not stay open: {error}");
        }
    }
    let args = [
        "--socket".into(),
        next.socket.clone().into_os_string(),
        "--listener".into(),
        next.listener.to_string().into(),
        "--adopt".into(),
        file.as_raw_fd().to_string().into(),
    ];
    let mut error = format!(
        "{} did not start: {}",
        next.executable.display(),
        exec(&next.executable, &args)
    );
    eprintln!("horizon-terminald: {error}");
    if let Some(again) = &next.again {
        error = format!("{error}; this build again: {}", exec(again, &args));
    }
    restore(&kept);
    drop(file);
    error
}

fn exec(executable: &Path, args: &[std::ffi::OsString]) -> std::io::Error {
    Command::new(executable).args(args).exec()
}

/// The descriptors go back to closing on `exec`: a program a terminal
/// starts later must not inherit them.
fn restore(kept: &[RawFd]) {
    for fd in kept {
        // SAFETY: `fcntl` with these commands takes no pointers.
        unsafe {
            let flags = libc::fcntl(*fd, libc::F_GETFD);
            if flags != -1 {
                libc::fcntl(*fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

#[cfg(test)]
mod tests;
