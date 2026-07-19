//! The crux tests for the network-proxy leg
//! (`docs/agent-approval-design.md`, "Sandbox architecture" / "Staging" leg
//! 4): a real `horizon-sandbox`-contained process, reaching the network
//! *only* through [`horizon_sandbox_proxy::UdsBridge`], can reach an
//! allowlisted host and cannot reach a different one -- and a plain,
//! unbridged direct connection attempt stays exactly as denied as it is
//! under `NetworkPolicy::Disabled` today.
//!
//! The sandboxed "process" is `uds_http_probe` (see `src/bin/`), a tiny
//! fixture that speaks CONNECT-then-plain-HTTP over the bind-mounted
//! socket by hand -- deliberately not `curl`/`reqwest`, neither of which
//! supports proxying over an arbitrary UNIX socket the way this bridge
//! needs (see that binary's own doc comment).
//!
//! Every spawned process/task here is bounded and cleaned up by test end:
//! the sandboxed child is watchdog-killed if it overruns a timeout, and
//! `AllowlistProxy`/`UdsBridge` abort their background tasks on `Drop`.

use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use horizon_sandbox::{NetworkPolicy, ReadableScope, SandboxPolicy, SandboxStdio};
use horizon_sandbox_proxy::{Allowlist, AllowlistProxy, UdsBridge};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_PATH: &str = env!("CARGO_BIN_EXE_uds_http_probe");

