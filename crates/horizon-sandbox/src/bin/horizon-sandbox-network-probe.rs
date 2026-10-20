//! Syscall probe used by Linux containment integration tests.

use std::io::Write;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::unix::net::UnixStream;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(mode) = args.next() else {
        fail("missing mode");
    };
    let Some(target) = args.next() else {
        fail("missing target");
    };

    let result = match mode.as_str() {
        "tcp" => TcpStream::connect(&target).map(|_| ()),
        "thread" => {
            // Connect from a deliberately non-main thread: seccomp notify
            // reports the calling thread's TID, so supervisor bugs in the
            // pidfd path only show up off the thread-group leader.
            std::thread::spawn(move || TcpStream::connect(&target).map(|_| ()))
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("probe thread panicked")))
        }
        "bind" => TcpListener::bind(&target).map(|_| ()),
        "udp" => UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.send_to(b"HORIZON-UDP-PROBE", &target).map(|_| ())),
        "unix" => UnixStream::connect(&target).map(|_| ()),
        #[cfg(target_os = "linux")]
        "truncate" => truncate_path(&target),
        #[cfg(target_os = "linux")]
        "io_uring" => io_uring_setup(),
        #[cfg(target_os = "linux")]
        "non-utf8-create" => non_utf8_create(&target),
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        "x32-create" => x32_create(&target),
        _ => fail("unknown mode"),
    };

    match result {
        Ok(()) => println!("PROBE-CONNECTED"),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "PROBE-DENIED: {error}");
            std::process::exit(23);
        }
    }
}

/// `truncate(2)` by path; coreutils `truncate` opens the file instead.
#[cfg(target_os = "linux")]
fn truncate_path(target: &str) -> std::io::Result<()> {
    let path = std::ffi::CString::new(target).map_err(std::io::Error::other)?;
    // SAFETY: path is NUL-terminated and outlives the call.
    if unsafe { libc::truncate(path.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// One `io_uring_setup(2)` call with default parameters.
#[cfg(target_os = "linux")]
fn io_uring_setup() -> std::io::Result<()> {
    // `struct io_uring_params` is 120 bytes; all-zero asks for defaults.
    let mut params = [0_u8; 120];
    // SAFETY: params is a writable buffer of the kernel's struct size.
    let fd = unsafe { libc::syscall(libc::SYS_io_uring_setup, 1_u32, params.as_mut_ptr()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a descriptor this process just received.
    unsafe {
        libc::close(fd as libc::c_int);
    }
    Ok(())
}

/// Creates a file whose name is not valid UTF-8 inside directory `target`.
#[cfg(target_os = "linux")]
fn non_utf8_create(target: &str) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let name = std::path::Path::new(target).join(std::ffi::OsStr::from_bytes(b"name-\xff"));
    std::fs::File::create(name).map(|_| ())
}

/// `openat(AT_FDCWD, target, O_WRONLY|O_CREAT)` issued through the x32 ABI
/// (`__X32_SYSCALL_BIT` set on the syscall number).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn x32_create(target: &str) -> std::io::Result<()> {
    const X32_SYSCALL_BIT: libc::c_long = 0x4000_0000;
    let path = std::ffi::CString::new(target).map_err(std::io::Error::other)?;
    // SAFETY: path is NUL-terminated and outlives the call.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat | X32_SYSCALL_BIT,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT,
            0o644,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a descriptor this process just received.
    unsafe {
        libc::close(fd as libc::c_int);
    }
    Ok(())
}

fn fail(message: &str) -> ! {
    eprintln!("horizon-sandbox-network-probe: {message}");
    std::process::exit(2)
}
