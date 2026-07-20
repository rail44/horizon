//! Horizon's single eager client runtime for the shared session daemon —
//! since v10, a remoc `SessionHub` client (`docs/remoc-adoption-design.md`
//! §2). The public shape is unchanged from the JSONL era: [`SessiondHandle`]
//! is a sync API, started eagerly and non-blocking, backed by one dedicated
//! current-thread tokio runtime on a background OS thread; internally the
//! raw-envelope FIFO and the `Routes` correlation maps became typed
//! [`connection::Op`]s (rtc calls return futures, so replies ride their own
//! channels) and per-attachment channel bridges. The sync-world ⇄ tokio
//! boundary did not move.

mod connection;
mod routing;

use std::path::Path;
use std::sync::Arc;

use connection::Op;
use crossbeam_channel::{unbounded, Receiver, Sender};
use horizon_agent::contract::{self, Command, ProviderEvent};
use horizon_agent::wire::{self, HostToolRequest, HostToolResponse};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use routing::Routes;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct SessiondHandle {
    ops: tokio::sync::mpsc::UnboundedSender<Op>,
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
    ops: tokio::sync::mpsc::WeakUnboundedSender<Op>,
}

/// A live view of `WorkspaceShell::sessiond`, threaded into panes that can
/// outlive a single runtime instance -- currently just the theme settings
/// view (`src/theme_settings/mod.rs`). A `SessiondHandle` cloned once at
/// pane-construction time goes stale across `Reload Session Runtime`:
/// `WorkspaceShell::sessiond` is `None` from the moment the old handle is
/// taken until the async drain finishes and a fresh one lands, and
/// `WorkspaceShell::reconcile` never recreates a pane view that already
/// exists in `self.panes` -- so a pane whose view happens to be
/// (re)constructed inside that window (its underlying model pane survives
/// the reload; only the view cache is cleared) would otherwise capture
/// `None` forever. This slot is cloned cheaply (an `Rc`) and always
/// reflects whatever `WorkspaceShell` currently holds, as long as it's kept
/// in sync at every `self.sessiond` write site.
#[derive(Clone, Default)]
pub(crate) struct SessiondSlot(std::rc::Rc<std::cell::RefCell<Option<SessiondHandle>>>);

impl SessiondSlot {
    pub(crate) fn new(handle: Option<SessiondHandle>) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(handle)))
    }

    pub(crate) fn get(&self) -> Option<SessiondHandle> {
        self.0.borrow().clone()
    }

    pub(crate) fn set(&self, handle: Option<SessiondHandle>) {
        *self.0.borrow_mut() = handle;
    }
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
        if let Some(ops) = self.ops.upgrade() {
            let _ = ops.send(Op::HostToolResponse(response));
        }
    }
}

