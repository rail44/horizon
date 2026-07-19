//! Integration-level containment proof for the network-proxy leg of tier 1
//! (`docs/agent-approval-design.md`'s "Staging" leg 4a). Prior spikes
//! (`crates/horizon-sandbox-proxy/tests/containment.rs`) already proved the
//! proxy/bridge/sandbox layers hold in isolation, against raw
//! `horizon_sandbox::spawn`. These tests prove the same invariants through
//! the *actual* production call path a tier-1 auto-approved `bash` call
//! takes: `execute_agent_tool` -> `execute_tier1_bash` ->
//! `bash::spawn_sandboxed` -> `exec::run_sandboxed`, wired with a real
//! `AllowlistProxy` + `UdsBridge` via `ToolSessionState::with_bridge_socket`
//! exactly the way `horizon-sessiond`'s `session::run_session` wires the
//! daemon's own long-lived pair in.
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
//! those two; see that module's doc comment on the re-export).
//!
//! **Readiness, not single-shot** (2026-07-19 hardening): under the full
//! workspace's own concurrent nextest run, this test's `AllowlistProxy`/
//! `UdsBridge` tokio tasks (spawned but not yet polled) can be slow to get
//! their first CPU timeslice while dozens of *other* tests' own bwrap
//! sandboxes compete for it -- and a bare single sandboxed-probe-and-read
//! could then time out and report empty/ambiguous output even though
//! nothing is actually broken, exactly the failure mode confirmed in
//! `crates/horizon-sandbox-proxy/tests/containment.rs` under the same
//! full-workspace load (see that file's own module doc, which shares this
//! exact fix shape). [`wait_for_bridge_warm`] confirms the bridge+proxy
//! pipeline is actually serving before ever issuing the real (sandboxed)
//! bash call; [`expect_bash_probe_denied`] additionally retries the bash
//! call itself (bwrap sandbox creation can independently be slow) with a
//! bounded backoff, never accepting mere emptiness/timeout as proof of
//! denial -- only an explicit `PROBE-DENIED` counts, and reaching the
//! forbidden marker on *any* attempt is an immediate, non-retryable
//! failure.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use horizon_agent::config::AgentToolsConfig;
use horizon_agent::contract::{SessionId, ToolCallId, ToolCallRequest};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    execute_agent_tool, register_session_runtime, unregister_session_runtime, BashCompletion,
    Execution, HostTools, RecallContext, ToolSessionState,
};
use horizon_sandbox_proxy::{Allowlist, AllowlistProxy, UdsBridge};
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

fn temp_workspace(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "horizon-agent-tier1-network-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp workspace dir");
    dir.canonicalize().expect("canonicalize temp workspace dir")
}

/// Starts a real `AllowlistProxy` + `UdsBridge` pair (empty allowlist,
/// mirroring leg 4a's default posture -- there's no config surface yet) on
/// a dedicated tokio runtime kept alive for the caller. `run_sandboxed`
/// itself is fully synchronous and never touches tokio (see its own doc
/// comment), so this is purely test scaffolding standing in for what
/// `horizon-sessiond`'s `network::NetworkProxy` owns for the daemon's whole
/// lifetime.
fn start_test_proxy() -> (Runtime, AllowlistProxy, UdsBridge) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build test tokio runtime");
    let bridge_socket = std::env::temp_dir().join(format!(
        "horizon-agent-bridge-test-{}.sock",
        uuid::Uuid::new_v4()
    ));
    let (proxy, bridge) = runtime.block_on(async {
        let proxy = AllowlistProxy::spawn(Allowlist::new(Vec::<String>::new()))
            .await
            .expect("proxy should start");
        let bridge = UdsBridge::spawn(bridge_socket, proxy.addr())
            .await
            .expect("bridge should start");
        (proxy, bridge)
    });
    (runtime, proxy, bridge)
}

