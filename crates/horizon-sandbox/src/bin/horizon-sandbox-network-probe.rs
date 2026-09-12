//! Dependency-free syscall probe used by Linux containment integration tests.

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

fn fail(message: &str) -> ! {
    eprintln!("horizon-sandbox-network-probe: {message}");
    std::process::exit(2)
}
