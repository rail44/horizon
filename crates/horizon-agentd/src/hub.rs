//! The daemon's [`SessionHub`] implementation — the v10 replacement for
//! `main`'s JSONL kind-dispatch loop (`docs/remoc-adoption-design.md` §2).
//! One [`Hub`] is built per accepted connection and served via
//! `SessionHubServerShared` with per-call task spawning, so a slow call
//! (a replay) never blocks the others; the process-lifetime state stays
//! where it always was ([`AgentdState`]), reached through the same
//! `Connection` seam.
//!
//! Agent domain only since v17: terminal hosting moved to
//! `horizon-terminald`'s own hub (`docs/terminald-split-design.md`), so
//! nothing here spawns, owns, or kills a PTY -- which is exactly what makes
//! [`SessionHub::drain`] (and therefore `Reload Agent Runtime`) safe to
//! run as often as an agent-side rebuild demands.
//!
//! Bridging pattern, used by every attachment: the session side of the
//! daemon is synchronous (std threads, crossbeam channels), so each remote
//! channel gets a local unbounded tokio channel as its sync-sendable half,
//! and a small async pump task that drains it into the remote `rch`
//! sender. The receive side is not written here at all — it is
//! [`horizon_wire::receive_pump`], the same loop terminald drives its
//! inbound channels with, so adoption condition 2's skip-vs-fatal boundary
//! has one definition rather than one per runtime. The *send* pumps stay
//! local: a send error latches on the local sender, so they end the channel
//! instead of skipping.

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    agent_version_range, AgentAttachment, AgentWireEvent, HostToolRequest, HostToolResponse,
    HubHello, SessionHub, SessionNew, SessionSummary,
};
use horizon_wire::{
    receive_pump, ClientHello, HelloGate, HubError, WireCodec, CHANNEL_BUFFER,
    COMMAND_MAX_ITEM_BYTES, CONTROL_MAX_ITEM_BYTES, TOOL_IO_MAX_ITEM_BYTES,
};
use remoc::rch;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::session::Connection;
use crate::DAEMON_NAME;

pub(crate) struct Hub {
    connection: Connection,
    binary_id: &'static str,
    /// Whether this connection's `hello` has completed successfully — the
    /// enforcement half of "`hello` is the first call on every connection"
    /// (§3), shared with `horizon-terminald`'s hub so the invariant has one
    /// definition.
    hello: HelloGate,
}

impl Hub {
    pub(crate) fn new(connection: Connection, binary_id: &'static str) -> Self {
        Self {
            connection,
            binary_id,
            hello: HelloGate::new(),
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
                    eprintln!("horizon-agentd: closing an agent event attachment: {err}");
                    break;
                }
            }
        });

        let (mut command_tx, command_rx) = rch::mpsc::channel::<Command, WireCodec>(CHANNEL_BUFFER);
        command_tx.set_max_item_size(COMMAND_MAX_ITEM_BYTES);
        let connection = self.connection.clone();
        tokio::spawn(receive_pump(
            command_rx,
            "horizon-agentd agent commands",
            move |command| connection.route_command(session_id, command),
        ));

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
        let negotiated =
            horizon_wire::negotiate_hello(agent_version_range(), &client, DAEMON_NAME)?;

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
                    eprintln!("horizon-agentd: closing the host-tool request channel: {err}");
                    break;
                }
            }
        });

        // Host-tool responses: routed to whichever session thread blocks
        // on the matching request id.
        let (mut response_tx, response_rx) =
            rch::mpsc::channel::<HostToolResponse, WireCodec>(CHANNEL_BUFFER);
        response_tx.set_max_item_size(TOOL_IO_MAX_ITEM_BYTES);
        let connection = self.connection.clone();
        tokio::spawn(receive_pump(
            response_rx,
            "horizon-agentd host-tool responses",
            move |response| connection.handle_host_tool_response(response),
        ));

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

        self.hello.mark_completed();
        Ok(HubHello {
            negotiated,
            binary_id: self.binary_id.to_string(),
            host_tools: request_rx,
            host_tool_responses: response_tx,
            skipped_lines: skipped_rx,
        })
    }

    /// Readiness-gated exactly as the JSONL `session_list` was (bind-first
    /// fix in `main`): a client connecting while the startup resume is
    /// still running must not see a partial view.
    async fn list_agents(&self) -> Result<Vec<SessionSummary>, HubError> {
        self.hello.require()?;
        self.connection.wait_until_resume_ready().await;
        Ok(self.connection.session_list())
    }

    /// Readiness-gated like `list_agents` for a different reason: the
    /// session's persistence choice is decided once at spawn time, and a
    /// spawn racing `set_writer` would silently run without persistence
    /// for its whole lifetime (see the old `Control::SessionNew` arm's
    /// comment, preserved by this gate).
    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError> {
        self.hello.require()?;
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
        self.hello.require()?;
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
                "horizon-agentd: withheld {skipped_unknown} unknown event(s) from \
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

    /// Since v17 this kills nothing the user is looking at: every PTY lives
    /// in `horizon-terminald`, so a drain here flushes the event log and
    /// exits, and the terminals -- including whatever interactive CLI is
    /// running in them -- carry on untouched
    /// (`docs/terminald-split-design.md` decision 2).
    async fn drain(&self) -> Result<(), HubError> {
        flush_event_log_before_exit(self.connection.writer());
        eprintln!("horizon-agentd: drained, exiting");
        std::process::exit(0);
    }

    /// Re-reads `[provider]` and rebuilds the registry in place -- see
    /// [`crate::session::AgentdState::reload_provider_config`]. A config
    /// parse error leaves the previous registry in place and is logged
    /// daemon-side (the call still succeeds from the client's view: the
    /// outcome it cares about -- "no respawn needed" -- holds either way,
    /// and a failure here is a race the UI's own `horizon_config::reload`
    /// already ruled out for the same file).
    async fn reload_provider_config(&self) -> Result<(), HubError> {
        self.hello.require()?;
        if let Err(error) = self.connection.reload_provider_config() {
            eprintln!(
                "horizon-agentd: provider config reload failed, keeping the previous config: {error}"
            );
        }
        Ok(())
    }
}

