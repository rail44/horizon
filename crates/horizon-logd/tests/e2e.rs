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

    // Inject the binary path so `connect_or_spawn_logd_retrying` (inside
    // the Store's `ingest`) finds this exact binary instead of searching
    // next to the test process's own executable (which is under deps/, not
    // next to the daemon). `CARGO_BIN_EXE_horizon-logd` is guaranteed by
    // cargo for this test's own package.
    std::env::set_var(
        "HORIZON_LOGD_BINARY",
        resolve_logd_binary().to_string_lossy().to_string(),
    );

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
        .await
        .expect("add through logd");
    assert_eq!(item.id, 1);
    assert_eq!(item.rank, "n");

    store
        .set_status(1, "ready")
        .await
        .expect("set_status through logd");
    store
        .comment(1, "owner", "a note")
        .await
        .expect("comment through logd");

    let shown = store.show(1).expect("show").expect("item exists");
    assert_eq!(shown.title, "Task");
    assert_eq!(shown.status, "ready");
    assert_eq!(shown.comments.len(), 1);
    assert_eq!(shown.comments[0].text, "a note");

    // claim on the same store through the same daemon.
    let claimed = store.claim("alice").await.expect("claim through logd");
    let claimed = claimed.expect("a ready+unassigned item exists");
    assert_eq!(claimed.id, 1);
    assert_eq!(claimed.status, "in-progress");
    assert_eq!(claimed.assignee, "alice");

    drop(logd);
    std::env::remove_var("HORIZON_LOGD_BINARY");
}

// ---- subscribe (stage B) --------------------------------------------------

/// `edit` updates title and body through the real daemon, and a partial edit
/// (only --title) leaves the body untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_updates_title_and_body_through_logd() {
    use horizon_board::{Position, Store};

    let socket_path = scratch_socket("logd-board-edit");
    let mut command = Command::new(resolve_logd_binary());
    command.arg("--socket").arg(&socket_path);
    let logd = DaemonProcess::spawn(&mut command, socket_path.clone());

    std::env::set_var(
        "HORIZON_LOGD_BINARY",
        resolve_logd_binary().to_string_lossy().to_string(),
    );

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-board-edit-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = Store::at_with_socket(dir.join("events.jsonl"), socket_path);

    let item = store
        .add("Old Title", "old body", None, Position::Bottom)
        .await
        .expect("add through logd");
    assert_eq!(item.id, 1);

    // Edit both fields.
    store
        .edit(
            1,
            Some("New Title".to_string()),
            Some("new body".to_string()),
        )
        .await
        .expect("edit through logd");

    let shown = store.show(1).expect("show").expect("item exists");
    assert_eq!(shown.title, "New Title");
    assert_eq!(shown.body, "new body");

    // Partial edit: only title — body must survive.
    store
        .edit(1, Some("Title Only".to_string()), None)
        .await
        .expect("partial edit through logd");

    let shown = store.show(1).expect("show").expect("item exists");
    assert_eq!(shown.title, "Title Only");
    assert_eq!(shown.body, "new body");

    drop(logd);
    std::env::remove_var("HORIZON_LOGD_BINARY");
}

/// `edit` on a nonexistent item returns `ItemNotFound`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_on_nonexistent_item_is_rejected() {
    use horizon_board::wire::LogError;

    let logd = spawn_logd();
    let client = connect_hub(&logd.socket_path).await;
    client
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-edit-err-{}",
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
            IngestRequest::Edit {
                id: 99,
                title: Some("x".to_string()),
                body: None,
            },
        )
        .await;
    assert!(
        matches!(result, Err(LogError::ItemNotFound(99))),
        "expected ItemNotFound(99), got {result:?}"
    );
}

// ---- subscribe (stage B) --------------------------------------------------

/// Connects a raw NDJSON subscriber to logd (not remoc — the subscribe path
/// is first-byte-sniffed away from chmux). Sends the request line, reads
/// the current-seq reply, and returns the stream for further reads.
struct SubscribeClient {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    _write: tokio::net::unix::OwnedWriteHalf,
}

