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
//!
//! **Readiness, not single-shot** (2026-07-19 hardening): under the full
//! workspace's own concurrent nextest run -- dozens of *other* tests
//! spawning their own bwrap sandboxes for CPU at the same time -- a bare
//! single spawn-probe-and-read could observe empty output even though
//! nothing is actually broken: this process's own `AllowlistProxy`/
//! `UdsBridge` tokio tasks (spawned but not yet polled) or the sandboxed
//! probe's own bwrap setup can simply be slow to get their first CPU
//! timeslice under that contention, and the probe's `TEST_TIMEOUT`
//! watchdog then kills it before it prints anything. [`wait_for_bridge_warm`]
//! confirms the bridge+proxy pipeline is actually serving (a real HTTP-
//! shaped reply, allow or deny, to a throwaway target) before ever paying
//! for the comparatively expensive sandboxed run; [`expect_probe_reaches`]/
//! [`expect_probe_denied`] additionally retry the sandboxed probe itself
//! (bwrap setup can independently be slow) with a bounded backoff. The DENY
//! side never accepts silence as proof: only an explicit `PROBE-DENIED`
//! counts, and a reach of the forbidden marker on *any* attempt is an
//! immediate, non-retryable failure -- see those functions' own doc
//! comments.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
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

/// A direct (non-sandboxed) CONNECT probe against the bridge, run from
/// this test process itself -- mirrors `uds_http_probe`'s own wire
/// protocol exactly, just without going through bwrap. Used only as a
/// readiness check ([`wait_for_bridge_warm`]): returns an empty string on
/// any failure to connect/read rather than panicking, since "not ready
/// yet" is an expected, retriable outcome here, not an error.
fn direct_probe(bridge_socket: &Path, target: &str) -> String {
    let Ok(mut stream) = UnixStream::connect(bridge_socket) else {
        return String::new();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let connect_req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    if stream.write_all(connect_req.as_bytes()).is_err() {
        return String::new();
    }
    let mut buf = [0u8; 4096];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buf[..n]).to_string(),
        _ => String::new(),
    }
}

/// Blocks (briefly, bounded) until the bridge + proxy pipeline is actually
/// serving requests -- confirmed by an HTTP-shaped CONNECT reply to an
/// arbitrary throwaway target (`127.0.0.1:1`; nothing needs to be
/// listening there -- `AllowlistHandler::handle_request` rejects an
/// unlisted host before ever dialing it, so either a `200` or a `403`
/// equally proves the handler code actually ran). This closes the
/// readiness race described in the module doc: confirming the pipeline is
/// warm *before* ever paying for a comparatively expensive sandboxed probe
/// run means that run's own result reflects the allowlist decision, not
/// whether this process's tokio tasks had been scheduled yet.
fn wait_for_bridge_warm(bridge_socket: &Path) {
    const MAX_ATTEMPTS: u32 = 100;
    const BACKOFF: Duration = Duration::from_millis(50);
    for attempt in 0..MAX_ATTEMPTS {
        if direct_probe(bridge_socket, "127.0.0.1:1").starts_with("HTTP/") {
            return;
        }
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "bridge/proxy pipeline at {} never became ready within {:?}",
        bridge_socket.display(),
        BACKOFF * MAX_ATTEMPTS
    );
}

/// Repeatedly runs the sandboxed probe against `target` until it reports a
/// definitive `PROBE-OK` containing `expected_marker`, or the attempt
/// budget is exhausted. Tolerates transient emptiness/`PROBE-ERROR` (bwrap
/// sandbox creation itself can independently be slow under heavy
/// concurrent load, even after [`wait_for_bridge_warm`] has confirmed the
/// bridge/proxy side is ready) by retrying with a backoff -- a real
/// containment *success* requires actually reaching the origin, so this
/// never accepts a denial as good enough.
fn expect_probe_reaches(bridge_socket: &Path, target: &str, expected_marker: &str) -> String {
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut last = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        let out = run_probe_in_sandbox(bridge_socket, target);
        if out.starts_with("PROBE-OK") && out.contains(expected_marker) {
            return out;
        }
        last = out;
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "probe never reached {target} with marker {expected_marker:?} after {MAX_ATTEMPTS} \
         attempts; last output: {last:?}"
    );
}

/// Repeatedly runs the sandboxed probe against `target` until it reports a
/// definitive `PROBE-DENIED`, tolerating transient emptiness the same way
/// [`expect_probe_reaches`] does -- but a genuine reach of
/// `forbidden_marker` is an immediate, unconditional failure on *any*
/// attempt, retried or not: a real containment breach must never be
/// masked by "well, a later attempt came back denied". Mere emptiness or
/// exhausting every attempt without ever seeing an explicit `PROBE-DENIED`
/// is *also* not accepted as proof -- that would be exactly the "timeout
/// masquerading as a denial" failure mode this exists to rule out, so it
/// panics (fails the test) instead of returning.
fn expect_probe_denied(bridge_socket: &Path, target: &str, forbidden_marker: &str) -> String {
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut last = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        let out = run_probe_in_sandbox(bridge_socket, target);
        assert!(
            !out.contains(forbidden_marker),
            "the sandboxed probe must never actually reach the denied origin: {out}"
        );
        if out.starts_with("PROBE-DENIED") {
            return out;
        }
        last = out;
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "probe never got an explicit denial for {target} after {MAX_ATTEMPTS} attempts (never \
         PROBE-DENIED, and never reached the forbidden marker either -- inconclusive, not proof \
         of containment); last output: {last:?}"
    );
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
    let out = tokio::task::spawn_blocking(move || {
        wait_for_bridge_warm(&bridge_socket);
        expect_probe_reaches(&bridge_socket, &target, "ALLOWED-ORIGIN-MARKER")
    })
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
    let out = tokio::task::spawn_blocking(move || {
        wait_for_bridge_warm(&bridge_socket);
        expect_probe_denied(
            &bridge_socket,
            &target,
            "DENIED-ORIGIN-MARKER-SHOULD-NEVER-APPEAR",
        )
    })
    .await
    .unwrap();

    assert!(out.starts_with("PROBE-DENIED 403"), "probe output: {out}");

    // The allowed host stays reachable through the very same bridge --
    // this isn't a bridge outage, it's the allowlist doing its job. No
    // separate `wait_for_bridge_warm` needed here: the denied probe above
    // already proved the pipeline is warm.
    let allowed_target = allowed.to_string();
    let allowed_socket = bridge.socket_path().to_path_buf();
    let allowed_out = tokio::task::spawn_blocking(move || {
        expect_probe_reaches(&allowed_socket, &allowed_target, "ALLOWED-ORIGIN-MARKER")
    })
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
    let out = tokio::task::spawn_blocking(move || {
        wait_for_bridge_warm(&bridge_socket);
        expect_probe_denied(&bridge_socket, &target, "SHOULD-NEVER-BE-SEEN")
    })
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
