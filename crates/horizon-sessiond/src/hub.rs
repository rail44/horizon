//! The daemon's [`SessionHub`] implementation — the v10 replacement for
//! `main`'s JSONL kind-dispatch loop (`docs/remoc-adoption-design.md` §2).
//! One [`Hub`] is built per accepted connection and served via
//! `SessionHubServerShared` with per-call task spawning, so a slow call
//! (a PTY spawn, a replay) never blocks the others; the process-lifetime
//! state stays where it always was ([`SessiondState`]/[`TerminalHost`]),
//! reached through the same `Connection` seam.
//!
//! Bridging pattern, used by every attachment: the PTY/session side of the
//! daemon is synchronous (std threads, crossbeam channels), so each remote
//! channel gets a local unbounded tokio channel as its sync-sendable half,
//! and a small async pump task that drains it into the remote `rch`
//! sender. On the receive side, pumps skip an item whose deserialization
//! failed (adoption condition 2: a non-final [`recv
//! error`](remoc::rch::mpsc::RecvError::is_final) never tears the channel
//! down) and stop on final errors.

use std::sync::atomic::{AtomicBool, Ordering};

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use horizon_session_protocol::{
    AgentAttachment, ClientHello, DecodeSkipLog, HubError, HubHello, SessionHub,
    TerminalAttachment, VersionRange, WireCodec, COMMAND_MAX_ITEM_BYTES, CONTROL_MAX_ITEM_BYTES,
    FRAME_MAX_ITEM_BYTES, TERMINAL_EVENT_MAX_ITEM_BYTES, TOOL_IO_MAX_ITEM_BYTES,
};
use horizon_terminal_core::{
    TerminalCommand, TerminalFrame, TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use remoc::rch;
use remoc::rch::watch::WatchExt as _;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

use crate::session::Connection;
use crate::terminal::{SubscriberChannels, TerminalHost};

/// Local buffer of each remote channel (the rch-side counterpart of the
/// unbounded local bridges). Frames are the hot path; 64 matches the spike
/// bench's channel sizing.
const CHANNEL_BUFFER: usize = 64;

pub(crate) struct Hub {
    connection: Connection,
    terminals: TerminalHost,
    binary_id: &'static str,
    /// Whether this connection's `hello` has completed successfully — the
    /// enforcement half of "`hello` is the first call on every connection"
    /// (§3). See [`Self::require_hello`].
    hello_completed: AtomicBool,
}

impl Hub {
    pub(crate) fn new(
        connection: Connection,
        terminals: TerminalHost,
        binary_id: &'static str,
    ) -> Self {
        Self {
            connection,
            terminals,
            binary_id,
            hello_completed: AtomicBool::new(false),
        }
    }

    /// The hello gate: every method except `hello` itself and `drain` (the
    /// version-stable recovery surface a *rejected* client legitimately
    /// calls) refuses to run before a successful negotiation, rather than
    /// trusting the client to call in order.
    fn require_hello(&self) -> Result<(), HubError> {
        if self.hello_completed.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(HubError::HelloRequired)
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

        // Frames: a watch seeded with the retained latest frame. The
        // creator caps both directions (`FRAME_MAX_ITEM_BYTES`) — the
        // receiver's const parameter travels with it when transported, the
        // sender's runtime limit gates serialization.
        let (frame_tx, frame_rx) = rch::watch::channel::<TerminalFrame, WireCodec>(seed)
            .with_max_item_size::<FRAME_MAX_ITEM_BYTES>();
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
                    eprintln!("horizon-sessiond: closing a terminal event attachment: {err}");
                    break;
                }
            }
        });

        let (mut command_tx, mut command_rx) =
            rch::mpsc::channel::<TerminalCommand, WireCodec>(CHANNEL_BUFFER);
        // A transported sender carries the cap its creator set: this is
        // the daemon-side receive limit for the UI's commands.
        command_tx.set_max_item_size(COMMAND_MAX_ITEM_BYTES);
        let terminals = self.terminals.clone();
        tokio::spawn(async move {
            let mut skips = DecodeSkipLog::new("horizon-sessiond terminal commands");
            loop {
                match command_rx.recv().await {
                    Ok(Some(command)) => terminals.handle_command(session_id, command),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    // Adoption condition 2: one undecodable command is
                    // skipped; the channel survives.
                    Err(err) => skips.note(&err),
                }
            }
        });

        TerminalAttachment {
            frames: frame_rx,
            events: event_rx,
            commands: command_tx,
        }
    }

    /// Wires one agent attachment: local event bridge → remote events
    /// channel, remote commands channel → the session thread's inbound
    /// queue.
    fn agent_attachment(
        &self,
        session_id: SessionId,
        mut local_events: UnboundedReceiver<AgentWireEvent>,
    ) -> AgentAttachment {
        let (event_tx, event_rx) = rch::mpsc::channel::<AgentWireEvent, WireCodec>(CHANNEL_BUFFER);
        // Tool I/O size cap (events carry `JsonValue` payloads) -- see
        // `TOOL_IO_MAX_ITEM_BYTES`'s doc.
        let event_rx = event_rx.set_max_item_size::<TOOL_IO_MAX_ITEM_BYTES>();
        tokio::spawn(async move {
            while let Some(event) = local_events.recv().await {
                if let Err(err) = event_tx.send(event).await {
                    // See the terminal-update pump: send errors latch, so
                    // the attachment ends rather than skip-looping.
                    eprintln!("horizon-sessiond: closing an agent event attachment: {err}");
                    break;
                }
            }
        });

        let (mut command_tx, mut command_rx) =
            rch::mpsc::channel::<Command, WireCodec>(CHANNEL_BUFFER);
        command_tx.set_max_item_size(COMMAND_MAX_ITEM_BYTES);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let mut skips = DecodeSkipLog::new("horizon-sessiond agent commands");
            loop {
                match command_rx.recv().await {
                    Ok(Some(command)) => connection.route_command(session_id, command),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    Err(err) => skips.note(&err),
                }
            }
        });

        AgentAttachment {
            events: event_rx,
            commands: command_tx,
        }
    }
}

