//! Integration-level containment proof for the network-proxy leg of tier 1
//! (`docs/agent-approval-design.md`'s "Staging" legs 4a/4b). Prior spikes
//! (`crates/horizon-sandbox-proxy/tests/containment.rs`) already proved the
//! proxy/bridge/sandbox layers hold in isolation, against raw
//! `horizon_sandbox::spawn`. These tests prove the same invariants -- plus
//! leg 4b's per-session allowlist mutation and exit-code-independent denial
//! attribution -- through the *actual* production call path a tier-1
//! auto-approved `bash` call takes: `execute_agent_tool` ->
//! `execute_tier1_bash` -> `bash::spawn_sandboxed` -> `exec::run_sandboxed`,
//! wired with a real, per-session `tools::network::SessionNetworkProxy`
//! exactly the way `horizon-sessiond`'s `session::run_session` wires one in
//! for an isolated, sandbox-available session.
//!
//! This lives under `tests/` (an integration test, a separate compilation
//! unit linked against `horizon_agent` as an external crate) rather than as
//! a lib unit test, specifically so `env!("CARGO_BIN_EXE_bridge_probe")`
//! resolves: Cargo only bakes `CARGO_BIN_EXE_<name>` into integration-test/
//! bench/example compilation units, never a crate's own lib unit tests, and
//! only guarantees the named `[[bin]]` target is actually built *for those
//! same compilation kinds* -- a lib unit test has no such guarantee. An
//! earlier version of these two tests lived in `src/tools/tests.rs` with a
//! runtime-resolved fallback path; that passed locally (the binary happened
//! to already exist from an earlier build) but failed deterministically on
//! a clean `cargo clean -p horizon-agent && cargo nextest run --workspace`,
//! since nothing guarantees `bridge_probe` is built before lib unit tests
//! run. Being a real integration test here fixes both problems at once: no
//! path-guessing, and Cargo builds `bridge_probe` before this file even
//! compiles.
//!
//! Driving `execute_agent_tool`/`Execution` from outside the crate needs
//! them re-exported `pub` from `horizon_agent::tools` (narrowly -- just
//! those two, plus `SessionNetworkProxy` as of leg 4b; see that module's
//! doc comment on each re-export).
//!
//! **`bridge_probe` conveniently always exits `0`**, allow or deny (see its
//! own doc comment: it prints a `PROBE-OK`/`PROBE-DENIED` status line and
//! returns normally either way) -- exactly the shape backlog 59 flags as
//! unsafe to key denial detection on (`curl ... | head` exits `0` even
//! though `curl` itself never reached the network). `domain_denial_*`
//! below leans on this directly: a domain denial must be detected and
//! surfaced regardless of the sandboxed process's own successful exit.
//!
//! **Readiness, not single-shot** (2026-07-19 hardening, carried into leg
//! 4b's new tests below): under the full workspace's own concurrent
//! nextest run -- dozens of *other* tests spawning their own sandboxes for
//! CPU at the same time -- a bare single spawn-probe-and-read could observe
//! empty output even though nothing is actually broken; this process's own
//! `SessionNetworkProxy` tokio tasks (spawned but not yet polled) or the
//! sandboxed probe's own sandbox setup can simply be slow to get their
//! first CPU timeslice under that contention, and a per-attempt timeout
//! would then fire before it ever prints anything. [`wait_for_bridge_warm`]
//! confirms the bridge+proxy pipeline is actually serving (a real
//! HTTP-shaped reply, allow or deny, to a throwaway target) before ever
//! paying for a comparatively expensive sandboxed run; the `expect_*`
//! helpers below additionally retry the sandboxed probe itself with a
//! bounded backoff. A genuine reach of a forbidden marker on *any* attempt
//! is an immediate, non-retryable failure -- never masked by "well, a later
//! attempt came back denied".

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::AgentToolsConfig;
use horizon_agent::contract::{SessionId, ToolCallId, ToolCallRequest, ToolCallResult};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    execute_agent_tool, register_session_runtime, unregister_session_runtime, BashCompletion,
    Execution, HostTools, RecallContext, SessionNetworkProxy, ToolSessionState,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

