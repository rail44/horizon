//! The daemon's [`TerminalHub`] implementation — the terminal half of what
//! `horizon-agentd`'s hub used to serve, moved here verbatim by the v17
//! split (`docs/terminald-split-design.md`). One [`Hub`] is built per
//! accepted connection and served via `TerminalHubServerShared` with
//! per-call task spawning, so a slow call (a PTY spawn) never blocks the
//! others; the process-lifetime state stays in [`TerminalHost`].
//!
//! Bridging pattern, unchanged from the agentd era: the PTY side of the
//! daemon is synchronous (std threads, crossbeam channels), so each remote
//! channel gets a local unbounded tokio channel as its sync-sendable half,
//! and a small async pump task that drains it into the remote `rch` sender.
//! The receive side is not written here at all — it is
//! [`horizon_wire::receive_pump`], the same loop agentd drives its inbound
//! channels with, so adoption condition 2's skip-vs-fatal boundary has one
//! definition rather than one per runtime. The *send* pumps stay local:
//! their break conditions genuinely differ (a latching mpsc send error, a
//! watch whose send fails only once every receiver is gone).

use std::sync::Arc;

use horizon_terminal_core::wire::{
    HandOverRefused, TerminalAttachment, TerminalHub, TerminalHubHello, terminal_version_range,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use horizon_wire::{
    CHANNEL_BUFFER, COMMAND_MAX_ITEM_BYTES, ClientHello, HelloGate, HubError,
    TERMINAL_EVENT_MAX_ITEM_BYTES, WireCodec, receive_pump,
};
use remoc::rch;
use uuid::Uuid;

use crate::DAEMON_NAME;
use crate::next_image::{self, NextImage};
use crate::terminal::{SubscriberChannels, TerminalHost};

pub(crate) struct Hub {
    terminals: TerminalHost,
    binary_id: &'static str,
    /// Whether this connection's `hello` has completed successfully — the
    /// enforcement half of "`hello` is the first call on every connection"
    /// (`docs/remoc-adoption-design.md` §3), shared with `horizon-agentd`'s
    /// hub so the invariant has one definition.
    hello: HelloGate,
    next: Arc<NextImage>,
}

impl Hub {
    pub(crate) fn new(
        terminals: TerminalHost,
        binary_id: &'static str,
        next: Arc<NextImage>,
    ) -> Self {
        Self {
            terminals,
            binary_id,
            hello: HelloGate::new(),
            next,
        }
    }

    /// Wires one terminal attachment: local event bridge → remote events
    /// mpsc, remote commands channel → [`TerminalHost::handle_command`].
    fn terminal_attachment(
        &self,
        session_id: Uuid,
        channels: SubscriberChannels,
    ) -> TerminalAttachment {
        let SubscriberChannels {
            events: mut local_events,
            ..
        } = channels;

        // Events: the state, the bytes, and what the session says.
        let (event_tx, event_rx) =
            rch::mpsc::with_local_buffer::<TerminalUpdate, WireCodec>(CHANNEL_BUFFER);
        let event_rx = event_rx.set_max_item_size::<TERMINAL_EVENT_MAX_ITEM_BYTES>();
        tokio::spawn(async move {
            while let Some(event) = local_events.recv().await {
                if let Err(err) = event_tx.send(event).await {
                    // Any send failure ends the event bridge: a dead remote,
                    // or an item over the cap — rch *latches* a remote-send
                    // error on the local sender (measured: every later send
                    // fails too), so skip-and-continue would error forever.
                    // Dropping `local_events` makes the host's next send
                    // fail, which lazily removes the subscriber; the client
                    // re-attaches for fresh channels.
                    eprintln!("horizon-terminald: closing a terminal event attachment: {err}");
                    break;
                }
            }
        });

        let (mut command_tx, command_rx) =
            rch::mpsc::with_local_buffer::<TerminalCommand, WireCodec>(CHANNEL_BUFFER);
        // A transported sender carries the cap its creator set: this is
        // the daemon-side receive limit for the UI's commands.
        command_tx.set_max_item_size(COMMAND_MAX_ITEM_BYTES);
        let terminals = self.terminals.clone();
        tokio::spawn(receive_pump(
            command_rx,
            "horizon-terminald terminal commands",
            move |command| terminals.handle_command(session_id, command),
        ));

        TerminalAttachment {
            events: event_rx,
            commands: command_tx,
        }
    }
}

impl TerminalHub for Hub {
    /// The §3 range negotiation. Channel-free, unlike agentd's `hello`:
    /// every connection-global channel belongs to the agent domain, so this
    /// reply is just the negotiated version plus this binary's id (the skew
    /// insurance the client records — see [`TerminalHubHello`]).
    async fn hello(&self, client: ClientHello) -> Result<TerminalHubHello, HubError> {
        let negotiated =
            horizon_wire::negotiate_hello(terminal_version_range(), &client, DAEMON_NAME)?;
        self.hello.mark_completed();
        Ok(TerminalHubHello {
            hands_over: true,
            negotiated,
            binary_id: self.binary_id.to_string(),
        })
    }

    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError> {
        self.hello.require()?;
        Ok(self.terminals.list())
    }

    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError> {
        self.hello.require()?;
        // Subscribe before spawning so the session's very first events
        // are never lost — the JSONL flow's `mark_attached`-before-create,
        // made structural.
        let channels = self.terminals.subscribe_for_create(session_id);
        // `TerminalHost::create` blocks (bounded spawn attempts with a
        // watchdog timeout each — see its doc comment); `serve(true)` runs
        // this call on its own task, and `spawn_blocking` keeps that task
        // off the async workers.
        let terminals = self.terminals.clone();
        let spawned = tokio::task::spawn_blocking(move || terminals.create(session_id, spec)).await;
        match spawned {
            Ok(Ok(())) => Ok(self.terminal_attachment(session_id, channels)),
            Ok(Err(error)) => {
                self.terminals.unsubscribe(session_id, &channels);
                Err(HubError::TerminalSpawnFailed(error))
            }
            Err(join_error) => {
                self.terminals.unsubscribe(session_id, &channels);
                Err(HubError::TerminalSpawnFailed(join_error.to_string()))
            }
        }
    }

    async fn attach_terminal(&self, session_id: Uuid) -> Result<TerminalAttachment, HubError> {
        self.hello.require()?;
        // Existence check and subscriber install happen under one lock
        // (`TerminalHost::attach_subscribe`), so a session exiting
        // concurrently either turns this into `TerminalNotFound` or
        // delivers `Exited` through the fresh subscriber — never a live
        // attachment onto a dead session.
        let Some(channels) = self.terminals.attach_subscribe(session_id) else {
            return Err(HubError::TerminalNotFound);
        };
        Ok(self.terminal_attachment(session_id, channels))
    }

    /// The destructive half of `Reload Terminal Runtime`: every hosted PTY
    /// is killed and this process exits, so the UI's terminal sessions are
    /// genuinely gone (close-vs-terminate discipline — this is a terminate).
    async fn drain(&self) -> Result<(), HubError> {
        self.terminals.shutdown_all();
        eprintln!("horizon-terminald: drained, exiting");
        std::process::exit(0);
    }

    /// Asks no `hello`, as `drain` does: a shell built after a terminal
    /// wire bump is refused by this image's `hello` and still hands over.
    async fn hand_over(&self) -> Result<HandOverRefused, HubError> {
        eprintln!("horizon-terminald: handing the terminals to the next image");
        let terminals = self.terminals.clone();
        let next = self.next.clone();
        let reason = tokio::task::spawn_blocking(move || next_image::hand_over(&terminals, &next))
            .await
            .unwrap_or_else(|error| format!("the handover stopped: {error}"));
        Ok(HandOverRefused { reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_terminal_core::wire::{TERMINAL_PROTOCOL_VERSION, terminal_client_hello};
    use horizon_wire::VersionRange;

    fn test_hub() -> Hub {
        let next = Arc::new(NextImage {
            executable: "/nonexistent/horizon-terminald".into(),
            socket: "/nonexistent/terminald.sock".into(),
            listener: -1,
            again: None,
        });
        Hub::new(TerminalHost::new(), "test-terminald", next)
    }

    /// The hello gate: a method called before `hello` — or after a
    /// *rejected* hello — is refused with `HelloRequired`; a successful
    /// negotiation opens the gate. (`drain` is deliberately exempt: it is
    /// the version-stable recovery surface a rejected client legitimately
    /// calls — enforced by it taking no `hello.require()`, which this test
    /// cannot exercise directly since `drain` exits the process.)
    #[tokio::test]
    async fn non_hello_methods_are_refused_until_hello_succeeds() {
        let hub = test_hub();

        std::assert_matches!(hub.list_terminals().await, Err(HubError::HelloRequired));

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
        std::assert_matches!(hub.list_terminals().await, Err(HubError::HelloRequired));
        assert!(matches!(
            hub.attach_terminal(Uuid::new_v4()).await,
            Err(HubError::HelloRequired)
        ));

        hub.hello(terminal_client_hello("test-client"))
            .await
            .expect("a matching range must negotiate");
        assert_eq!(hub.list_terminals().await.unwrap(), Vec::new());
    }

    /// `hello` reports this binary's id — the value the client records as
    /// its skew insurance (`docs/terminald-split-design.md` decision 6).
    #[tokio::test]
    async fn hello_reports_the_negotiated_version_and_this_binarys_id() {
        let hub = test_hub();
        let hello = hub
            .hello(terminal_client_hello("test-client"))
            .await
            .expect("a matching range must negotiate");
        assert_eq!(hello.negotiated, TERMINAL_PROTOCOL_VERSION);
        assert_eq!(hello.binary_id, "test-terminald");
    }
}
