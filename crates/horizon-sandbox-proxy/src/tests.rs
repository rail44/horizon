//! Wire-level tests against a real running [`AllowlistProxy`]: a plain TCP
//! client connects directly to `proxy.addr()` and speaks raw HTTP/1.1. This
//! exercises the real wiring end-to-end
//! (hudsucker's CONNECT parsing, `HttpContext` construction from a real
//! accepted connection, `handler::AllowlistHandler`'s allow/deny/refusal
//! shape) -- see `handler.rs`'s own test module for why a handler-only
//! unit test isn't possible (`HttpContext` has no public constructor).

use std::io::Result as IoResult;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::{Allowlist, AllowlistProxy, DENIAL_HEADER, DENIAL_REASON_NOT_ALLOWLISTED};

/// A one-shot HTTP origin: accepts exactly one connection, replies `200`
/// with `marker` as the body, then stops. Loopback-only, ephemeral port.
async fn spawn_origin(marker: &'static str) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await; // discard the request itself
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            marker.len(),
            marker
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    addr
}

/// Sends a CONNECT for `target` over a fresh connection to `proxy_addr`,
/// returning the connection (for a follow-up tunneled request) plus the
/// raw response bytes read back so far.
async fn send_connect(proxy_addr: SocketAddr, target: &str) -> IoResult<(TcpStream, String)> {
    let mut stream = TcpStream::connect(proxy_addr).await?;
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    Ok((stream, String::from_utf8_lossy(&buf[..n]).into_owned()))
}

#[tokio::test]
async fn allowed_host_tunnels_through_to_the_real_origin() {
    let origin_addr = spawn_origin("ALLOWED-ORIGIN-BODY").await;
    let proxy = AllowlistProxy::spawn(Allowlist::new([origin_addr.ip().to_string()]))
        .await
        .expect("proxy should start");

    let (mut stream, head) = send_connect(proxy.addr(), &origin_addr.to_string())
        .await
        .expect("CONNECT should succeed at the transport level");
    assert!(head.starts_with("HTTP/1.1 200"), "head: {head}");

    let get = format!("GET / HTTP/1.1\r\nHost: {origin_addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(get.as_bytes()).await.unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(
        body.contains("ALLOWED-ORIGIN-BODY"),
        "expected the tunnel to reach the real origin, got: {body}"
    );
}

#[tokio::test]
async fn denied_host_gets_refused_with_the_boundary_crossing_marker() {
    let proxy = AllowlistProxy::spawn(Allowlist::new(["127.0.0.9"]))
        .await
        .expect("proxy should start");

    // Nothing needs to be listening at this address: a denied CONNECT must
    // be refused before the proxy ever dials out.
    let (_stream, head) = send_connect(proxy.addr(), "127.0.0.42:1")
        .await
        .expect("the proxy should still respond, just with a refusal");
    assert!(head.starts_with("HTTP/1.1 403"), "head: {head}");
    assert!(
        head.to_lowercase().contains(&DENIAL_HEADER.to_lowercase()),
        "expected the boundary-crossing marker header, got: {head}"
    );
    assert!(
        head.contains(DENIAL_REASON_NOT_ALLOWLISTED),
        "expected the denial reason value, got: {head}"
    );
}

#[tokio::test]
async fn empty_allowlist_denies_a_host_that_would_otherwise_be_reachable() {
    let origin_addr = spawn_origin("SHOULD-NEVER-BE-SEEN").await;
    let proxy = AllowlistProxy::spawn(Allowlist::new(Vec::<String>::new()))
        .await
        .expect("proxy should start");

    let (_stream, head) = send_connect(proxy.addr(), &origin_addr.to_string())
        .await
        .expect("the proxy should still respond, just with a refusal");
    assert!(head.starts_with("HTTP/1.1 403"), "head: {head}");
}

#[tokio::test]
async fn plain_http_forward_proxying_is_also_allowlisted() {
    // Absolute-form (non-CONNECT) forwarding: the plain-HTTP path through
    // `self.client` (a bare `HttpConnector`, see `proxy.rs`), exercised
    // separately from the CONNECT-tunnel path above.
    let origin_addr = spawn_origin("PLAIN-HTTP-ORIGIN-BODY").await;
    let proxy = AllowlistProxy::spawn(Allowlist::new([origin_addr.ip().to_string()]))
        .await
        .expect("proxy should start");

    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    let request = format!(
        "GET http://{origin_addr}/ HTTP/1.1\r\nHost: {origin_addr}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("PLAIN-HTTP-ORIGIN-BODY"), "got: {body}");
}