async fn connect_subscriber(
    socket_path: &std::path::Path,
    request: &horizon_board::wire::SubscribeRequest,
) -> SubscribeClient {
    use tokio::io::AsyncWriteExt;
    let stream = connect_with_retry(socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let line = serde_json::to_string(request).unwrap();
    write_half
        .write_all(line.as_bytes())
        .await
        .expect("write subscribe request");
    write_half.write_all(b"\n").await.expect("write newline");
    write_half.flush().await.expect("flush");
    SubscribeClient {
        reader: tokio::io::BufReader::new(read_half),
        _write: write_half,
    }
}

impl SubscribeClient {
    async fn read_line(&mut self) -> Option<String> {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await.expect("read");
        if n == 0 {
            return None;
        }
        if line.ends_with('\n') {
            line.pop();
        }
        Some(line)
    }
}

/// A subscriber connects, receives the current-seq reply (0 for an empty
/// file), then a second client ingests and the subscriber receives the
/// poke {"log":"board","seq":1}.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_receives_poke_after_ingest() {
    use horizon_board::wire::SubscribeRequest;

    let logd = spawn_logd();

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-sub-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("events.jsonl");

    // Connect a subscriber before any ingest.
    let mut sub = connect_subscriber(
        &logd.socket_path,
        &SubscribeRequest {
            path: Some(path.to_string_lossy().to_string()),
            since: None,
        },
    )
    .await;

    // The first line is the current-seq reply: 0 (empty file).
    let first = sub.read_line().await.expect("current-seq reply");
    assert_eq!(first, r#"{"log":"board","seq":0}"#);

    // A second client hellos and ingests an Add.
    let writer = connect_hub(&logd.socket_path).await;
    writer
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");
    let reply = writer
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "First".to_string(),
                body: String::new(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        )
        .await
        .expect("ingest");
    assert!(matches!(reply, IngestReply::Item(_)));

    // The subscriber should receive a poke with seq=1.
    let poke = tokio::time::timeout(Duration::from_secs(5), sub.read_line())
        .await
        .expect("timed out waiting for poke")
        .expect("poke line");
    assert_eq!(poke, r#"{"log":"board","seq":1}"#);
}

/// The cursor-on-connect reply reflects the current seq when the file
/// already has events (not just 0).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_cursor_reply_reflects_current_seq() {
    use horizon_board::wire::SubscribeRequest;

    let logd = spawn_logd();

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-cur-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("events.jsonl");

    // Ingest two items first.
    let writer = connect_hub(&logd.socket_path).await;
    writer
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");
    writer
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "A".to_string(),
                body: String::new(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        )
        .await
        .expect("ingest 1");
    writer
        .hub
        .ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "B".to_string(),
                body: String::new(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        )
        .await
        .expect("ingest 2");

    // Now subscribe — the cursor reply should be seq=2 (two lines in the file).
    let mut sub = connect_subscriber(
        &logd.socket_path,
        &SubscribeRequest {
            path: Some(path.to_string_lossy().to_string()),
            since: Some(1),
        },
    )
    .await;

    let first = sub.read_line().await.expect("current-seq reply");
    assert_eq!(first, r#"{"log":"board","seq":2}"#);
}

/// A subscriber that is stuck (not reading) must not block ingest. The
/// poke may be dropped (lossy by design), but the ingest must complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stuck_subscriber_does_not_block_ingest() {
    use horizon_board::wire::SubscribeRequest;

    let logd = spawn_logd();

    let dir = std::env::temp_dir().join(format!(
        "horizon-logd-stuck-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("events.jsonl");

    // Connect a subscriber but never read from it — its channel will fill.
    let _sub = connect_subscriber(
        &logd.socket_path,
        &SubscribeRequest {
            path: Some(path.to_string_lossy().to_string()),
            since: None,
        },
    )
    .await;

    // Give the subscribe handler a moment to register.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Ingest should complete promptly despite the stuck subscriber.
    let writer = connect_hub(&logd.socket_path).await;
    writer
        .hub
        .hello(log_client_hello("test-client"))
        .await
        .expect("hello");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        writer.hub.ingest(
            path.to_string_lossy().to_string(),
            IngestRequest::Add {
                title: "X".to_string(),
                body: String::new(),
                parent: None,
                position: horizon_board::Position::Bottom,
            },
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "ingest must not block on a stuck subscriber"
    );
    assert!(result.unwrap().is_ok(), "ingest should succeed");
}