/// This crate's own `bridge_probe` fixture (`src/bin/bridge_probe.rs`): a
/// minimal, dependency-free HTTP client that speaks a CONNECT request by
/// hand over a UNIX-domain-socket bridge, since no standard HTTP client
/// (`curl`/`reqwest`) supports proxying over an arbitrary bind-mounted
/// socket the way `horizon_sandbox_proxy::UdsBridge` needs. Resolved at
/// compile time -- see this file's own module doc for why that's only
/// possible here, not from a lib unit test.
const BRIDGE_PROBE_PATH: &str = env!("CARGO_BIN_EXE_bridge_probe");

/// A `HostTools` stub: neither test here exercises a host-owned auto-allow
/// tool (`workspace.snapshot`), only `bash`, so this always falls through.
struct StubHostTools;

impl HostTools for StubHostTools {
    fn execute_auto(
        &self,
        _tool_id: &str,
        _input: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        None
    }
}

fn temp_workspace(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "horizon-agent-tier1-network-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp workspace dir");
    dir.canonicalize().expect("canonicalize temp workspace dir")
}

/// A throwaway tokio runtime for standing up decoy origins only -- entirely
/// separate from [`SessionNetworkProxy`]'s own internal shared runtime
/// (`tools::network`'s module doc), which this test never touches directly.
fn test_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build test tokio runtime")
}

/// Wires a fresh, isolated `ToolSessionState` to a fresh, real
/// `SessionNetworkProxy` -- the same production shape `horizon-sessiond`'s
/// `session::run_session` wires in for an isolated, sandbox-available
/// session, just constructed directly here instead of gated behind
/// `isolated && horizon_sandbox::is_available()`. Returns the `ToolSessionState`
/// plus the `Arc<SessionNetworkProxy>` handle a test needs to call
/// `allow_domain`/`bridge_socket` directly (simulating what `tools::
/// approval::resolve_domain_denial_retry` does on a real user approval).
fn isolated_session_with_network(label: &str) -> (ToolSessionState, Arc<SessionNetworkProxy>) {
    let network =
        Arc::new(SessionNetworkProxy::start().expect("session network proxy should start"));
    let root = temp_workspace(label);
    let tool_state =
        ToolSessionState::for_root(root, AgentToolsConfig::default(), RecallContext::default())
            .with_isolated_worktree(true)
            .with_network_proxy(Some(network.clone()));
    (tool_state, network)
}

/// A real, listening loopback origin standing in for an arbitrary
/// non-allowlisted host -- a successful reach would be unambiguous proof of
/// a containment failure, not just "nothing answered" (mirrors
/// `horizon-sandbox-proxy`'s own `tests/containment.rs::spawn_origin`).
fn start_decoy_origin(runtime: &Runtime, bind_addr: &str, marker: &'static str) -> SocketAddr {
    runtime.block_on(async {
        let listener = TcpListener::bind((bind_addr, 0))
            .await
            .expect("bind decoy origin");
        let addr = listener.local_addr().expect("decoy origin local_addr");
        tokio::spawn(async move {
            loop {
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
            }
        });
        addr
    })
}

/// A direct (non-sandboxed) CONNECT probe against the bridge, run from
/// this test process itself -- mirrors `bridge_probe`'s own wire protocol
/// exactly, just without going through the sandbox. Used only as a
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
/// listening there -- the allowlist rejects an unlisted host before ever
/// dialing it, so either a `200` or a `403` equally proves the handler
/// code actually ran). See this file's own module doc for why this
/// matters under the full-workspace concurrent test run.
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

/// Issues one fresh tier-1 sandboxed `bash` call running `bridge_probe`
/// against `target` through `bridge_socket`, and waits (bounded) for its
/// `BashCompletion`.
fn run_bridge_probe(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    bash_results_rx: &crossbeam_channel::Receiver<BashCompletion>,
    bridge_socket: &Path,
    target: &str,
    per_attempt_timeout: Duration,
) -> Option<BashCompletion> {
    let request = ToolCallRequest {
        call_id: ToolCallId(format!("call-{}", uuid::Uuid::new_v4())),
        tool_id: "bash".to_string(),
        input: json!({
            "command": format!("{BRIDGE_PROBE_PATH} {} {target}", bridge_socket.display())
        }),
    };
    let execution = execute_agent_tool(&StubHostTools, tool_state, session_id, &request);
    assert!(matches!(execution, Execution::Started(_)));
    bash_results_rx.recv_timeout(per_attempt_timeout).ok()
}

