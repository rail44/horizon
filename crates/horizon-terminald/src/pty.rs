//! The PTY a terminal session runs in, and the program started in it.
//!
//! A session holds the PTY as its master side's file descriptor and the
//! program as its process id; nothing else of either is kept.

use std::ffi::{CStr, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use horizon_terminal_core::TerminalSize;

/// What is started in a new PTY.
pub(crate) struct Program<'a> {
    /// Looked up on `PATH` when it has no slash; also the program's
    /// `argv[0]`.
    pub(crate) path: &'a str,
    pub(crate) args: &'a [String],
    /// Set over the daemon's own environment.
    pub(crate) env: Vec<(OsString, OsString)>,
    /// The home directory is used where this is not a directory.
    pub(crate) cwd: &'a Path,
}

/// The master side of a PTY.
pub(crate) struct Pty {
    master: OwnedFd,
}

impl Pty {
    /// Opens a PTY of `size` and starts `program` in it, as the leader of
    /// a session of its own with the PTY as its controlling terminal.
    /// Returns the PTY and the program's process id; the program is not
    /// waited for, and its end is seen as the end of what the PTY reads.
    pub(crate) fn spawn(size: TerminalSize, program: Program<'_>) -> io::Result<(Self, u32)> {
        let (master, slave) = open(size)?;
        let mut command = Command::new(program.path);
        command.args(program.args);
        command.current_dir(directory_or_home(program.cwd));
        if std::env::var_os("SHELL").is_none() {
            command.env("SHELL", login_shell());
        }
        command.envs(program.env);
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        let highest = highest_descriptor();
        // SAFETY: the closure runs in the forked child before `exec`, so
        // it may only call async-signal-safe functions: every call in
        // `prepare_child` is a system call that allocates nothing and
        // takes no lock.
        unsafe {
            command.pre_exec(move || prepare_child(highest));
        }
        let child = command.spawn()?;
        Ok((Self { master }, child.id()))
    }

    /// Tells the PTY, and through it the program, its new size.
    pub(crate) fn resize(&self, size: TerminalSize) -> io::Result<()> {
        let size = window_size(size);
        // SAFETY: `TIOCSWINSZ` reads one `winsize` from the pointer, which
        // points at one for the length of the call.
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// What the program writes to the PTY.
    pub(crate) fn reader(&self) -> io::Result<PtyReader> {
        Ok(PtyReader(File::from(self.master.try_clone()?)))
    }

    /// What is written to the program. Dropping it ends the program's
    /// input with an end-of-file.
    pub(crate) fn writer(&self) -> io::Result<PtyWriter> {
        Ok(PtyWriter(File::from(self.master.try_clone()?)))
    }
}

/// Sends the program `SIGHUP`, as a terminal that closes does.
pub(crate) fn hang_up(pid: u32) -> io::Result<()> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `kill` takes no pointers.
    if unsafe { libc::kill(pid, libc::SIGHUP) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) struct PtyReader(File);

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            // Linux reads EIO once no process holds the slave side open:
            // the program and all it started are gone.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

pub(crate) struct PtyWriter(File);

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Drop for PtyWriter {
    fn drop(&mut self) {
        // SAFETY: an all-zero `termios` is a valid value of the plain C
        // struct, and `tcgetattr` writes one through the pointer.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.0.as_raw_fd(), &mut termios) } == 0 {
            // A canonical-mode reader takes EOF only at the start of a
            // line.
            let eof = termios.c_cc[libc::VEOF];
            if eof != 0 {
                let _ = self.0.write_all(&[b'\n', eof]);
            }
        }
    }
}

/// A PTY's two sides, each closed on `exec`.
fn open(size: TerminalSize) -> io::Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave): (RawFd, RawFd) = (-1, -1);
    let mut size = window_size(size);
    // SAFETY: the two descriptors are written through their pointers, the
    // name and the terminal settings are not asked for, and the size is
    // read from its pointer for the length of the call. (The size is
    // `*const` on Linux and `*mut` on the BSDs and macOS.)
    #[allow(clippy::unnecessary_mut_passed)]
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openpty` succeeded, so both are open descriptors that
    // nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    close_on_exec(master.as_raw_fd())?;
    close_on_exec(slave.as_raw_fd())?;
    Ok((master, slave))
}

fn close_on_exec(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fcntl` with these commands takes no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn window_size(size: TerminalSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

/// The highest descriptor number a process may have open, asked before
/// the fork: `sysconf` is not among the functions safe to call after it.
fn highest_descriptor() -> libc::c_int {
    // SAFETY: `sysconf` takes no pointers.
    let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    libc::c_int::try_from(limit)
        .unwrap_or(libc::c_int::MAX)
        .max(3)
}

/// Run in the forked child before `exec`: the signal state a program
/// expects to start with, a session of its own led by it, the PTY on its
/// standard input as its controlling terminal, and every descriptor above
/// the standard three closed on `exec` -- marked rather than closed, so
/// that the standard library's own pipe still reports a failed `exec`.
fn prepare_child(highest: libc::c_int) -> io::Result<()> {
    // SAFETY: each call below takes no pointers, or pointers to values
    // that live for the length of the call; none allocates or locks.
    unsafe {
        for signal in [
            libc::SIGCHLD,
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGALRM,
        ] {
            libc::signal(signal, libc::SIG_DFL);
        }
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
        if libc::setsid() == -1 {
            return Err(io::Error::last_os_error());
        }
        if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
            return Err(io::Error::last_os_error());
        }
        mark_close_on_exec_from(3, highest);
    }
    Ok(())
}

/// Marks every descriptor from `first` up to `highest` closed on `exec`.
///
/// # Safety
///
/// Async-signal-safe; it only issues system calls.
unsafe fn mark_close_on_exec_from(first: libc::c_int, highest: libc::c_int) {
    #[cfg(target_os = "linux")]
    {
        // `close_range` with this flag marks rather than closes (Linux
        // 5.11 and later); on an older kernel it fails and the loop below
        // does the same one descriptor at a time.
        // SAFETY: the system call takes three integers.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                first as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if marked == 0 {
            return;
        }
    }
    for fd in first..highest {
        // SAFETY: `fcntl` with these commands takes no pointers; a number
        // that is not an open descriptor fails and is passed over.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags != -1 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// The user's shell in the password database, else `/bin/sh`: what
/// `SHELL` says where the daemon's environment does not.
fn login_shell() -> OsString {
    password_entry(|entry| entry.pw_shell).unwrap_or_else(|| OsString::from("/bin/sh"))
}

/// `cwd` where it is a directory, else `$HOME`, else the user's home in
/// the password database, else `/`.
fn directory_or_home(cwd: &Path) -> PathBuf {
    if cwd.is_dir() {
        return cwd.to_path_buf();
    }
    std::env::var_os("HOME")
        .or_else(|| password_entry(|entry| entry.pw_dir))
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// One text field of this user's entry in the password database.
fn password_entry(field: fn(&libc::passwd) -> *mut libc::c_char) -> Option<OsString> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: an all-zero `passwd` is a valid value of the plain C struct.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `getpwuid_r` writes the entry and the strings it points to
    // into `entry` and `buffer`, both alive for the call and after it, and
    // sets `found` to `&entry` or to null.
    let result = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() {
        return None;
    }
    let text = field(&entry);
    if text.is_null() {
        return None;
    }
    // SAFETY: the field points into `buffer`, at a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(text) };
    Some(OsStr::from_bytes(text.to_bytes()).to_os_string())
}

#[cfg(test)]
mod tests;