/// Blocks until every event-log record enqueued so far has actually been
/// written and flushed to disk (see [`WriterHandle::flush`]'s doc comment),
/// then returns. An `Appender::append_provider_events` call only enqueues
/// onto the writer's own background thread; forwarding the resulting event
/// to a connected client happens after that same enqueue, not after it
/// becomes durable. Without this, a client observing a session's latest
/// event over the wire and then shutting the daemon down could still race
/// the writer's thread and lose it.
///
/// **Every exit this process can intercept calls this**, and there are
/// exactly two: [`SessionHub::drain`] (right before
/// `std::process::exit(0)`) and [`crate::run`]'s SIGTERM arm — a plain
/// `kill`, which is what an operator is told to use to stop this daemon by
/// hand, and which used to unlink the socket and return without flushing.
/// The durability boundary is therefore only what no handler can run
/// ahead of: SIGKILL, and a crash. Those lose whatever was still queued on
/// the writer's channel and can leave a torn final line, which
/// `event_log::read` tolerates (`ReadReport::ignored_partial_line`).
///
/// A blocking call is safe on both paths despite running on an async task:
/// nothing else the runtime hosts still needs to make progress by the time
/// either caller reaches this.
pub(crate) fn flush_event_log_before_exit(writer: Option<WriterHandle>) {
    if let Some(writer) = writer {
        if let Err(error) = writer.flush() {
            eprintln!("horizon-agentd: failed to flush event log before draining: {error}");
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
    use crate::session::AgentdState;
    use horizon_agent::config::AgentConfig;
    use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
    use horizon_agent::registry::ProviderRegistry;
    use horizon_agent::wire::agent_client_hello;
    use horizon_wire::VersionRange;
    use std::sync::Arc;

    fn test_hub() -> Hub {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        // No `spawn_resume_task` here, so nothing would ever open the
        // readiness gate every post-hello agent method blocks on
        // (`wait_until_resume_ready`) -- open it directly, or a test that
        // calls `list_agents` after a successful hello hangs forever
        // instead of failing.
        state.mark_resume_ready();
        Hub::new(Connection::new(state), "test-agentd")
    }

    /// The hello gate (review item): a method called before `hello` — or
    /// after a *rejected* hello — is refused with `HelloRequired`; a
    /// successful negotiation opens the gate. (`drain` is deliberately
    /// exempt: it is the version-stable recovery surface a rejected
    /// client legitimately calls — enforced by it taking no
    /// `hello.require()`, which this test cannot exercise directly since
    /// `drain` exits the process.)
    #[tokio::test]
    async fn non_hello_methods_are_refused_until_hello_succeeds() {
        let hub = test_hub();

        // Before any hello.
        assert!(matches!(
            hub.list_agents().await,
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
            hub.list_agents().await,
            Err(HubError::HelloRequired)
        ));
        assert!(matches!(
            hub.attach_agent(SessionId::new()).await,
            Err(HubError::HelloRequired)
        ));

        // A successful negotiation opens it.
        hub.hello(agent_client_hello("test-client"))
            .await
            .expect("a matching range must negotiate");
        assert_eq!(hub.list_agents().await.unwrap(), Vec::new());
    }

    /// `reload_provider_config` re-reads the config file at the state's
    /// `config_path` and rebuilds the registry in place, so the *next* session
    /// sees the new model. Proven on both halves of the swap: the registry's
    /// `resolved_model` (which reads the rig provider's rebuilt config) and
    /// `agent_config.rig.model` (the judge's base-URL source). `OPENAI_API_KEY`
    /// is set only to flip `openai_enabled` on so `resolved_model` reports the
    /// model instead of `None` -- `resolved_model` makes no network call, and
    /// nextest's per-test process isolation means this `set_var` cannot race
    /// another test (the `config` module's own env-mutation warning is about
    /// `cargo test`'s in-process parallelism, which the gate does not use).
    #[tokio::test]
    async fn reload_provider_config_rebuilds_the_registry_from_the_config_file() {
        std::env::set_var("OPENAI_API_KEY", "test-only");
        std::env::remove_var("HORIZON_RIG_MODEL");

        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[provider]\nmodel = \"before-model\"\n").unwrap();

        let agent_config =
            AgentConfig::from_env_and_provider(Some("before-model".to_string()), None);
        let providers = ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        );
        let state = Arc::new(AgentdState::new(
            providers,
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            Some(config.clone()),
            Vec::new(),
        ));
        state.mark_resume_ready();
        let hub = Hub::new(Connection::new(state.clone()), "test-agentd");
        hub.hello(agent_client_hello("test-client"))
            .await
            .expect("hello");

        let provider_id = state.providers.lock().unwrap().default_provider_id();
        assert_eq!(
            state
                .providers
                .lock()
                .unwrap()
                .resolved_model(&provider_id, None),
            Some("before-model".to_string()),
        );

        std::fs::write(&config, "[provider]\nmodel = \"after-model\"\n").unwrap();
        hub.reload_provider_config().await.expect("reload");

        assert_eq!(
            state
                .providers
                .lock()
                .unwrap()
                .resolved_model(&provider_id, None),
            Some("after-model".to_string()),
        );
        assert_eq!(state.agent_config.lock().unwrap().rig.model, "after-model");
    }
}