/// Repeatedly runs [`run_bridge_probe`] against `target` until it reports a
/// definitive [`BashCompletion::DomainDenied`] naming `target`'s host,
/// tolerating transient emptiness/timeouts (bounded backoff) -- but a genuine reach of
/// `forbidden_marker` (a `Finished` *or* `DomainDenied` result whose output
/// contains it) is an immediate, unconditional failure on *any* attempt,
/// and exhausting every attempt without ever seeing an explicit
/// `DomainDenied` is also not accepted as proof.
fn expect_domain_denied(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    bash_results_rx: &crossbeam_channel::Receiver<BashCompletion>,
    bridge_socket: &Path,
    target: &str,
    forbidden_marker: &str,
) -> ToolCallResult {
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut last_note = String::new();

    for attempt in 0..MAX_ATTEMPTS {
        match run_bridge_probe(
            tool_state,
            session_id,
            bash_results_rx,
            bridge_socket,
            target,
            Duration::from_secs(15),
        ) {
            Some(BashCompletion::DomainDenied {
                domains, result, ..
            }) => {
                let output_text = result.output["output"].as_str().unwrap_or_default();
                assert!(
                    !output_text.contains(forbidden_marker),
                    "the sandboxed bash call must never actually reach the denied origin: \
                     {output_text}"
                );
                assert!(
                    domains
                        .iter()
                        .any(|domain| target.starts_with(domain.as_str())),
                    "expected the denied-domain list to name the target host {target}: \
                     {domains:?}"
                );
                return result;
            }
            Some(BashCompletion::Finished(result)) => {
                let output_text = result.output["output"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !output_text.contains(forbidden_marker),
                    "the sandboxed bash call must never actually reach the denied origin: \
                     {output_text}"
                );
                last_note = format!("finished (not domain-denied): {:?}", result.output);
            }
            Some(BashCompletion::FilesystemDenied { denials, .. }) => {
                last_note = format!("unexpected filesystem denial: {denials:?}");
            }
            None => last_note = "timed out waiting for the bash call to finish".to_string(),
        }
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "bash-tool probe never got an explicit domain denial for {target} after \
         {MAX_ATTEMPTS} attempts; last: {last_note}"
    );
}

/// The reachability-side counterpart to [`expect_domain_denied`]: retries
/// until a `Finished` result reports a clean `PROBE-OK` containing
/// `expected_marker`. Tolerates transient `DomainDenied`/timeouts (bwrap-
/// /nono-sandbox setup and the proxy's own tasks can independently be slow
/// under load), since a real containment *success* requires actually
/// reaching the origin -- this never accepts a denial as good enough.
fn expect_reaches(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    bash_results_rx: &crossbeam_channel::Receiver<BashCompletion>,
    bridge_socket: &Path,
    target: &str,
    expected_marker: &str,
) -> ToolCallResult {
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut last_note = String::new();

    for attempt in 0..MAX_ATTEMPTS {
        match run_bridge_probe(
            tool_state,
            session_id,
            bash_results_rx,
            bridge_socket,
            target,
            Duration::from_secs(15),
        ) {
            Some(BashCompletion::Finished(result)) => {
                let output_text = result.output["output"].as_str().unwrap_or_default();
                if output_text.starts_with("PROBE-OK") && output_text.contains(expected_marker) {
                    return result;
                }
                last_note = format!("finished but not a clean reach: {output_text}");
            }
            Some(BashCompletion::DomainDenied { domains, .. }) => {
                last_note = format!("still domain-denied: {domains:?}");
            }
            Some(BashCompletion::FilesystemDenied { denials, .. }) => {
                last_note = format!("unexpected filesystem denial: {denials:?}");
            }
            None => last_note = "timed out waiting for the bash call to finish".to_string(),
        }
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "bash-tool probe never reached {target} with marker {expected_marker:?} after \
         {MAX_ATTEMPTS} attempts; last: {last_note}"
    );
}

