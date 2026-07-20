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


use horizon_agent::contract::{Command, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use horizon_session_protocol::{
    AgentAttachment, ClientHello, HubError, HubHello, SessionHub, TerminalAttachment, VersionRange,
    WireCodec,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use remoc::rch;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

use crate::session::Connection;
use crate::terminal::TerminalHost;

/// Local buffer of each remote channel (the rch-side counterpart of the
/// unbounded local bridges). Frames are the hot path; 64 matches the spike
/// bench's channel sizing.
const CHANNEL_BUFFER: usize = 64;

pub(crate) struct Hub {
    connection: Connection,
    terminals: TerminalHost,
    binary_id: &'static str,
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
        }
    }

    /// Wires one terminal attachment: local update bridge → remote updates
    /// channel, remote commands channel → [`TerminalHost::handle_command`].
    fn terminal_attachment(
        &self,
        session_id: Uuid,
        mut local_updates: UnboundedReceiver<TerminalUpdate>,
    ) -> TerminalAttachment {
        let (update_tx, update_rx) = rch::mpsc::channel::<TerminalUpdate, WireCodec>(CHANNEL_BUFFER);
        tokio::spawn(async move {
            while let Some(update) = local_updates.recv().await {
                if update_tx.send(update).await.is_err() {
                    // The attachment's remote side is gone (detach or dead
                    // connection); dropping `local_updates` makes the
                    // host's next send fail, which lazily removes the
                    // subscriber entry.
                    break;
                }
            }
        });

        let (command_tx, mut command_rx) =
            rch::mpsc::channel::<TerminalCommand, WireCodec>(CHANNEL_BUFFER);
        let terminals = self.terminals.clone();
        tokio::spawn(async move {
            loop {
                match command_rx.recv().await {
                    Ok(Some(command)) => terminals.handle_command(session_id, command),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    // Adoption condition 2: one undecodable command is
                    // skipped; the channel survives.
                    Err(err) => eprintln!(
                        "horizon-sessiond: skipping an undecodable terminal command: {err}"
                    ),
                }
            }
        });

        TerminalAttachment {
            updates: update_rx,
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
        tokio::spawn(async move {
            while let Some(event) = local_events.recv().await {
                if event_tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        let (command_tx, mut command_rx) = rch::mpsc::channel::<Command, WireCodec>(CHANNEL_BUFFER);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            loop {
                match command_rx.recv().await {
                    Ok(Some(command)) => connection.route_command(session_id, command),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    Err(err) => {
                        eprintln!("horizon-sessiond: skipping an undecodable agent command: {err}")
                    }
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
            eprintln!("horizon-sessiond: rejecting hello from {}: {reason}", client.binary_id);
            return Err(reason);
        };

        // Host-tool requests: sessions push into the connection-global
        // local bridge; this pump forwards them to the client.
        let (request_tx, request_rx) =
            rch::mpsc::channel::<HostToolRequest, WireCodec>(CHANNEL_BUFFER);
        let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
        self.connection.connect_host_tools(local_tx);
        tokio::spawn(async move {
            while let Some(request) = local_rx.recv().await {
                if request_tx.send(request).await.is_err() {
                    break;
                }
            }
        });

        // Host-tool responses: routed to whichever session thread blocks
        // on the matching request id.
        let (response_tx, mut response_rx) =
            rch::mpsc::channel::<HostToolResponse, WireCodec>(CHANNEL_BUFFER);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            loop {
                match response_rx.recv().await {
                    Ok(Some(response)) => connection.handle_host_tool_response(response),
                    Ok(None) => break,
                    Err(err) if err.is_final() => break,
                    Err(err) => eprintln!(
                        "horizon-sessiond: skipping an undecodable host-tool response: {err}"
                    ),
                }
            }
        });

        // Startup skipped-lines diagnostics: at most one message, after
        // the resume finishes — never blocking hello's own reply.
        let (skipped_tx, skipped_rx) = rch::mpsc::channel::<String, WireCodec>(1);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            if let Some(summary) = connection.skipped_lines_summary() {
                let _ = skipped_tx.send(summary).await;
            }
        });

        Ok(HubHello {
            negotiated,
            binary_id: self.binary_id.to_string(),
            host_tools: request_rx,
            host_tool_responses: response_tx,
            skipped_lines: skipped_rx,
        })
    }

    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError> {
        Ok(self.terminals.list())
    }

    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError> {
        // Subscribe before spawning so the session's very first updates
        // (and the seeding snapshot, once the core emits one) are never
        // lost — the JSONL flow's `mark_attached`-before-create, made
        // structural.
        let local_updates = self.terminals.subscribe(session_id);
        // `TerminalHost::create` blocks (bounded spawn attempts with a
        // watchdog timeout each — see its doc comment on the suspected
        // portable-pty fork hazard); `serve(true)` runs this call on its
        // own task, and `spawn_blocking` keeps that task off the async
        // workers.
        let terminals = self.terminals.clone();
        let spawned =
            tokio::task::spawn_blocking(move || terminals.create(session_id, spec)).await;
        match spawned {
            Ok(Ok(())) => Ok(self.terminal_attachment(session_id, local_updates)),
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
        if !self.terminals.has_session(session_id) {
            return Err(HubError::TerminalNotFound);
        }
        let local_updates = self.terminals.subscribe(session_id);
        Ok(self.terminal_attachment(session_id, local_updates))
    }

    /// Readiness-gated exactly as the JSONL `session_list` was (bind-first
    /// fix in `main`): a client connecting while the startup resume is
    /// still running must not see a partial view.
    async fn list_agents(&self) -> Result<Vec<SessionSummary>, HubError> {
        self.connection.wait_until_resume_ready().await;
        Ok(self.connection.session_list())
    }

    /// Readiness-gated like `list_agents` for a different reason: the
    /// session's persistence choice is decided once at spawn time, and a
    /// spawn racing `set_writer` would silently run without persistence
    /// for its whole lifetime (see the old `Control::SessionNew` arm's
    /// comment, preserved by this gate).
    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError> {
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
        self.connection.wait_until_resume_ready().await;
        let local_events = self.connection.subscribe_agent(session_id);
        for event in self.connection.replay_events(session_id).await {
            self.connection
                .send_session_event(session_id, AgentWireEvent::Event(event));
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