impl SessiondHandle {
    pub(crate) fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.routes, &other.routes)
    }

    /// Starts the one process-wide socket runtime and returns before the
    /// connection (or the `hello` negotiation) completes. Typed requests
    /// enqueue onto the op queue meanwhile and are served in order once
    /// the hub is live.
    pub(crate) fn start(
        socket_path: &Path,
        control_socket: &Path,
    ) -> (
        Self,
        Receiver<HostToolRequest>,
        Receiver<(contract::SessionId, wire::WorkspaceRootResolved)>,
    ) {
        let (handle, host_tools, workspace_roots, ops) = Self::parts();
        connection::spawn(
            socket_path.to_path_buf(),
            control_socket.to_path_buf(),
            ops,
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
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (ops_tx, ops_rx) = tokio::sync::mpsc::unbounded_channel();
        let (host_tool_tx, host_tool_rx) = unbounded();
        let (workspace_root_tx, workspace_root_rx) = unbounded();
        let routes = Arc::new(Routes::new(host_tool_tx, workspace_root_tx));
        let control = Arc::new(connection::RuntimeControl::new());
        let lifetime = Arc::new(RuntimeLifetime {
            control: control.clone(),
        });
        (
            Self {
                ops: ops_tx,
                routes,
                control,
                _lifetime: lifetime,
            },
            host_tool_rx,
            workspace_root_rx,
            ops_rx,
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
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        let (handle, host_tools, workspace_roots, ops) = Self::parts();
        connection::spawn_test_stream(stream, ops, handle.routes.clone(), handle.control.clone());
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
        let (handle, commands) = self.register_agent(session_id);
        let _ = self.ops.send(Op::NewAgent {
            new: wire::SessionNew {
                session_id,
                provider_id,
                role_id,
                workspace_root,
                spawn_source_session_id,
                isolate,
            },
            commands,
        });
        handle
    }

    pub(crate) fn attach_session(&self, session_id: contract::SessionId) -> AgentSessionHandle {
        let (handle, commands) = self.register_agent(session_id);
        let _ = self.ops.send(Op::AttachAgent {
            session_id,
            commands,
        });
        handle
    }

    /// Registers the pane-facing channels for one agent session and starts
    /// the bridge thread that carries its commands from the sync world
    /// into the runtime (crossbeam → tokio unbounded; the runtime pumps
    /// the tokio half into the attachment's remote channel once the rtc
    /// call returns).
    fn register_agent(
        &self,
        session_id: contract::SessionId,
    ) -> (
        AgentSessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) {
        let (command_tx, command_rx) = unbounded::<Command>();
        let (event_tx, event_rx) = unbounded::<ProviderEvent>();
        self.routes.register_agent(session_id, event_tx);

        let (bridge_tx, bridge_rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                if bridge_tx.send(command).is_err() {
                    break;
                }
            }
        });
        (
            AgentSessionHandle {
                inner: contract::SessionHandle::new(command_tx, event_rx),
                session_id,
                routes: self.routes.clone(),
            },
            bridge_rx,
        )
    }

    pub(crate) fn start_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> TerminalSessionHandle {
        let (handle, commands) = self.register_terminal(session_id);
        let _ = self.ops.send(Op::CreateTerminal {
            session_id,
            spec: Box::new(spec),
            commands,
        });
        handle
    }

    /// The terminal twin of [`Self::register_agent`]. The bridge's sending
    /// half is also registered with [`Routes`] so a broadcast
    /// ([`Self::broadcast_terminal_color_scheme`]) can inject commands
    /// without a pane's handle.
    fn register_terminal(
        &self,
        session_id: Uuid,
    ) -> (
        TerminalSessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<TerminalCommand>,
    ) {
        let (command_tx, command_rx) = unbounded::<TerminalCommand>();
        let (update_tx, update_rx) = unbounded::<TerminalUpdate>();
        let (bridge_tx, bridge_rx) = tokio::sync::mpsc::unbounded_channel();
        self.routes
            .register_terminal(session_id, update_tx, bridge_tx.clone());

        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                if bridge_tx.send(command).is_err() {
                    break;
                }
            }
        });

        (
            TerminalSessionHandle {
                commands: command_tx,
                updates: update_rx,
                session_id,
                routes: self.routes.clone(),
            },
            bridge_rx,
        )
    }

    /// Re-pushes a live theme apply's color scheme
    /// (`theme::terminal_color_scheme()`) to every terminal session this
    /// client currently routes updates for, so each one's `TerminalCore`
    /// re-resolves OSC 10/11/12 query replies against it instead of its
    /// spawn-time snapshot (`TerminalCommand::SetColorScheme`'s doc
    /// comment). Fire-and-forget, same as every other per-session command
    /// send -- a session that has since exited just has its command
    /// dropped.
    pub(crate) fn broadcast_terminal_color_scheme(
        &self,
        scheme: horizon_terminal_core::TerminalColorScheme,
    ) {
        self.routes
            .broadcast_terminal_command(TerminalCommand::SetColorScheme(scheme));
    }

    pub(crate) fn terminal_list(&self) -> Result<Vec<TerminalSummary>, String> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        if self.ops.send(Op::TerminalList { reply: reply_tx }).is_err() {
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
            let (handle, commands) = self.register_terminal(session_id);
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            let sent = self
                .ops
                .send(Op::AttachTerminal {
                    session_id,
                    commands,
                    reply: reply_tx,
                })
                .is_ok();
            if sent {
                pending.push((session_id, handle, reply_rx));
            }
        }

        pending
            .into_iter()
            .filter_map(|(session_id, handle, reply)| match reply.recv() {
                // Only an explicit successful attach may claim the session
                // -- not-found (or a failed call) drops the handle, whose
                // Drop unregisters the routes it claimed.
                Ok(true) => Some((session_id, handle)),
                Ok(false) | Err(_) => None,
            })
            .collect()
    }

    pub(crate) fn responder(&self) -> SessiondResponder {
        SessiondResponder {
            ops: self.ops.downgrade(),
        }
    }

    pub(crate) fn session_list(&self) -> Result<Vec<wire::SessionSummary>, String> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        if self.ops.send(Op::SessionList { reply: reply_tx }).is_err() {
            return Err("session runtime stopped before the agent list was sent".to_string());
        }
        reply_rx
            .recv()
            .map_err(|_| "session runtime stopped before the agent list completed".to_string())?
    }

    fn drain(&self) {
        let _ = self.ops.send(Op::Drain);
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
