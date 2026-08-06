//! End-to-end tests against the real `horizon-logd` binary, over the actual
//! `LogHub` rtc trait on an actual unix socket: `hello` range negotiation,
//! `ingest` performing real flock append + id/rank computation, and `drain`.
//!
//! Uses a multi-thread runtime because the remoc chmux mux task must be
//! polled concurrently with the test's own awaits.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use horizon_board::wire::{
    log_client_hello, IngestReply, IngestRequest, LogHub as _, LogHubClient, LOG_PROTOCOL_VERSION,
    MIN_SUPPORTED_LOG_PROTOCOL_VERSION,
};
use horizon_daemon_testkit::{
    connect_hub_client, connect_with_retry, resolve_daemon_binary, scratch_socket, wait_for_exit,
    DaemonProcess,
};
use horizon_wire::{ClientHello, HubError, VersionRange, WireCodec};

/// Resolves the `horizon-logd` binary to spawn.
fn resolve_logd_binary() -> PathBuf {
    resolve_daemon_binary("horizon-logd", env!("CARGO_BIN_EXE_horizon-logd"))
}

/// Spawns `horizon-logd` on a fresh throwaway socket. The handle kills the
/// child and unlinks the socket on drop.
fn spawn_logd() -> DaemonProcess {
    let socket_path = scratch_socket("logd-e2e");
    let mut command = Command::new(resolve_logd_binary());
    command.arg("--socket").arg(&socket_path);
    DaemonProcess::spawn(&mut command, socket_path)
}

/// A connected `LogHub` client over the real socket.
struct HubTestClient {
    hub: LogHubClient<WireCodec>,
    _conn_task: tokio::task::JoinHandle<()>,
}

impl Drop for HubTestClient {
    fn drop(&mut self) {
        self._conn_task.abort();
    }
}

async fn connect_hub(socket_path: &std::path::Path) -> HubTestClient {
    let stream = connect_with_retry(socket_path).await;
    let (hub, conn_task) = connect_hub_client::<LogHubClient<WireCodec>>(stream).await;
    HubTestClient {
        hub,
        _conn_task: conn_task,
    }
}

/// `hello` negotiates the current version and reports the daemon's binary id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_negotiates_and_reports_binary_id() {
    let logd = spawn_logd();
    let client = connect_hub(&logd.socket_path).await;

    let hello = client
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello must succeed");
    assert_eq!(hello.negotiated, LOG_PROTOCOL_VERSION);
    assert!(hello.binary_id.starts_with("horizon-logd/"));
}

/// An incompatible version range is rejected, but `drain` still works on the
/// same connection (the version-stable recovery surface).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_version_range_is_rejected_but_drain_still_works() {
    let mut logd = spawn_logd();
    let stream = connect_with_retry(&logd.socket_path).await;
    let (hub, conn_task) = connect_hub_client::<LogHubClient<WireCodec>>(stream).await;

    let future = VersionRange {
        min_supported: LOG_PROTOCOL_VERSION + 100,
        current: LOG_PROTOCOL_VERSION + 100,
    };
    let result = hub
        .hello(ClientHello {
            supported: future,
            binary_id: "future-horizon".to_string(),
        })
        .await;
    match result {
        Err(HubError::IncompatibleVersion { client, daemon }) => {
            assert_eq!(client, future);
            assert_eq!(daemon.current, LOG_PROTOCOL_VERSION);
            assert_eq!(daemon.min_supported, MIN_SUPPORTED_LOG_PROTOCOL_VERSION);
        }
        Err(other) => panic!("expected a version rejection, got {other:?}"),
        Ok(_) => panic!("a disjoint version range must be rejected"),
    }

    let _ = tokio::time::timeout(Duration::from_secs(5), hub.drain()).await;
    let status = wait_for_exit(&mut logd.child).await;
    assert!(status.success(), "drain should exit cleanly");
    conn_task.abort();
}

/// `ingest` appends to the target file and returns the assigned seq (item id).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_appends_and_assigns_seq() {
    let logd = spawn_logd();
    let client = connect_hub(&logd.socket_path).await;
    client
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-e2e-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("events.jsonl");

    // Add an item — should get id 1, rank "n".
    let reply = client
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "First".to_string(),
                body: "Body".to_string(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        )
        .await
        .expect("ingest add");
    match reply {
        IngestReply::Item(item) => {
            assert_eq!(item.id, 1);
            assert_eq!(item.title, "First");
            assert_eq!(item.rank, "n");
        }
        other => panic!("expected Item, got {other:?}"),
    }

    // Add a second item — should get id 2.
    let reply = client
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "Second".to_string(),
                body: String::new(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        )
        .await
        .expect("ingest add 2");
    match reply {
        IngestReply::Item(item) => assert_eq!(item.id, 2),
        other => panic!("expected Item, got {other:?}"),
    }

    // Verify the file has two lines.
    let text = std::fs::read_to_string(&path).expect("read events.jsonl");
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"item-created\""));
    assert!(lines[0].contains("\"id\":1"));
    assert!(lines[1].contains("\"id\":2"));
}

/// `ingest` with `Comment` on a nonexistent item returns `ItemNotFound`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_comment_on_nonexistent_item_is_rejected() {
    use horizon_board::wire::LogError;

    let logd = spawn_logd();
    let client = connect_hub(&logd.socket_path).await;
    client
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-e2e-err-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("events.jsonl");

    let result = client
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Comment {
                id: 99,
                author: "x".to_string(),
                text: "y".to_string(),
            },
        )
        .await;
    assert!(
        matches!(result, Err(LogError::ItemNotFound(99))),
        "expected ItemNotFound(99), got {result:?}"
    );
}

/// The full client→daemon round-trip: `horizon_board::Store` (the library
/// callers use — CLI and GUI) connect-or-spawns this very daemon, hellos,
/// and sends `ingest` over the real socket. `CARGO_BIN_EXE_horizon-logd`
/// (the env var `resolve_daemon_binary` reads) is guaranteed by cargo for
/// this test's own package, so the binary is always built before the test
/// runs — the ordering gap that broke `horizon-board`'s in-crate spawn is
/// absent here by construction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn board_store_client_round_trip_through_real_logd() {
    use horizon_board::{Position, Store};

    // Point the store at this daemon's socket (not the default path).
    let socket_path = scratch_socket("logd-board-rt");
    let mut command = Command::new(resolve_logd_binary());
    command.arg("--socket").arg(&socket_path);
    let logd = DaemonProcess::spawn(&mut command, socket_path.clone());

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-board-rt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = Store::at_with_socket(dir.join("events.jsonl"), socket_path);

    // add → set_status → comment → show, all through the socket.
    let item = store
        .add("Task", "body", None, Position::Bottom)
        .expect("add through logd");
    assert_eq!(item.id, 1);
    assert_eq!(item.rank, "n");

    store
        .set_status(1, "ready")
        .expect("set_status through logd");
    store
        .comment(1, "owner", "a note")
        .expect("comment through logd");

    let shown = store.show(1).expect("show").expect("item exists");
    assert_eq!(shown.title, "Task");
    assert_eq!(shown.status, "ready");
    assert_eq!(shown.comments.len(), 1);
    assert_eq!(shown.comments[0].text, "a note");

    // claim on the same store through the same daemon.
    let claimed = store.claim("alice").expect("claim through logd");
    let claimed = claimed.expect("a ready+unassigned item exists");
    assert_eq!(claimed.id, 1);
    assert_eq!(claimed.status, "in-progress");
    assert_eq!(claimed.assignee, "alice");

    drop(logd);
}