/// A real, listening loopback origin standing in for an arbitrary
/// non-allowlisted host -- a successful reach would be unambiguous proof of
/// a containment failure, not just "nothing answered" (mirrors
/// `horizon-sandbox-proxy`'s own `tests/containment.rs::spawn_origin`).
fn start_decoy_origin(runtime: &Runtime) -> SocketAddr {
    runtime.block_on(async {
        let listener = TcpListener::bind(("127.0.0.2", 0))
            .await
            .expect("bind decoy origin");
        let addr = listener.local_addr().expect("decoy origin local_addr");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = "DECOY-ORIGIN-MARKER-SHOULD-NEVER-APPEAR";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        addr
    })
}

/// A direct (non-sandboxed) CONNECT probe against the bridge, run from
/// this test process itself -- mirrors `bridge_probe`'s own wire protocol
/// exactly, just without going through bwrap. Used only as a readiness
/// check ([`wait_for_bridge_warm`]): returns an empty string on any
/// failure to connect/read rather than panicking, since "not ready yet" is
/// an expected, retriable outcome here, not an error.
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

/// Issues a fresh tier-1 sandboxed `bash` call (running `bridge_probe`
/// against `target` through the bridge) via `execute_agent_tool`,
/// retrying up to a bounded budget until it reports a definitive
/// `PROBE-DENIED`, tolerating transient emptiness/timeouts/`RetryWithout
/// Sandbox` classifications the same readiness race
/// [`wait_for_bridge_warm`] mostly heads off (plus bwrap sandbox creation
/// itself being independently slow under heavy concurrent load) --
/// **but** a genuine reach of `forbidden_marker` on *any* attempt is an
/// immediate, unconditional failure, and exhausting every attempt without
/// ever seeing an explicit `PROBE-DENIED` is *also* not accepted as proof
/// (that would be exactly the "timeout masquerading as a denial" failure
/// mode this exists to rule out) -- it panics instead of returning.
/// `session_id` must already be registered (`register_session_runtime`)
/// against `tool_state`/`bash_results_rx` before calling this; issuing
/// several bash calls against the same already-registered session is
/// ordinary production behavior, so no per-attempt re-registration is
/// needed.
fn expect_bash_probe_denied(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    bash_results_rx: &crossbeam_channel::Receiver<BashCompletion>,
    bridge_socket: &Path,
    target: &str,
    forbidden_marker: &str,
) -> String {
    const MAX_ATTEMPTS: u32 = 5;
    const PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut last = String::new();

    for attempt in 0..MAX_ATTEMPTS {
        let request = ToolCallRequest {
            call_id: ToolCallId(format!("call-denied-{attempt}")),
            tool_id: "bash".to_string(),
            input: json!({
                "command": format!(
                    "{BRIDGE_PROBE_PATH} {} {target}",
                    bridge_socket.display()
                )
            }),
        };
        let execution = execute_agent_tool(&StubHostTools, tool_state, session_id, &request);
        assert!(matches!(execution, Execution::Started(_)));

        let output = match bash_results_rx.recv_timeout(PER_ATTEMPT_TIMEOUT) {
            Ok(BashCompletion::Finished(result)) => result.output["output"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Ok(BashCompletion::RetryWithoutSandbox { reason, .. }) => {
                format!("(looked sandbox-denied, not a proxy denial: {reason})")
            }
            Err(_) => "(timed out waiting for the bash call to finish)".to_string(),
        };

        assert!(
            !output.contains(forbidden_marker),
            "the sandboxed bash call must never actually reach the denied origin: {output}"
        );
        if output.contains("PROBE-DENIED 403") {
            return output;
        }
        last = output;
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(BACKOFF);
        }
    }
    panic!(
        "bash-tool probe never got an explicit denial for {target} after {MAX_ATTEMPTS} \
         attempts (never PROBE-DENIED 403, and never reached the forbidden marker either -- \
         inconclusive, not proof of containment); last output: {last:?}"
    );
}