impl SessionHub for Hub {
    /// The §3 range negotiation, plus the connection-global channel
    /// handover. `hello` never touches session state (the bind-first
    /// ordering in `main` relies on it answering immediately, before the
    /// event-log resume finishes).
    async fn hello(&self, client: ClientHello) -> Result<HubHello, HubError> {
        let ours = VersionRange::ours();
        let Some(negotiated) = ours.negotiate(client.supported) else {
            let reason = HubError::IncompatibleVersion {
                client: client.supported,
                daemon: ours,
            };
            eprintln!(
                "horizon-sessiond: rejecting hello from {}: {reason}",
                client.binary_id
            );
            return Err(reason);
        };

        // Host-tool requests: sessions push into the connection-global
        // local bridge; this pump forwards them to the client.
        let (request_tx, request_rx) =
            rch::mpsc::channel::<HostToolRequest, WireCodec>(CHANNEL_BUFFER);
        let request_rx = request_rx.set_max_item_size::<TOOL_IO_MAX_ITEM_BYTES>();
        let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
        self.connection.connect_host_tools(local_tx);
        tokio::spawn(async move {
            while let Some(request) = local_rx.recv().await {
                if let Err(err) = request_tx.send(request).await {
                    // See the terminal-update pump: send errors latch, so
                    // the channel ends rather than skip-looping.
                    eprintln!("horizon-sessiond: closing the host-tool request channel: {err}");
                    break;
                }
            }
        });

        // Host-tool responses: routed to whichever session thread blocks
        // on the matching request id.
        let (mut response_tx, mut response_rx) =
            rch::mpsc::channel::<HostToolResponse, WireCodec>(CHANNEL_BUFFER);
        response_tx.set_max_item_size(TOOL_IO_MAX_ITEM_BYTES);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let mut skips = DecodeSkipLog::new("horizon-sessiond host-tool responses");
            loop {
                match response_rx.recv().await {
                    Ok(Some(response)) => connection.handle_host_tool_response(response),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    Err(err) => skips.note(&err),
                }
            }
        });

        // Startup skipped-lines diagnostics: at most one message, after
        // the resume finishes — never blocking hello's own reply.
        let (skipped_tx, skipped_rx) = rch::mpsc::channel::<String, WireCodec>(1);
        let skipped_rx = skipped_rx.set_max_item_size::<CONTROL_MAX_ITEM_BYTES>();
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            if let Some(summary) = connection.skipped_lines_summary() {
                let _ = skipped_tx.send(summary).await;
            }
        });

        self.hello_completed.store(true, Ordering::Release);
        Ok(HubHello {
            negotiated,
            binary_id: self.binary_id.to_string(),
            host_tools: request_rx,
            host_tool_responses: response_tx,
            skipped_lines: skipped_rx,
        })
    }

    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError> {
        self.require_hello()?;
        Ok(self.terminals.list())
    }

    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError> {
        self.require_hello()?;
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
        self.require_hello()?;
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

    /// Readiness-gated exactly as the JSONL `session_list` was (bind-first
    /// fix in `main`): a client connecting while the startup resume is
    /// still running must not see a partial view.
    async fn list_agents(&self) -> Result<Vec<SessionSummary>, HubError> {
        self.require_hello()?;
        self.connection.wait_until_resume_ready().await;
        Ok(self.connection.session_list())
    }

    /// Readiness-gated like `list_agents` for a different reason: the
    /// session's persistence choice is decided once at spawn time, and a
    /// spawn racing `set_writer` would silently run without persistence
    /// for its whole lifetime (see the old `Control::SessionNew` arm's
    /// comment, preserved by this gate).
    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError> {
        self.require_hello()?;
        self.connection.wait_until_resume_ready().await;
        let session_id = new.session_id;
        let local_events = self.connection.subscribe_agent(session_id);
        self.connection.handle_session_new(new);
        Ok(self.agent_attachment(session_id, local_events))
    }

    /// The old `Control::SessionLoad`: subscribe, replay the session's
    /// committed events, re-announce its resolved model, then live events
    /// flow — all in order through the same bridge. An unknown session id
    /// succeeds with an empty replay, as before.
    async fn attach_agent(&self, session_id: SessionId) -> Result<AgentAttachment, HubError> {
        self.require_hello()?;
        self.connection.wait_until_resume_ready().await;
        let local_events = self.connection.subscribe_agent(session_id);
        let mut skipped_unknown = 0_u64;
        for event in self.connection.replay_events(session_id).await {
            if !replayable(&event) {
                skipped_unknown += 1;
                continue;
            }
            self.connection
                .send_session_event(session_id, AgentWireEvent::Event(event));
        }
        if skipped_unknown > 0 {
            eprintln!(
                "horizon-sessiond: withheld {skipped_unknown} unknown event(s) from \
                 {session_id:?}'s replay (log lines written by a newer build; see \
                 `replayable`)"
            );
        }
        if let Some(model) = self.connection.session_model(session_id) {
            self.connection
                .send_session_event(session_id, AgentWireEvent::SessionModel(model));
        }
        Ok(self.agent_attachment(session_id, local_events))
    }

    async fn drain(&self) -> Result<(), HubError> {
        self.terminals.shutdown_all();
        flush_event_log_before_exit(self.connection.writer());
        eprintln!("horizon-sessiond: drained, exiting");
        std::process::exit(0);
    }
}

