//! The daemon's [`LogHub`] implementation. One [`Hub`] is built per accepted
//! connection and served via `LogHubServerShared` with per-call task spawning,
//! so a slow call (a large fold) never blocks the others.

use std::path::PathBuf;

use horizon_board::wire::{
    log_version_range, IngestReply, IngestRequest, LogError, LogHub, LogHubHello,
};
use horizon_wire::{negotiate_hello, ClientHello, HelloGate, HubError};

/// This daemon's name in every log line and diagnostic.
pub(crate) const DAEMON_NAME: &str = "horizon-logd";

/// Reported in this binary's `hello` reply's `binary_id`.
const BINARY_ID: &str = concat!("horizon-logd/", env!("CARGO_PKG_VERSION"));

/// One connection's hub. Owns only the `hello` gate — the write path is
/// stateless (each `ingest` opens, locks, appends, and releases).
pub struct Hub {
    binary_id: &'static str,
    /// Whether this connection's `hello` has completed successfully — the
    /// enforcement half of "`hello` is the first call on every connection",
    /// shared with the other two daemons' hubs.
    hello: HelloGate,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            binary_id: BINARY_ID,
            hello: HelloGate::new(),
        }
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl LogHub for Hub {
    async fn hello(&self, client: ClientHello) -> Result<LogHubHello, HubError> {
        let negotiated = negotiate_hello(log_version_range(), &client, DAEMON_NAME)?;
        self.hello.mark_completed();
        Ok(LogHubHello {
            negotiated,
            binary_id: self.binary_id.to_string(),
        })
    }

    async fn ingest(&self, path: String, request: IngestRequest) -> Result<IngestReply, LogError> {
        self.hello
            .require()
            .map_err(|_| LogError::Call("hello has not completed".to_string()))?;
        let path = PathBuf::from(path);
        // The write path is synchronous (flock, file I/O); `serve(true)` runs
        // this call on its own task, and `spawn_blocking` keeps that task off
        // the async workers.
        tokio::task::spawn_blocking(move || crate::writer::perform(&path, request))
            .await
            .map_err(|e| LogError::Io(e.to_string()))?
    }

    async fn drain(&self) -> Result<(), HubError> {
        eprintln!("horizon-logd: drained, exiting");
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_board::wire::{log_client_hello, LOG_PROTOCOL_VERSION};
    use horizon_wire::VersionRange;

    fn test_hub() -> Hub {
        Hub::new()
    }

    /// The hello gate: a method called before `hello` — or after a rejected
    /// hello — is refused; a successful negotiation opens the gate. (`drain`
    /// is exempt: it is the version-stable recovery surface a rejected client
    /// legitimately calls — enforced by it taking no `hello.require()`.)
    #[tokio::test]
    async fn non_hello_methods_are_refused_until_hello_succeeds() {
        let hub = test_hub();

        let disjoint = ClientHello {
            supported: VersionRange {
                min_supported: u32::MAX,
                current: u32::MAX,
            },
            binary_id: "future-client".to_string(),
        };
        assert!(matches!(
            hub.hello(disjoint).await,
            Err(HubError::IncompatibleVersion { .. })
        ));

        // An ingest before a successful hello is rejected.
        let result = hub
            .ingest(
                "/tmp/nonexistent".to_string(),
                IngestRequest::Comment {
                    id: 1,
                    author: "x".to_string(),
                    text: "y".to_string(),
                },
            )
            .await;
        assert!(result.is_err());

        hub.hello(log_client_hello("test-client"))
            .await
            .expect("a matching range must negotiate");
    }

    /// `hello` reports this binary's id and the negotiated version.
    #[tokio::test]
    async fn hello_reports_the_negotiated_version_and_this_binarys_id() {
        let hub = test_hub();
        let hello = hub
            .hello(log_client_hello("test-client"))
            .await
            .expect("a matching range must negotiate");
        assert_eq!(hello.negotiated, LOG_PROTOCOL_VERSION);
        assert_eq!(hello.binary_id, BINARY_ID);
    }
}
