//! End-to-end proof for tier-1's standard-CLI -> per-session TCP proxy path.
//! Proxy environment variables provide compatibility; the sandbox helper is
//! the security boundary when a client ignores or overrides them.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::AgentToolsConfig;
use horizon_agent::contract::{
    ApprovalKind, ApprovalRequest, Event, ProviderEvent, SessionId, ToolCallId, ToolCallRequest,
    ToolCallResult,
};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    execute_agent_tool, register_session_runtime, resolve_approval, unregister_session_runtime,
    ApprovalDecision, ApprovalOutcome, BashCompletion, Execution, HostTools, RecallContext,
    SessionNetworkProxy, ToolSessionState,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

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

fn test_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build test tokio runtime")
}

fn isolated_session_with_network(label: &str) -> (ToolSessionState, Arc<SessionNetworkProxy>) {
    let network =
        Arc::new(SessionNetworkProxy::start().expect("session network proxy should start"));
    let tool_state = ToolSessionState::for_root(
        temp_workspace(label),
        AgentToolsConfig::default(),
        RecallContext::default(),
    )
    .with_isolated_worktree(true)
    .with_network_proxy(Some(network.clone()));
    (tool_state, network)
}

fn start_origin(runtime: &Runtime, bind_addr: &str, marker: &'static str) -> SocketAddr {
    runtime.block_on(async {
        let listener = TcpListener::bind((bind_addr, 0))
            .await
            .expect("bind origin");
        let addr = listener.local_addr().expect("origin local_addr");
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

fn direct_proxy_probe(proxy: SocketAddr, target: &str) -> String {
    let Ok(mut stream) = TcpStream::connect_timeout(&proxy, Duration::from_secs(2)) else {
        return String::new();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return String::new();
    }
    let mut buf = [0u8; 4096];
    match stream.read(&mut buf) {
        Ok(count) if count > 0 => String::from_utf8_lossy(&buf[..count]).into_owned(),
        _ => String::new(),
    }
}

fn wait_for_proxy(proxy: SocketAddr) {
    for _ in 0..100 {
        if direct_proxy_probe(proxy, "127.0.0.1:1").starts_with("HTTP/") {
            // Readiness probes are intentionally denied. Remove their log
            // entries so they cannot be attributed to the first tool call.
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("proxy at {proxy} never became ready");
}

fn run_curl(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    results: &crossbeam_channel::Receiver<BashCompletion>,
    target: &str,
) -> BashCompletion {
    let request = curl_request(target);
    start_request(tool_state, session_id, &request);
    results
        .recv_timeout(Duration::from_secs(30))
        .expect("sandboxed curl should finish")
}

fn curl_request(target: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId(format!("call-{}", uuid::Uuid::new_v4())),
        tool_id: "bash".to_string(),
        // No curl proxy option: production-injected standard environment is
        // the compatibility mechanism. `|| true` proves denial attribution
        // does not depend on the shell's exit status.
        input: json!({
            "command": format!("curl --max-time 5 -sS http://{target} || true")
        })
        .into(),
    }
}

fn start_request(tool_state: &ToolSessionState, session_id: SessionId, request: &ToolCallRequest) {
    assert!(matches!(
        execute_agent_tool(&StubHostTools, tool_state, session_id, request),
        Execution::Started(_)
    ));
}

fn expect_domain_denied(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    results: &crossbeam_channel::Receiver<BashCompletion>,
    target: &str,
    forbidden_marker: &str,
) -> ToolCallResult {
    match run_curl(tool_state, session_id, results, target) {
        BashCompletion::DomainDenied {
            domains, result, ..
        } => {
            let output = result.output["output"].as_str().unwrap_or_default();
            assert!(!output.contains(forbidden_marker));
            let expected_host = target.split(':').next().unwrap();
            assert!(domains.iter().any(|domain| domain == expected_host));
            result
        }
        BashCompletion::Finished(result) => {
            panic!(
                "expected named proxy denial, got finished: {:?}",
                result.output
            )
        }
        BashCompletion::FilesystemDenied { denials, .. } => {
            panic!("unexpected filesystem denial: {denials:?}")
        }
        BashCompletion::DomainGrantRequired { domains, .. } => {
            panic!("bash must not produce a host-side domain grant: {domains:?}")
        }
    }
}

#[test]
fn standard_curl_denial_is_named_even_when_shell_exits_zero() {
    let runtime = test_runtime();
    let origin = start_origin(&runtime, "127.0.0.2", "MUST-NOT-REACH");
    let (tool_state, network) = isolated_session_with_network("domain-denied-exit-zero");
    let session_id = SessionId::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        LiveState::with_disabled_persistence(),
        tx,
    );
    wait_for_proxy(network.proxy_addr());
    network.drain_denied_hosts();

    let result = expect_domain_denied(
        &tool_state,
        session_id,
        &rx,
        &origin.to_string(),
        "MUST-NOT-REACH",
    );
    assert!(result.is_error);
    assert_eq!(result.output["exit_code"], 0);
    unregister_session_runtime(session_id);
}

#[test]
fn domain_approval_is_session_scoped_and_host_narrow() {
    let runtime = test_runtime();
    let origin_x = start_origin(&runtime, "127.0.0.3", "ORIGIN-X");
    let origin_y = start_origin(&runtime, "127.0.0.4", "ORIGIN-Y");
    let target_x = origin_x.to_string();
    let target_y = origin_y.to_string();

    let (state_a, network_a) = isolated_session_with_network("session-a");
    let (state_b, network_b) = isolated_session_with_network("session-b");
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let (tx_a, rx_a) = crossbeam_channel::unbounded();
    let (tx_b, rx_b) = crossbeam_channel::unbounded();
    let live_state_a = LiveState::with_disabled_persistence();
    register_session_runtime(session_a, state_a.clone(), live_state_a.clone(), tx_a);
    register_session_runtime(
        session_b,
        state_b.clone(),
        LiveState::with_disabled_persistence(),
        tx_b,
    );
    wait_for_proxy(network_a.proxy_addr());
    wait_for_proxy(network_b.proxy_addr());
    network_a.drain_denied_hosts();
    network_b.drain_denied_hosts();

    let request = curl_request(&target_x);
    live_state_a.extend_provider_events([ProviderEvent::from(Event::ToolCallRequested(
        request.clone(),
    ))]);
    start_request(&state_a, session_a, &request);
    let (domains, prior_result) = match rx_a
        .recv_timeout(Duration::from_secs(30))
        .expect("first curl completion")
    {
        BashCompletion::DomainDenied {
            domains, result, ..
        } => (domains, result),
        other => panic!("expected the first curl to be domain denied, got {other:?}"),
    };
    let frame = live_state_a.extend_provider_events([ProviderEvent::from(
        Event::ApprovalRequested(ApprovalRequest {
            call_id: request.call_id.clone(),
            reason: "allow exactly the denied host for this session and retry".to_string(),
            kind: ApprovalKind::DomainDenialRetry {
                domains,
                prior_result,
            },
        }),
    )]);
    assert!(matches!(
        resolve_approval(
            &frame,
            session_a,
            request.call_id.clone(),
            ApprovalDecision::Approve,
        ),
        ApprovalOutcome::Started { .. }
    ));
    let reached = match rx_a
        .recv_timeout(Duration::from_secs(30))
        .expect("approved retry completion")
    {
        BashCompletion::Finished(result) => {
            assert!(result.output["output"]
                .as_str()
                .unwrap_or_default()
                .contains("ORIGIN-X"));
            result
        }
        other => panic!("approved retry did not finish normally: {other:?}"),
    };
    assert!(
        !reached.is_error,
        "approved reach was marked failed: {reached:?}"
    );
    assert_eq!(reached.output["domain_approved"], true);
    expect_domain_denied(&state_a, session_a, &rx_a, &target_y, "ORIGIN-Y");
    expect_domain_denied(&state_b, session_b, &rx_b, &target_x, "ORIGIN-X");

    unregister_session_runtime(session_a);
    unregister_session_runtime(session_b);
}

#[test]
fn proxy_unaware_direct_connect_cannot_bypass_the_fixed_endpoint() {
    let (tool_state, _network) = isolated_session_with_network("direct-egress");
    let session_id = SessionId::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        LiveState::with_disabled_persistence(),
        tx,
    );
    let request = ToolCallRequest {
        call_id: ToolCallId("direct-connect".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "exec 3<>/dev/tcp/127.0.0.2/80" }).into(),
    };
    assert!(matches!(
        execute_agent_tool(&StubHostTools, &tool_state, session_id, &request),
        Execution::Started(_)
    ));
    match rx
        .recv_timeout(Duration::from_secs(30))
        .expect("completion")
    {
        BashCompletion::Finished(result) => {
            assert_ne!(result.output["exit_code"], 0);
            assert!(result.output["denied_network_routes"].is_array());
        }
        BashCompletion::DomainDenied { domains, .. } => {
            panic!("kernel-side bypass must not become a domain grant: {domains:?}")
        }
        BashCompletion::FilesystemDenied { denials, .. } => {
            panic!("unexpected filesystem denial: {denials:?}")
        }
        BashCompletion::DomainGrantRequired { domains, .. } => {
            panic!("kernel-side bypass must not become a host-side domain grant: {domains:?}")
        }
    }
    unregister_session_runtime(session_id);
}