/// The crux containment proof for leg 4a: a tier-1 auto-approved, sandboxed
/// `bash` call wired to a real network-proxy bridge (empty allowlist) still
/// cannot reach an arbitrary host -- even a *real*, listening decoy, not
/// merely an unrouted address. This is the honest behavior a network-using
/// command sees today (no config surface / approval UX yet, `docs/
/// agent-approval-design.md`'s leg 4b): the proxy's own `403` (carrying
/// `x-horizon-sandbox-proxy-denial`), surfaced here via the probe's
/// `PROBE-DENIED` line.
#[test]
fn tier1_sandboxed_bash_with_empty_allowlist_cannot_reach_a_real_listening_decoy() {
    let (runtime, _proxy, bridge) = start_test_proxy();
    let decoy = start_decoy_origin(&runtime);

    let root = temp_workspace("tier1-bash-network-denied");
    let tool_state =
        ToolSessionState::for_root(root, AgentToolsConfig::default(), RecallContext::default())
            .with_isolated_worktree(true)
            .with_bridge_socket(Some(bridge.socket_path().to_path_buf()));
    let session_id = SessionId::new();
    let live_state = LiveState::with_disabled_persistence();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    wait_for_bridge_warm(bridge.socket_path());
    let target = decoy.to_string();
    let output = expect_bash_probe_denied(
        &tool_state,
        session_id,
        &bash_results_rx,
        bridge.socket_path(),
        &target,
        "DECOY-ORIGIN-MARKER-SHOULD-NEVER-APPEAR",
    );

    assert!(
        output.contains("PROBE-DENIED 403"),
        "expected the proxy to refuse the CONNECT for an empty allowlist: {output}"
    );

    unregister_session_runtime(session_id);
}

/// Mirrors `horizon-sandbox-proxy`'s own `direct_egress_stays_fully_blocked_
/// even_under_a_proxied_policy` test, but through the actual bash-tool call
/// path (`execute_agent_tool` -> tier-1 dispatch -> `spawn_sandboxed` ->
/// `run_sandboxed`) instead of raw `horizon_sandbox::spawn`: a direct,
/// unbridged TCP connect attempt from inside the sandbox must stay exactly
/// as blocked under `Proxied` as it is under `Disabled` today -- the
/// seccomp network-syscall cut is unconditional for both (see
/// `horizon_sandbox::linux::spawn`). Accepts either of the two ways
/// `run_sandboxed` can report that: the denial can look sandbox-shaped
/// enough that `horizon_sandbox::is_likely_sandbox_denied` classifies it as
/// a retry-without-sandbox prompt, or (if the exact wording doesn't match
/// its keyword list) a plain finished result with a non-zero exit and a
/// denial-shaped message. Either way, a genuine escape (exit 0, a real
/// response) is the only outcome that must never happen.
#[test]
fn tier1_sandboxed_bash_direct_egress_stays_blocked_under_proxied() {
    let (_runtime, _proxy, bridge) = start_test_proxy();

    let root = temp_workspace("tier1-bash-network-direct-egress");
    let tool_state =
        ToolSessionState::for_root(root, AgentToolsConfig::default(), RecallContext::default())
            .with_isolated_worktree(true)
            .with_bridge_socket(Some(bridge.socket_path().to_path_buf()));
    let session_id = SessionId::new();
    let live_state = LiveState::with_disabled_persistence();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

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
    // workspace's own concurrent test run, bwrap sandbox creation can be
    // slow purely from CPU contention with everyone else's sandboxes, and
    // this doesn't touch the bridge/proxy readiness race at all -- it's
    // just extra margin against unrelated scheduling delay. Still an
    // honest failure (`.expect`, not a swallowed timeout) if genuinely
    // wedged.
    let completion = bash_results_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the sandboxed bash call should finish");

    // Landlock denies the connect with EACCES ("permission denied"); an
    // older-ABI seccomp fallback would deny `socket(2)` with EPERM
    // ("operation not permitted"); a kernel where neither layer engaged
    // would instead see a plain "network is unreachable" at the routing
    // step.
    match completion {
        BashCompletion::RetryWithoutSandbox { reason, .. } => {
            let lower = reason.to_lowercase();
            assert!(
                lower.contains("network")
                    || lower.contains("operation not permitted")
                    || lower.contains("permission denied"),
                "expected a sandbox-denial-shaped reason: {reason}"
            );
        }
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
    }

    unregister_session_runtime(session_id);
}
