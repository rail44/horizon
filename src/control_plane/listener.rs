//! The control plane's accept loop: a dedicated OS thread that binds
//! [`super::socket::default_socket_path`] and spawns one more thread per
//! accepted connection -- deliberately not `horizon-agentd`'s "one
//! connection at a time" simplification (`docs/cli-control-plane-design.md`'s
//! "Endpoint" decision: the CLI contract assumes multiple concurrent
//! clients from v1).

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use super::connection::handle_connection;
use super::executor::ControlExecutor;

/// Spawns the listener thread and returns immediately -- binding happens on
/// that thread, not the caller's, so a slow or failing bind (e.g. an
/// unwritable `XDG_RUNTIME_DIR`) never blocks Horizon's own startup. A bind
/// failure is logged to stderr and the thread simply exits; there is no
/// retry, matching this feature's "best-effort, not load-bearing" status
/// (see `control_plane::start`'s doc comment).
pub(super) fn spawn(socket_path: PathBuf, executor: impl ControlExecutor + 'static) {
    let executor: Arc<dyn ControlExecutor> = Arc::new(executor);
    thread::spawn(move || {
        let listener = match bind(&socket_path) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!(
                    "horizon: control socket bind failed on {} ({err}) -- external control is \
                     unavailable for this run",
                    socket_path.display()
                );
                return;
            }
        };
        eprintln!(
            "horizon: control socket listening on {}",
            socket_path.display()
        );
        accept_loop(listener, &executor);
    });
}

/// Accepts connections until the socket itself errors out (the process is
/// exiting, or the socket file was removed out from under the listener) --
/// spawning a fresh thread per connection so one slow or wedged client can
/// never delay accepting the next, unlike `horizon-agentd`'s own accept loop
/// (see the module doc).
fn accept_loop(listener: UnixListener, executor: &Arc<dyn ControlExecutor>) {
    for accepted in listener.incoming() {
        match accepted {
            Ok(stream) => {
                let executor = executor.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, executor.as_ref()) {
                        eprintln!("horizon: control connection error: {err}");
                    }
                });
            }
            Err(err) => {
                eprintln!("horizon: control socket accept error: {err}");
            }
        }
    }
}

/// Binds `path`, handling the stale-socket case exactly like `horizon-
/// agentd`'s own `bind_listener` (`crates/horizon-agentd/src/main.rs`): if a
/// socket file already exists there but nothing answers a connection attempt
/// (a previous Horizon process that didn't shut down cleanly -- this path
/// embeds this process's own pid, so in practice this only fires on a pid
/// wraparound colliding with a leftover file), remove it and rebind; if
/// something *is* accepting, refuse to steal the path out from under a live
/// instance.
pub(super) fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_stream) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} is already accepting connections", path.display()),
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::executor::ControlRequest;
    use horizon_control::contract::{Envelope, EnvelopeBody, Hello, CONTROL_VERSION};
    use horizon_control::wire;
    use std::io::BufReader;
    use std::time::{Duration, Instant};

    fn unique_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-control-listener-test-{label}-{}-{}.sock",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn binds_successfully_to_a_fresh_path() {
        let path = unique_path("fresh");

        let listener = bind(&path).expect("fresh path should bind");
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn removes_a_stale_socket_file_and_rebinds() {
        let path = unique_path("stale");
        // A stale socket: bind once, then drop the listener without
        // unlinking -- leaves the file behind with nothing accepting.
        let listener = UnixListener::bind(&path).expect("initial bind");
        drop(listener);

        let rebound = bind(&path).expect("stale socket should be removed and rebound");
        drop(rebound);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn refuses_to_steal_a_path_something_is_actively_accepting_on() {
        let path = unique_path("live");
        let listener = UnixListener::bind(&path).expect("initial bind");

        let error = bind(&path).expect_err("a live listener's path must not be stolen");
        assert!(error.to_string().contains("already accepting"));

        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    struct StubExecutor;

    impl ControlExecutor for StubExecutor {
        fn execute(&self, _request: ControlRequest) -> EnvelopeBody {
            EnvelopeBody::Ok
        }
    }

    fn connect_with_retry(path: &Path) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(stream) = UnixStream::connect(path) {
                return stream;
            }
            if Instant::now() >= deadline {
                panic!("timed out connecting to {}", path.display());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn hello(id: u64) -> Envelope {
        Envelope::new(
            id,
            EnvelopeBody::Hello(Hello {
                control_version: CONTROL_VERSION,
                binary_id: "test-client".to_string(),
            }),
        )
    }

    /// Proves the design's "not agentd's one-connection-at-a-time"
    /// decision: a connection that never sends anything (holding the accept
    /// loop's would-be single slot in an agentd-style server) must not stop
    /// a second, fully-driven connection from being served.
    #[test]
    fn listener_serves_a_second_connection_while_a_first_sits_idle() {
        let path = unique_path("concurrent");
        spawn(path.clone(), StubExecutor);

        let idle_connection = connect_with_retry(&path);
        // Deliberately never write anything on `idle_connection` -- it just
        // holds a slot open the way agentd's single accepted connection
        // would.

        let mut second = connect_with_retry(&path);
        wire::write_envelope(&mut second, &hello(1)).expect("write hello");
        let mut reader = BufReader::new(second.try_clone().expect("clone stream"));
        let reply = wire::read_envelope(&mut reader)
            .expect("read reply")
            .expect("connection should not have closed");

        assert!(matches!(reply.body, EnvelopeBody::HelloAck(_)));

        drop(idle_connection);
        drop(second);
        let _ = std::fs::remove_file(&path);
    }
}
