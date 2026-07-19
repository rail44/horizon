//! Test-only fixture, run as the *sandboxed command* in
//! `tests/containment.rs`: a minimal HTTP client that reaches the network
//! purely over a UNIX domain socket bridge -- exactly the "reachability"
//! mechanism `docs/agent-approval-design.md`'s network layer decided on
//! (`horizon_sandbox_proxy::UdsBridge`). Deliberately not `curl`/`reqwest`:
//! those speak the CONNECT/absolute-form forward-proxy protocol over a
//! plain TCP connection to the proxy, not over an arbitrary bind-mounted
//! UNIX socket, so this probe crafts the bytes itself instead of depending
//! on whichever HTTP client happens to support that (few, if any, do).
//!
//! Usage: `uds_http_probe <bridge-socket-path> <target-host:port>`
//!
//! Prints exactly one status line to stdout, plus the origin's response
//! body on success (so a test can also assert on which origin actually
//! answered, not just that *a* 2xx came back):
//!   `PROBE-OK <status>`      -- CONNECT accepted, tunnel established
//!   `PROBE-DENIED <status>`  -- proxy refused the CONNECT
//!   `PROBE-ERROR <message>`  -- couldn't talk to the bridge at all

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn status_code(response_head: &str) -> &str {
    response_head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("???")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(socket_path), Some(target)) = (args.next(), args.next()) else {
        eprintln!("usage: uds_http_probe <bridge-socket-path> <target-host:port>");
        std::process::exit(2);
    };

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(stream) => stream,
        Err(e) => {
            println!("PROBE-ERROR connect to bridge: {e}");
            return;
        }
    };

    let connect_req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    if let Err(e) = stream.write_all(connect_req.as_bytes()) {
        println!("PROBE-ERROR write CONNECT: {e}");
        return;
    }

    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            println!("PROBE-ERROR read CONNECT response: {e}");
            return;
        }
    };
    let connect_response = String::from_utf8_lossy(&buf[..n]).to_string();
    let status = status_code(&connect_response).to_string();

    if !status.starts_with('2') {
        println!("PROBE-DENIED {status}");
        return;
    }

    let get_req = format!("GET / HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n");
    if let Err(e) = stream.write_all(get_req.as_bytes()) {
        println!("PROBE-ERROR write GET: {e}");
        return;
    }

    let mut body = String::new();
    if let Err(e) = stream.read_to_string(&mut body) {
        println!("PROBE-ERROR read GET response: {e}");
        return;
    }

    println!("PROBE-OK {}", status_code(&body));
    println!("{body}");
}