fn tempdir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "horizon-sandbox-proxy-test-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A one-shot HTTP origin bound to a specific loopback address (not
/// necessarily `127.0.0.1` -- distinct addresses in `127.0.0.0/8` stand in
/// for genuinely distinct hosts, since the allowlist matches by host, not
/// by port; see `run_containment_pair` below).
async fn spawn_origin(bind_addr: &str, marker: &'static str) -> SocketAddr {
    let listener = TcpListener::bind((bind_addr, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            marker.len(),
            marker
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    addr
}

/// Runs `uds_http_probe <bridge_socket> <target>` inside a real bwrap
/// sandbox whose *only* network path is `bridge_socket`
/// (`NetworkPolicy::Proxied`), watchdog-bounded, and returns its captured
/// stdout. Blocking (uses `horizon_sandbox::spawn`'s plain
/// `std::process::Child`), so callers on an async runtime should run it
/// via `spawn_blocking`.
fn run_probe_in_sandbox(bridge_socket: &Path, target: &str) -> String {
    let workdir = tempdir("probe-workdir");

    let policy = SandboxPolicy {
        writable_roots: vec![workdir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Proxied {
            bridge_socket: bridge_socket.to_path_buf(),
        },
    };
    let mut cmd = Command::new(PROBE_PATH);
    cmd.arg(bridge_socket).arg(target);

    let sandboxed =
        horizon_sandbox::spawn(cmd, &policy, SandboxStdio::piped_output()).expect("spawn probe");
    let mut child = sandboxed.child;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("stdout should be piped");

    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let done = done.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + TEST_TIMEOUT;
            while Instant::now() < deadline && !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            if !done.load(Ordering::SeqCst) {
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        })
    };

    let mut out = String::new();
    let _ = stdout.read_to_string(&mut out);
    let _ = child.wait();
    done.store(true, Ordering::SeqCst);
    let _ = watchdog.join();

    let _ = std::fs::remove_dir_all(workdir);
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandboxed_probe_reaches_the_allowlisted_host_through_the_bridge() {
    let allowed = spawn_origin("127.0.0.2", "ALLOWED-ORIGIN-MARKER").await;

    let proxy = AllowlistProxy::spawn(Allowlist::new(["127.0.0.2"]))
        .await
        .expect("proxy should start");
    let bridge_socket = tempdir("bridge-allowed").join("proxy.sock");
    let bridge = UdsBridge::spawn(bridge_socket.clone(), proxy.addr())
        .await
        .expect("bridge should start");

    let target = allowed.to_string();
    let out = tokio::task::spawn_blocking(move || run_probe_in_sandbox(&bridge_socket, &target))
        .await
        .unwrap();

    assert!(out.starts_with("PROBE-OK 200"), "probe output: {out}");
    assert!(
        out.contains("ALLOWED-ORIGIN-MARKER"),
        "expected the sandboxed probe to reach the real allowed origin, got: {out}"
    );

    drop(bridge);
    drop(proxy);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandboxed_probe_cannot_reach_a_different_host_through_the_same_bridge() {
    // Two distinct loopback hosts (127.0.0.0/8 all routes locally): the
    // proxy allowlists only the first. The second is a *real, listening*
    // origin -- so a successful reach would be unambiguous proof of a
    // containment failure, not just "nothing answered".
    let allowed = spawn_origin("127.0.0.2", "ALLOWED-ORIGIN-MARKER").await;
    let denied = spawn_origin("127.0.0.3", "DENIED-ORIGIN-MARKER-SHOULD-NEVER-APPEAR").await;

    let proxy = AllowlistProxy::spawn(Allowlist::new(["127.0.0.2"]))
        .await
        .expect("proxy should start");
    let bridge_socket = tempdir("bridge-denied").join("proxy.sock");
    let bridge = UdsBridge::spawn(bridge_socket.clone(), proxy.addr())
        .await
        .expect("bridge should start");

    let target = denied.to_string();
    let out = tokio::task::spawn_blocking(move || run_probe_in_sandbox(&bridge_socket, &target))
        .await
        .unwrap();

    assert!(out.starts_with("PROBE-DENIED 403"), "probe output: {out}");
    assert!(
        !out.contains("DENIED-ORIGIN-MARKER-SHOULD-NEVER-APPEAR"),
        "the sandboxed process must never actually reach the non-allowlisted host, got: {out}"
    );

    // The allowed host stays reachable through the very same bridge --
    // this isn't a bridge outage, it's the allowlist doing its job.
    let allowed_target = allowed.to_string();
    let allowed_socket = bridge.socket_path().to_path_buf();
    let allowed_out =
        tokio::task::spawn_blocking(move || run_probe_in_sandbox(&allowed_socket, &allowed_target))
            .await
            .unwrap();
    assert!(
        allowed_out.starts_with("PROBE-OK 200"),
        "probe output: {allowed_out}"
    );

    drop(bridge);
    drop(proxy);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_allowlist_denies_the_bridge_for_every_host() {
    let origin = spawn_origin("127.0.0.2", "SHOULD-NEVER-BE-SEEN").await;

    let proxy = AllowlistProxy::spawn(Allowlist::new(Vec::<String>::new()))
        .await
        .expect("proxy should start");
    let bridge_socket = tempdir("bridge-empty").join("proxy.sock");
    let bridge = UdsBridge::spawn(bridge_socket.clone(), proxy.addr())
        .await
        .expect("bridge should start");

    let target = origin.to_string();
    let out = tokio::task::spawn_blocking(move || run_probe_in_sandbox(&bridge_socket, &target))
        .await
        .unwrap();

    assert!(out.starts_with("PROBE-DENIED 403"), "probe output: {out}");

    drop(bridge);
    drop(proxy);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_egress_stays_fully_blocked_even_under_a_proxied_policy() {
    // No live bridge needed for this one: it only proves the seccomp cut
    // still applies when the policy is `Proxied` rather than `Disabled`
    // (mirroring `horizon-sandbox`'s own `network_off_fails_a_tcp_connect`).
    // `NetworkPolicy::Proxied` still requires the bridge-socket path to
    // exist (`bwrap::bind_ro`'s hard error on a missing path), so a plain
    // placeholder file stands in -- it's never actually dialed here.
    let workdir = tempdir("direct-egress-workdir");
    let bridge_socket = workdir.join("placeholder.sock");
    std::fs::File::create(&bridge_socket).unwrap();
    let log = workdir.join("stderr.log");

    let policy = SandboxPolicy {
        writable_roots: vec![workdir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Proxied { bridge_socket },
    };
    let mut cmd = Command::new("/bin/bash");
    cmd.arg("-c").arg(format!(
        "exec 2>{}; exec 3<>/dev/tcp/93.184.216.34/80",
        shell_quote(&log)
    ));
    cmd.env("LC_ALL", "C");

    let out = tokio::task::spawn_blocking(move || {
        let sandboxed = horizon_sandbox::spawn(cmd, &policy, SandboxStdio::inherit())
            .expect("spawn should succeed");
        let mut child = sandboxed.child;
        let status = child.wait().expect("wait should succeed");
        status.code().unwrap_or(-1)
    })
    .await
    .unwrap();

    let stderr = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .to_lowercase();
    assert_ne!(out, 0, "direct TCP connect must fail under Proxied too");
    assert!(
        stderr.contains("network") || stderr.contains("operation not permitted"),
        "expected a sandbox-denied-shaped error, got: {stderr:?}"
    );

    let _ = std::fs::remove_dir_all(workdir);
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