/// The crux containment proof for leg 4b's denial detection: a tier-1
/// auto-approved, sandboxed `bash` call reaching a non-allowlisted (but
/// really listening) decoy is refused, and the refusal is attributed by
/// *domain name* in the tool result's own output JSON -- `is_error`,
/// `denied_domains` -- even though `bridge_probe` itself always exits `0`
/// (see this file's module doc). This is exactly backlog 59's concern: a
/// denial must never be missed because the wrapped shell pipeline happened
/// to exit cleanly.
#[test]
fn tier1_sandboxed_bash_domain_denial_is_detected_independent_of_exit_code() {
    let runtime = test_runtime();
    let decoy = start_decoy_origin(
        &runtime,
        "127.0.0.2",
        "DECOY-ORIGIN-MARKER-SHOULD-NEVER-APPEAR",
    );

    let (tool_state, network) = isolated_session_with_network("domain-denied-exit-zero");
    let session_id = SessionId::new();
    let live_state = LiveState::with_disabled_persistence();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    wait_for_bridge_warm(network.bridge_socket());
    let target = decoy.to_string();
    let result = expect_domain_denied(
        &tool_state,
        session_id,
        &bash_results_rx,
        network.bridge_socket(),
        &target,
        "DECOY-ORIGIN-MARKER-SHOULD-NEVER-APPEAR",
    );

    assert!(
        result.is_error,
        "a domain-denied result must be is_error: {result:?}"
    );
    assert_eq!(
        result.output["exit_code"], 0,
        "bridge_probe always exits 0, allow or deny -- the point of this test: {:?}",
        result.output
    );
    let denied = result.output["denied_domains"]
        .as_array()
        .expect("denied_domains should be a JSON array");
    assert!(
        denied
            .iter()
            .any(|value| target.starts_with(value.as_str().unwrap_or_default())),
        "expected denied_domains to name the target host {target}: {:?}",
        result.output
    );

    unregister_session_runtime(session_id);
}

/// Leg 4b's per-session attribution + no-cross-session-leak proof: session
/// A's approval of a domain lets A reach it, but a *different* session B --
/// its own separate `SessionNetworkProxy`, its own separate allowlist --
/// still cannot, through the exact same real decoy origin.
#[test]
fn domain_approval_is_scoped_to_the_approving_session_no_leak() {
    let runtime = test_runtime();
    let decoy = start_decoy_origin(&runtime, "127.0.0.3", "SHARED-DECOY-MARKER");
    let target = decoy.to_string();

    let (tool_state_a, network_a) = isolated_session_with_network("session-a-no-leak");
    let session_a = SessionId::new();
    let live_state_a = LiveState::with_disabled_persistence();
    let (bash_results_tx_a, bash_results_rx_a) = crossbeam_channel::unbounded();
    register_session_runtime(
        session_a,
        tool_state_a.clone(),
        live_state_a,
        bash_results_tx_a,
    );

    let (tool_state_b, network_b) = isolated_session_with_network("session-b-no-leak");
    let session_b = SessionId::new();
    let live_state_b = LiveState::with_disabled_persistence();
    let (bash_results_tx_b, bash_results_rx_b) = crossbeam_channel::unbounded();
    register_session_runtime(
        session_b,
        tool_state_b.clone(),
        live_state_b,
        bash_results_tx_b,
    );

    wait_for_bridge_warm(network_a.bridge_socket());
    wait_for_bridge_warm(network_b.bridge_socket());

    // Before approval: both sessions are denied.
    expect_domain_denied(
        &tool_state_a,
        session_a,
        &bash_results_rx_a,
        network_a.bridge_socket(),
        &target,
        "SHARED-DECOY-MARKER",
    );

    // The approval: exactly what `tools::approval::resolve_domain_denial_retry`
    // does on the user's Approve -- add the denied host to *this session's*
    // allowlist only.
    let approved_host = decoy.ip().to_string();
    network_a.allow_domain(approved_host);

    // A's retry now reaches the real origin.
    let reached = expect_reaches(
        &tool_state_a,
        session_a,
        &bash_results_rx_a,
        network_a.bridge_socket(),
        &target,
        "SHARED-DECOY-MARKER",
    );
    assert!(
        !reached.is_error,
        "a successful reach must not be is_error: {reached:?}"
    );

    // B never approved anything -- still denied through its own, entirely
    // separate proxy/allowlist, proving zero cross-session leakage.
    expect_domain_denied(
        &tool_state_b,
        session_b,
        &bash_results_rx_b,
        network_b.bridge_socket(),
        &target,
        "SHARED-DECOY-MARKER",
    );

    unregister_session_runtime(session_a);
    unregister_session_runtime(session_b);
}

