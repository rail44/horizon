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

use horizon_terminal_core::wire::{
    terminal_version_range, TerminalAttachment, TerminalHub, TerminalHubHello,
};
use horizon_terminal_core::{
    TerminalCommand, TerminalFrame, TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use horizon_wire::{
    receive_pump, ClientHello, HelloGate, HubError, WireCodec, CHANNEL_BUFFER,
    COMMAND_MAX_ITEM_BYTES, FRAME_MAX_ITEM_BYTES, TERMINAL_EVENT_MAX_ITEM_BYTES,
};
use remoc::rch;
use uuid::Uuid;

use crate::terminal::{SubscriberChannels, TerminalHost};
use crate::DAEMON_NAME;

pub(crate) struct Hub {
    terminals: TerminalHost,
    binary_id: &'static str,
    /// Whether this connection's `hello` has completed successfully — the
    /// enforcement half of "`hello` is the first call on every connection"
    /// (`docs/remoc-adoption-design.md` §3), shared with `horizon-agentd`'s
    /// hub so the invariant has one definition.
    hello: HelloGate,
}

impl Hub {
    pub(crate) fn new(terminals: TerminalHost, binary_id: &'static str) -> Self {
        Self {
            terminals,
            binary_id,
            hello: HelloGate::new(),
        }
    }

    /// Wires one terminal attachment: local frame bridge → remote frames
    /// watch (seeded with `seed`, the daemon-retained latest frame), local
    /// event bridge → remote events mpsc, remote commands channel →
    /// [`TerminalHost::handle_command`]. Since wire v11
    /// (`docs/remoc-adoption-design.md` §5 Option A) the frame path is a
    /// snapshot-valued signal: full frames on `rch::watch<TerminalFrame>`,
    /// latest-value, no diffing.
    fn terminal_attachment(
        &self,
        session_id: Uuid,
        channels: SubscriberChannels,
    ) -> TerminalAttachment {
        let SubscriberChannels {
            seed,
            frames: mut local_frames,
            events: mut local_events,
        } = channels;

        // Frames: a watch seeded with the retained latest frame. Only the
        // receiver's `MAX_ITEM_SIZE` const is set (`FRAME_MAX_ITEM_BYTES`):
        // we transport the receiver and keep the sender, so that const is
        // the effective cap in both directions (the receiver's serializer
        // drives forwarding). The sender's runtime limit is never consulted
        // here — see `FRAME_MAX_ITEM_BYTES`/`CappedWatchReceiver`.
        let (frame_tx, frame_rx) = rch::watch::channel::<TerminalFrame, WireCodec>(seed);
        let frame_rx = frame_rx.set_max_item_size::<FRAME_MAX_ITEM_BYTES>();
        tokio::spawn(async move {
            while let Some(frame) = local_frames.recv().await {
                // `rch::watch::Sender::send` is synchronous and only errors
                // once every receiver is gone — nothing to skip-loop, no
                // backpressure to bound (the watch keeps only the latest).
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
        });

        // Events: the non-frame updates on an mpsc.
        let (event_tx, event_rx) = rch::mpsc::channel::<TerminalUpdate, WireCodec>(CHANNEL_BUFFER);
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
            rch::mpsc::channel::<TerminalCommand, WireCodec>(CHANNEL_BUFFER);
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
            frames: frame_rx,
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
        // Subscribe before spawning so the session's very first frames
        // are never lost — the JSONL flow's `mark_attached`-before-create,
        // made structural.
        let channels = self.terminals.subscribe_for_create(session_id);
        // `TerminalHost::create` blocks (bounded spawn attempts with a
        // watchdog timeout each — see its doc comment on the suspected
        // portable-pty fork hazard); `serve(true)` runs this call on its
        // own task, and `spawn_blocking` keeps that task off the async
        // workers.
        let terminals = self.terminals.clone();
        let spawned = tokio::task::spawn_blocking(move || terminals.create(session_id, spec)).await;
        match spawned {
            Ok(Ok(())) => Ok(self.terminal_attachment(session_id, channels)),
            Ok(Err(error)) => {
                self.terminals.unsubscribe(session_id);
                Err(HubError::TerminalSpawnFailed(error))
            }
            Err(join_error) => {
                self.terminals.unsubscribe(session_id);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_terminal_core::wire::{terminal_client_hello, TERMINAL_PROTOCOL_VERSION};
    use horizon_wire::VersionRange;

    fn test_hub() -> Hub {
        Hub::new(TerminalHost::new(), "test-terminald")
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

        assert!(matches!(
            hub.list_terminals().await,
            Err(HubError::HelloRequired)
        ));

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
        assert!(matches!(
            hub.list_terminals().await,
            Err(HubError::HelloRequired)
        ));
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