/// Blocks until every event-log record enqueued so far has actually been
/// written and flushed to disk (see [`WriterHandle::flush`]'s doc comment),
/// then returns -- called right before `std::process::exit(0)` on a
/// [`SessionHub::drain`]. An `Appender::append_provider_events` call only
/// enqueues onto the writer's own background thread; forwarding the
/// resulting event to a connected client happens after that same enqueue,
/// not after it becomes durable. Without this, a client observing a
/// session's latest event over the wire and immediately draining could
/// still race the writer's thread and lose it — indistinguishable from the
/// `kill -9` case this binary has no signal handler for, except a graceful
/// drain has every opportunity to just wait instead. A blocking call is
/// safe here despite running on an async task: this is the last thing that
/// happens before the process exits, so there is nothing else for the
/// runtime to make progress on.
fn flush_event_log_before_exit(writer: Option<WriterHandle>) {
    if let Some(writer) = writer {
        if let Err(error) = writer.flush() {
            eprintln!("horizon-sessiond: failed to flush event log before draining: {error}");
        }
    }
}

/// Whether a log-replayed event may be forwarded onto the wire.
/// `Event::Unknown` is a *received* degradation — a log line written by a
/// newer build that this one can only read as "something happened" — and
/// re-serializing it would put the literal `Unknown` tag on the wire,
/// which no peer is ever supposed to see (the §4 catch-alls exist for
/// *receiving*, not sending). The live path can never produce one (a
/// session thread only emits events this build constructed); replay is
/// the one seam where log-borne `Unknown`s could leak out, so they are
/// withheld here and counted in the caller's log line.
fn replayable(event: &Event) -> bool {
    !matches!(event, Event::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessiondState;
    use horizon_agent::config::AgentConfig;
    use horizon_agent::contract::ProviderRegistry;
    use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
    use horizon_session_protocol::VersionRange;
    use std::sync::Arc;

    fn test_hub() -> Hub {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
        ));
        Hub::new(Connection::new(state), TerminalHost::new(), "test-sessiond")
    }

    /// The hello gate (review item): a method called before `hello` — or
    /// after a *rejected* hello — is refused with `HelloRequired`; a
    /// successful negotiation opens the gate. (`drain` is deliberately
    /// exempt: it is the version-stable recovery surface a rejected
    /// client legitimately calls — enforced by it taking no
    /// `require_hello`, which this test cannot exercise directly since
    /// `drain` exits the process.)
    #[tokio::test]
    async fn non_hello_methods_are_refused_until_hello_succeeds() {
        let hub = test_hub();

        // Before any hello.
        assert!(matches!(
            hub.list_terminals().await,
            Err(HubError::HelloRequired)
        ));

        // A rejected hello leaves the gate closed.
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
            hub.list_agents().await,
            Err(HubError::HelloRequired)
        ));
        assert!(matches!(
            hub.attach_terminal(uuid::Uuid::new_v4()).await,
            Err(HubError::HelloRequired)
        ));

        // A successful negotiation opens it.
        hub.hello(ClientHello::new("test-client"))
            .await
            .expect("a matching range must negotiate");
        assert_eq!(hub.list_terminals().await.unwrap(), Vec::new());
    }
}