/// Approving one domain must not widen access to a *different* one: after
/// session A approves and reaches origin X, a fresh call against a
/// different origin Y stays refused for that same session.
#[test]
fn approving_one_domain_does_not_unlock_a_different_domain() {
    let runtime = test_runtime();
    let origin_x = start_decoy_origin(&runtime, "127.0.0.4", "ORIGIN-X-MARKER");
    let origin_y = start_decoy_origin(&runtime, "127.0.0.5", "ORIGIN-Y-MARKER-SHOULD-NEVER-APPEAR");

    let (tool_state, network) = isolated_session_with_network("no-cross-domain-unlock");
    let session_id = SessionId::new();
    let live_state = LiveState::with_disabled_persistence();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    wait_for_bridge_warm(network.bridge_socket());

    let target_x = origin_x.to_string();
    expect_domain_denied(
        &tool_state,
        session_id,
        &bash_results_rx,
        network.bridge_socket(),
        &target_x,
        "ORIGIN-X-MARKER",
    );

    network.allow_domain(origin_x.ip().to_string());

    expect_reaches(
        &tool_state,
        session_id,
        &bash_results_rx,
        network.bridge_socket(),
        &target_x,
        "ORIGIN-X-MARKER",
    );

    // Y was never approved -- still denied, even from the same session that
    // just got X approved.
    let target_y = origin_y.to_string();
    expect_domain_denied(
        &tool_state,
        session_id,
        &bash_results_rx,
        network.bridge_socket(),
        &target_y,
        "ORIGIN-Y-MARKER-SHOULD-NEVER-APPEAR",
    );

    unregister_session_runtime(session_id);
}

/// Mirrors `horizon-sandbox-proxy`'s own `direct_egress_stays_fully_blocked_
/// even_under_a_proxied_policy` test, but through the actual bash-tool call
/// path (`execute_agent_tool` -> tier-1 dispatch -> `spawn_sandboxed` ->
/// `run_sandboxed`) instead of raw `horizon_sandbox::spawn`: a direct,
/// unbridged TCP connect attempt from inside the sandbox must stay exactly
/// as blocked under `Proxied` as it is under `Disabled` today -- the
/// seccomp/Landlock network cut is unconditional for both. A direct-connect
/// refusal is a plain finished result with a non-zero exit and a
/// denial-shaped message, never a `DomainDenied` --
/// direct egress never reaches the proxy at all, so nothing is ever
/// recorded to drain. Either way, a genuine escape (exit 0, a real
/// response) is the only outcome that must never happen.
#[test]
fn tier1_sandboxed_bash_direct_egress_stays_blocked_under_proxied() {
    let (tool_state, network) = isolated_session_with_network("direct-egress");
    let session_id = SessionId::new();
    let live_state = LiveState::with_disabled_persistence();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);
    // Keep the proxy itself alive for the whole test even though this test
    // never uses its bridge socket -- dropping it early would tear down a
    // still-live tokio task on the shared runtime for no reason.
    let _network = network;

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        // A direct connect attempt, bypassing the bridge entirely -- exactly
        // what the network layer promises stays cut off even though *a*
        // UNIX-domain-socket egress now exists for this session.
        input: json!({ "command": "exec 3<>/dev/tcp/93.184.216.34/80" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    assert!(matches!(execution, Execution::Started(_)));

    // Generous (not the tight 10s an earlier version used): under the full
    // workspace's own concurrent test run, sandbox creation can be slow
    // purely from CPU contention with everyone else's sandboxes, and this
    // doesn't touch the bridge/proxy readiness race at all -- it's just
    // extra margin against unrelated scheduling delay. Still an honest
    // failure (`.expect`, not a swallowed timeout) if genuinely wedged.
    let completion = bash_results_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the sandboxed bash call should finish");

    // Landlock denies the connect with EACCES ("permission denied"); an
    // older-ABI seccomp fallback would deny `socket(2)` with EPERM
    // ("operation not permitted"); a kernel where neither layer engaged
    // would instead see a plain "network is unreachable" at the routing
    // step.
    match completion {
        BashCompletion::Finished(result) => {
            assert_ne!(
                result.output["exit_code"], 0,
                "a direct TCP connect must fail under Proxied too: {:?}",
                result.output
            );
            let output = result.output["output"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase();
            assert!(
                output.contains("network")
                    || output.contains("operation not permitted")
                    || output.contains("permission denied"),
                "expected a sandbox-denied-shaped error, got: {:?}",
                result.output
            );
        }
        BashCompletion::DomainDenied { domains, .. } => {
            panic!(
                "direct egress must never reach the proxy at all, so nothing should ever be \
                 recorded to drain: {domains:?}"
            );
        }
        BashCompletion::FilesystemDenied { denials, .. } => {
            panic!("direct network probe unexpectedly hit a filesystem denial: {denials:?}");
        }
    }

    unregister_session_runtime(session_id);
}
