//! End-to-end test against the real `horizon-agentd` binary (spawned via
//! `CARGO_BIN_EXE_horizon-agentd`, only available because this test lives in
//! the same package as the `[[bin]]` target) -- see
//! `docs/agent-runtime-split-design.md`'s step 2 deliverables.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, Hello, CONTRACT_VERSION};
use tokio::io::BufReader;
use tokio::net::UnixStream;

/// Owns the spawned `horizon-agentd` child and its socket path; kills the
/// child and removes the socket file on drop so a failing assertion doesn't
/// leak either across test runs.
struct AgentdProcess {
    child: Child,
    socket_path: PathBuf,
}

impl AgentdProcess {
    fn spawn() -> Self {
        let socket_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-{}-{}.sock",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let child = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"))
            .arg("--socket")
            .arg(&socket_path)
            .spawn()
            .expect("failed to spawn horizon-agentd");
        Self { child, socket_path }
    }
}

impl Drop for AgentdProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn connect_with_retry(path: &std::path::Path) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "horizon-agentd never accepted a connection on {}",
        path.display()
    );
}

async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    for _ in 0..200 {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("horizon-agentd did not exit in time");
}

#[tokio::test]
async fn hello_ping_session_list_and_drain_over_the_real_socket() {
    let mut agentd = AgentdProcess::spawn();
    let stream = connect_with_retry(&agentd.socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(Hello {
            contract_version: CONTRACT_VERSION,
            binary_id: "test-client".to_string(),
            capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();

    let reply = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should reply to hello");
    let EnvelopeBody::Control(Control::Hello(hello)) = reply.body else {
        panic!("expected a hello reply, got {:?}", reply.body);
    };
    assert_eq!(hello.contract_version, CONTRACT_VERSION);
    assert!(!hello.binary_id.is_empty());

    wire::write_envelope(&mut write_half, &Envelope::control(Control::Ping))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(reply.body, EnvelopeBody::Control(Control::Pong));

    wire::write_envelope(&mut write_half, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(Vec::new()))
    );

    wire::write_envelope(&mut write_half, &Envelope::control(Control::Drain))
        .await
        .unwrap();

    let status = wait_for_exit(&mut agentd.child).await;
    assert!(
        status.success(),
        "horizon-agentd should exit 0 after drain, got {status:?}"
    );
}

#[tokio::test]
async fn a_hello_with_the_wrong_contract_version_is_rejected_with_a_reason() {
    let agentd = AgentdProcess::spawn();
    let stream = connect_with_retry(&agentd.socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let wrong_version = CONTRACT_VERSION + 1;
    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(Hello {
            contract_version: wrong_version,
            binary_id: "test-client".to_string(),
            capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();

    let reply = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should still answer, with a rejection");
    let EnvelopeBody::Control(Control::HandshakeRejected(reason)) = reply.body else {
        panic!("expected a handshake rejection, got {:?}", reply.body);
    };
    assert!(
        reason.contains("reload required"),
        "rejection reason was: {reason}"
    );

    // Rejected handshakes end the connection -- the next read observes a
    // clean close rather than the server continuing to serve requests for
    // a client whose contract version it can't trust.
    let next = wire::read_envelope(&mut reader).await.unwrap();
    assert!(next.is_none(), "expected the connection to be closed");
}
