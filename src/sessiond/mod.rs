//! Horizon's single eager client runtime for the shared session daemon.

mod connection;
mod routing;

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use horizon_agent::contract::{self, Command, ProviderEvent};
use horizon_agent::wire::{self, Control, Envelope, HostToolRequest, HostToolResponse};
use horizon_session_protocol::{Envelope as RawEnvelope, SessionControl};
use horizon_terminal_core::{
    encode_terminal_command, encode_terminal_control, TerminalAttachResult, TerminalCommand,
    TerminalControl, TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use routing::Routes;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct SessiondHandle {
    outgoing: tokio::sync::mpsc::UnboundedSender<RawEnvelope>,
    routes: Arc<Routes>,
    control: Arc<connection::RuntimeControl>,
    _lifetime: Arc<RuntimeLifetime>,
}

struct RuntimeLifetime {
    control: Arc<connection::RuntimeControl>,
}

impl Drop for RuntimeLifetime {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

pub(crate) struct SessiondResponder {
    outgoing: tokio::sync::mpsc::WeakUnboundedSender<RawEnvelope>,
}

pub(crate) struct AgentSessionHandle {
    inner: contract::SessionHandle,
    session_id: contract::SessionId,
    routes: Arc<Routes>,
}

pub(crate) struct TerminalSessionHandle {
    commands: Sender<TerminalCommand>,
    updates: Receiver<TerminalUpdate>,
    session_id: Uuid,
    routes: Arc<Routes>,
}

impl AgentSessionHandle {
    pub(crate) fn sender(&self) -> Sender<Command> {
        self.inner.sender()
    }

    pub(crate) fn events(&self) -> Receiver<ProviderEvent> {
        self.inner.events()
    }
}

impl Drop for AgentSessionHandle {
    fn drop(&mut self) {
        self.routes.unregister_agent(self.session_id);
    }
}

impl TerminalSessionHandle {
    pub(crate) fn sender(&self) -> Sender<TerminalCommand> {
        self.commands.clone()
    }

    pub(crate) fn updates(&self) -> Receiver<TerminalUpdate> {
        self.updates.clone()
    }
}

impl Drop for TerminalSessionHandle {
    fn drop(&mut self) {
        self.routes.unregister_terminal(self.session_id);
    }
}

impl SessiondResponder {
    pub(crate) fn respond_host_tool(&self, response: HostToolResponse) {
        let envelope = Envelope::control(Control::HostToolResponse(response));
        let Ok(raw) = wire::encode_envelope(&envelope) else {
            return;
        };
        if let Some(outgoing) = self.outgoing.upgrade() {
            let _ = outgoing.send(raw);
        }
    }
}

impl SessiondHandle {
    pub(crate) fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.routes, &other.routes)
    }

    /// Starts the one process-wide socket runtime and returns before connect
    /// or Hello completes. Typed requests enqueue onto one raw FIFO meanwhile.
    pub(crate) fn start(
        socket_path: &Path,
        control_socket: &Path,
    ) -> (
        Self,
        Receiver<HostToolRequest>,
        Receiver<(contract::SessionId, wire::WorkspaceRootResolved)>,
    ) {
        let (handle, host_tools, workspace_roots, outgoing) = Self::parts();
        let weak_outgoing = handle.outgoing.downgrade();
        connection::spawn(
            socket_path.to_path_buf(),
            control_socket.to_path_buf(),
            outgoing,
            weak_outgoing,
            handle.routes.clone(),
            handle.control.clone(),
        );
        (handle, host_tools, workspace_roots)
    }

    #[allow(clippy::type_complexity)]
    fn parts() -> (
        Self,
        Receiver<HostToolRequest>,
        Receiver<(contract::SessionId, wire::WorkspaceRootResolved)>,
        tokio::sync::mpsc::UnboundedReceiver<RawEnvelope>,
    ) {
        let (raw_tx, raw_rx) = tokio::sync::mpsc::unbounded_channel();
        let (host_tool_tx, host_tool_rx) = unbounded();
        let (workspace_root_tx, workspace_root_rx) = unbounded();
        let routes = Arc::new(Routes::new(host_tool_tx, workspace_root_tx));
        let control = Arc::new(connection::RuntimeControl::new());
        let lifetime = Arc::new(RuntimeLifetime {
            control: control.clone(),
        });
        (
            Self {
                outgoing: raw_tx,
                routes,
                control,
                _lifetime: lifetime,
            },
            host_tool_rx,
            workspace_root_rx,
            raw_rx,
        )
    }

    #[cfg(test)]
    fn start_on_stream<S>(
        stream: S,
    ) -> (
        Self,
        Receiver<HostToolRequest>,
        Receiver<(contract::SessionId, wire::WorkspaceRootResolved)>,
    )
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (handle, host_tools, workspace_roots, outgoing) = Self::parts();
        let weak_outgoing = handle.outgoing.downgrade();
        connection::spawn_test_stream(
            stream,
            outgoing,
            weak_outgoing,
            handle.routes.clone(),
            handle.control.clone(),
        );
        (handle, host_tools, workspace_roots)
    }

    /// `workspace_root` is computed shell-side (`WorkspaceShell::reconcile`)
    /// and recorded on the workspace model before this is called, so the
    /// model and the daemon spawn can never disagree -- see `wire::
    /// SessionNew::workspace_root`'s doc comment. `spawn_source_session_id`/
    /// `isolate` are `docs/session-relationship-design.md` decision 3's
    /// per-spawn knobs: the pane this spawn was invoked "from"
    /// (kind-agnostic -- may be a terminal or an agent session id) and
    /// whether `horizon-sessiond` should give this session its own git
    /// worktree derived from it. Both are resolved by the caller
    /// (origin-based default plus any explicit override -- see
    /// `workspace::session_lifecycle::PendingAgentSpawn`); this method just
    /// forwards whatever concrete choice it's given. Note that for an
    /// isolated spawn, `workspace_root` here is only the *pre-isolation*
    /// value -- the daemon overrides it with the worktree path it creates,
    /// reported back via `wire::SessionSummary::workspace_root` (see
    /// `WorkspaceShell::spawn_agent_resume`/`spawn_workspace_restore`).
    pub(crate) fn start_session(
        &self,
        session_id: contract::SessionId,
        provider_id: contract::ProviderId,
        role_id: Option<horizon_agent::roles::RoleId>,
        workspace_root: Option<std::path::PathBuf>,
        spawn_source_session_id: Option<contract::SessionId>,
        isolate: bool,
    ) -> AgentSessionHandle {
        let handle = self.register_agent(session_id);
        self.enqueue_agent(Envelope::control(Control::SessionNew(wire::SessionNew {
            session_id,
            provider_id,
            role_id,
            workspace_root,
            spawn_source_session_id,
            isolate,
        })));
        handle
    }

    pub(crate) fn attach_session(&self, session_id: contract::SessionId) -> AgentSessionHandle {
        let handle = self.register_agent(session_id);
        self.enqueue_agent(Envelope::control(Control::SessionLoad(wire::SessionLoad {
            session_id,
        })));
        handle
    }

    fn register_agent(&self, session_id: contract::SessionId) -> AgentSessionHandle {
        let (command_tx, command_rx) = unbounded::<Command>();
        let (event_tx, event_rx) = unbounded::<ProviderEvent>();
        self.routes.register_agent(session_id, event_tx);

        let outgoing = self.outgoing.clone();
        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let envelope = Envelope::command(session_id, command);
                let Ok(raw) = wire::encode_envelope(&envelope) else {
                    continue;
                };
                if outgoing.send(raw).is_err() {
                    break;
                }
            }
        });
        AgentSessionHandle {
            inner: contract::SessionHandle::new(command_tx, event_rx),
            session_id,
            routes: self.routes.clone(),
        }
    }

    pub(crate) fn start_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> TerminalSessionHandle {
        let handle = self.register_terminal(session_id);

        match encode_terminal_control(Some(session_id), &TerminalControl::Create(Box::new(spec))) {
            Ok(envelope) => {
                let _ = self.outgoing.send(envelope);
            }
            Err(error) => eprintln!("failed to encode terminal create: {error}"),
        }

        handle
    }

    fn register_terminal(&self, session_id: Uuid) -> TerminalSessionHandle {
        let (command_tx, command_rx) = unbounded::<TerminalCommand>();
        let (update_tx, update_rx) = unbounded::<TerminalUpdate>();
        self.routes.register_terminal(session_id, update_tx);

        let outgoing = self.outgoing.clone();
        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let Ok(envelope) = encode_terminal_command(session_id, &command) else {
                    continue;
                };
                if outgoing.send(envelope).is_err() {
                    break;
                }
            }
        });

        TerminalSessionHandle {
            commands: command_tx,
            updates: update_rx,
            session_id,
            routes: self.routes.clone(),
        }
    }

    /// Re-pushes a live theme apply's color scheme
    /// (`theme::terminal_color_scheme()`) to every terminal session this
    /// client currently routes updates for, so each one's `TerminalCore`
    /// re-resolves OSC 10/11/12 query replies against it instead of its
    /// spawn-time snapshot (`TerminalCommand::SetColorScheme`'s doc
    /// comment). Fire-and-forget, same as every other per-session command
    /// send here -- a session that has since exited just has its envelope
    /// dropped daemon-side.
    pub(crate) fn broadcast_terminal_color_scheme(
        &self,
        scheme: horizon_terminal_core::TerminalColorScheme,
    ) {
        for session_id in self.routes.terminal_session_ids() {
            let command = TerminalCommand::SetColorScheme(scheme);
            match encode_terminal_command(session_id, &command) {
                Ok(envelope) => {
                    let _ = self.outgoing.send(envelope);
                }
                Err(error) => eprintln!("failed to encode terminal color-scheme push: {error}"),
            }
        }
    }

    pub(crate) fn terminal_list(&self) -> Result<Vec<TerminalSummary>, String> {
        let request_id = Uuid::new_v4();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.routes.set_pending_terminal_list(request_id, reply_tx);
        let envelope = match encode_terminal_control(None, &TerminalControl::List { request_id }) {
            Ok(envelope) => envelope,
            Err(error) => {
                self.routes.cancel_pending_terminal_list(request_id);
                return Err(error.to_string());
            }
        };
        if self.outgoing.send(envelope).is_err() {
            self.routes.cancel_pending_terminal_list(request_id);
            return Err("session runtime stopped before terminal list was sent".to_string());
        }
        reply_rx
            .recv()
            .map_err(|_| "session runtime stopped before the terminal list completed".to_string())?
    }

    pub(crate) fn attach_terminals(
        &self,
        session_ids: Vec<Uuid>,
    ) -> Vec<(Uuid, TerminalSessionHandle)> {
        let mut pending = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let request_id = Uuid::new_v4();
            let handle = self.register_terminal(session_id);
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            self.routes
                .set_pending_terminal_attach(request_id, session_id, reply_tx);
            let control = TerminalControl::Attach { request_id };
            let sent = encode_terminal_control(Some(session_id), &control)
                .ok()
                .is_some_and(|envelope| self.outgoing.send(envelope).is_ok());
            if sent {
                pending.push((session_id, handle, reply_rx));
            } else {
                self.routes.cancel_pending_terminal_attach(request_id);
            }
        }

        pending
            .into_iter()
            .filter_map(|(session_id, handle, reply)| match reply.recv() {
                Ok(Ok(TerminalAttachResult::Attached)) => Some((session_id, handle)),
                Ok(Ok(TerminalAttachResult::NotFound)) | Ok(Err(_)) | Err(_) => None,
            })
            .collect()
    }

    pub(crate) fn responder(&self) -> SessiondResponder {
        SessiondResponder {
            outgoing: self.outgoing.downgrade(),
        }
    }

    pub(crate) fn session_list(&self) -> Result<Vec<wire::SessionSummary>, String> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.routes.set_pending_session_list(reply_tx);
        self.enqueue_agent(Envelope::control(Control::SessionList));
        reply_rx
            .recv()
            .map_err(|_| "session runtime stopped before the agent list completed".to_string())?
    }

    fn drain(&self) {
        if let Ok(envelope) = RawEnvelope::session_control(&SessionControl::Drain) {
            let _ = self.outgoing.send(envelope);
        }
    }

    pub(crate) fn begin_reload(&self) -> bool {
        if self.control.is_established() {
            self.drain();
            true
        } else {
            self.control.cancel();
            false
        }
    }

    pub(crate) fn stop_and_wait(&self) {
        self.control.cancel();
        self.control.wait_stopped();
    }

    fn enqueue_agent(&self, envelope: Envelope) {
        match wire::encode_envelope(&envelope) {
            Ok(raw) => {
                let _ = self.outgoing.send(raw);
            }
            Err(error) => eprintln!("failed to encode agent envelope: {error}"),
        }
    }
}

pub(crate) fn wait_for_drain(socket_path: &Path) -> Result<(), String> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL: std::time::Duration = std::time::Duration::from_millis(50);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_err() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "horizon-sessiond did not drain within {:.1}s",
                TIMEOUT.as_secs_f64()
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests;
