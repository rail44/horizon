//! Horizon's eager clients for the per-view-kind runtimes, **two of
//! them**: [`AgentAcpHandle`] speaks ACP v2 to `horizon-agent-acp`, its
//! child, over stdio (the agent runtime, `docs/agent-stdio-child-design.md`)
//! and [`TerminaldHandle`] remoc to `horizon-terminald` over its socket (the
//! terminal runtime, `docs/remoc-adoption-design.md` §2), each with its own
//! connection, op queue, and `RuntimeControl`
//! (`docs/terminald-split-design.md`).
//!
//! That separation is the whole point: reloading one runtime drains and
//! respawns exactly one daemon and leaves the other's sessions — and, for
//! terminals, the interactive processes running inside them — untouched. It
//! also means a failure is domain-scoped: a dead agent runtime fans errors
//! only into agent transcripts, because the two connections keep separate
//! route tables (`routing::AgentRoutes` / `routing::TerminalRoutes`).
//!
//! Both handles keep the shape the JSONL era established: a sync API,
//! started eagerly and non-blocking, backed by one dedicated current-thread
//! tokio runtime on a background OS thread. The sync-world ⇄ tokio boundary
//! did not move.
//!
//! [`notify`] is this layer's view-facing half that no UI framework
//! shapes: the decision of when a burst of updates is worth a repaint. The
//! half that is a framework's own (bridging an attachment's channels into
//! its tasks) stays with each shell.
//!
//! [`agent_model`] is the agent session as the routes carry it and as a
//! pane folds it: the events, and the frame they fold into.

mod agent;
pub mod agent_display;
pub mod agent_model;
pub mod agent_view;
mod attachment;
mod common;
mod connection;
pub mod control_invoke;
mod notify;
mod request;
mod routing;
pub mod terminal_glyphs;
pub mod terminal_links;
pub use agent::SessionSummary;
pub use attachment::{AgentCommand, AgentUpdate, AttachmentState};
pub use routing::RuntimeNews;
pub use terminal::HandOver;
mod terminal;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use horizon_wire::SessionId;
use routing::{AgentRoutes, RouteKey, TerminalRoutes};
use uuid::Uuid;

use common::RuntimeControl;
pub use common::Timeouts;
use request::request;

pub use notify::{NotifyCoalescer, NotifyDecision};

/// The sync world's overall budget for one queued request: the op may
/// legitimately wait out connection establishment (retries with backoff)
/// plus the runtime's own per-call deadline (`common::OP_TIMEOUT`, 30 s), so
/// this sits above both. The old JSONL shape blocked forever here; a bounded
/// wait turns a wedged runtime into an error the caller can render instead
/// of a UI hang.
const SYNC_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The agent runtime's binary, found beside this executable (or named by
/// `HORIZON_AGENT_ACP_BINARY`, `horizon_wire::spawn::resolve_daemon_binary`).
pub const AGENT_BINARY_NAME: &str = "horizon-agent-acp";

/// The ACP agent the runtime starts as its child: a program and its
/// arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProgram {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl AgentProgram {
    /// Horizon's own agent, [`AGENT_BINARY_NAME`].
    pub fn horizon() -> Self {
        Self {
            program: horizon_wire::spawn::resolve_daemon_binary(AGENT_BINARY_NAME),
            args: Vec::new(),
        }
    }
}

/// The client of the agent runtime (`horizon-agent-acp`, this process's
/// child): agent sessions and the connection-global host-tool exchange.
#[derive(Clone)]
pub struct AgentAcpHandle {
    ops: tokio::sync::mpsc::UnboundedSender<agent::Op>,
    routes: Arc<AgentRoutes>,
    control: Arc<RuntimeControl>,
    _lifetime: Arc<RuntimeLifetime>,
}

/// The client runtime for `horizon-terminald`: terminal sessions only.
#[derive(Clone)]
pub struct TerminaldHandle {
    ops: tokio::sync::mpsc::UnboundedSender<terminal::Op>,
    routes: Arc<TerminalRoutes>,
    control: Arc<RuntimeControl>,
    _lifetime: Arc<RuntimeLifetime>,
}

struct RuntimeLifetime {
    control: Arc<RuntimeControl>,
}

impl Drop for RuntimeLifetime {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

/// A live view of `WorkspaceShell::terminald`, threaded into panes that can
/// outlive a single runtime instance -- currently just the theme settings
/// view (`src/theme_settings/mod.rs`), which re-pushes the terminal color
/// scheme on a live apply. A `TerminaldHandle` cloned once at
/// pane-construction time goes stale across `Reload Terminal Runtime`:
/// `WorkspaceShell::terminald` is `None` from the moment the old handle is
/// taken until the async drain finishes and a fresh one lands, and
/// `WorkspaceShell::reconcile` never recreates a pane view that already
/// exists in `self.panes` -- so a pane whose view happens to be
/// (re)constructed inside that window (its underlying model pane survives
/// the reload; only the view cache is cleared) would otherwise capture
/// `None` forever. This slot is cloned cheaply (an `Rc`) and always
/// reflects whatever `WorkspaceShell` currently holds, as long as it's kept
/// in sync at every `self.terminald` write site.
#[derive(Clone, Default)]
pub struct TerminaldSlot(std::rc::Rc<std::cell::RefCell<Option<TerminaldHandle>>>);

impl TerminaldSlot {
    pub fn new(handle: Option<TerminaldHandle>) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(handle)))
    }

    pub fn get(&self) -> Option<TerminaldHandle> {
        self.0.borrow().clone()
    }

    pub fn set(&self, handle: Option<TerminaldHandle>) {
        *self.0.borrow_mut() = handle;
    }
}

pub struct AgentSessionHandle {
    commands: Sender<AgentCommand>,
    events: Option<tokio::sync::mpsc::Receiver<AgentUpdate>>,
    route: RouteKey<SessionId>,
    routes: Arc<AgentRoutes>,
}

pub struct TerminalSessionHandle {
    commands: Sender<TerminalCommand>,
    /// What the daemon sends: the terminal's state and the bytes after it,
    /// and what the session says of itself (title/bell/clipboard/exit/
    /// error).
    events: Receiver<TerminalUpdate>,
    route: RouteKey<Uuid>,
    routes: Arc<TerminalRoutes>,
}

impl AgentSessionHandle {
    pub fn sender(&self) -> Sender<AgentCommand> {
        self.commands.clone()
    }

    pub fn take_events(&mut self) -> tokio::sync::mpsc::Receiver<AgentUpdate> {
        self.events
            .take()
            .expect("attachment has one event consumer")
    }

    /// The daemon-side session id, for connection-level requests that
    /// address the session by id (`session/set_config_option` -- the model
    /// picker's confirm path).
    pub fn session_id(&self) -> SessionId {
        self.routes.agent_of(self.route.session_id())
    }
}

impl Drop for AgentSessionHandle {
    fn drop(&mut self) {
        self.routes.detach_agent(self.route);
    }
}

impl TerminalSessionHandle {
    pub fn sender(&self) -> Sender<TerminalCommand> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<TerminalUpdate> {
        self.events.clone()
    }

    /// Has `heard` told whenever an event is put on [`events`](Self::events),
    /// from the thread it arrived on.
    pub fn hear_events(&self, heard: impl Fn() + Send + Sync + 'static) {
        (self.routes).hear_terminal_events(self.route, Arc::new(heard));
    }
}

impl Drop for TerminalSessionHandle {
    fn drop(&mut self) {
        self.routes.unregister_terminal(self.route);
    }
}

impl AgentAcpHandle {
    pub fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.routes, &other.routes)
    }

    /// Starts the agent runtime (`horizon-agent-acp` as this process's
    /// child) and returns before it has started or answered `initialize`.
    /// Typed requests enqueue onto the op queue meanwhile; once it answers,
    /// each op is *dispatched* in queue order but *executed* on its own
    /// task, so there is no cross-op completion-order guarantee -- a slow
    /// `session/new` does not delay a `session/list` behind it, and two lists
    /// may complete in either order. Per-session ordering is carried by
    /// each attachment's own channels, not the op queue.
    ///
    /// The child's own children reach this Horizon through
    /// `control_socket`; its standard error is appended to `log`.
    pub fn start(
        program: AgentProgram,
        control_socket: &Path,
        log: &Path,
    ) -> (Self, Receiver<RuntimeNews>) {
        Self::start_within(program, control_socket, log, Timeouts::default())
    }

    /// [`Self::start`], waiting on the runtime for `timeouts` in place of
    /// its own: a test's shorter ones.
    pub fn start_within(
        program: AgentProgram,
        control_socket: &Path,
        log: &Path,
        timeouts: Timeouts,
    ) -> (Self, Receiver<RuntimeNews>) {
        let (handle, workspace_roots, ops) = Self::parts(timeouts);
        agent::spawn(
            program,
            control_socket.to_path_buf(),
            log.to_path_buf(),
            ops,
            handle.routes.clone(),
            handle.control.clone(),
        );
        (handle, workspace_roots)
    }

    fn parts(
        timeouts: Timeouts,
    ) -> (
        Self,
        Receiver<RuntimeNews>,
        tokio::sync::mpsc::UnboundedReceiver<agent::Op>,
    ) {
        let (ops_tx, ops_rx) = tokio::sync::mpsc::unbounded_channel();
        let (news_tx, news_rx) = unbounded();
        let routes = Arc::new(AgentRoutes::new(news_tx));
        let control = Arc::new(RuntimeControl::new(timeouts));
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
            news_rx,
            ops_rx,
        )
    }

    #[cfg(test)]
    fn start_on_stream<S>(stream: S) -> (Self, Receiver<RuntimeNews>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        let (handle, workspace_roots, ops) = Self::parts(Timeouts::default());
        agent::spawn_test_stream(stream, ops, handle.routes.clone(), handle.control.clone());
        (handle, workspace_roots)
    }

    /// `workspace_root` is computed shell-side (`WorkspaceShell::reconcile`)
    /// and recorded on the workspace model before this is called, so the
    /// model and the daemon spawn can never disagree; it travels as
    /// `session/new`'s `cwd`. `spawn_source_session_id` is the pane this
    /// spawn was invoked "from" (kind-agnostic -- may be a terminal or an
    /// agent session id; `docs/session-relationship-design.md` decision 3).
    /// Once the session enters a worktree, the daemon reports the worktree
    /// path back via `SessionInfoMeta::workspace_root` (see
    /// `WorkspaceShell::spawn_agent_resume`/`spawn_workspace_restore`).
    /// Starts a session on the agent's default model.
    pub fn start_session(
        &self,
        session_id: SessionId,
        workspace_root: Option<std::path::PathBuf>,
    ) -> AgentSessionHandle {
        let (handle, commands) = self.register_agent(session_id);
        let _ = self.ops.send(agent::Op::NewAgent {
            route: handle.route,
            new: Box::new(agent::SessionNew { workspace_root }),
            commands,
        });
        handle
    }

    pub fn attach_session(&self, session_id: SessionId) -> AgentSessionHandle {
        let (handle, commands) = self.register_agent(session_id);
        let _ = self.ops.send(agent::Op::AttachAgent {
            route: handle.route,
            commands,
        });
        handle
    }

    /// Registers the pane-facing channels for one agent session and starts
    /// the bridge thread that carries its commands from the sync world
    /// into the runtime (crossbeam → tokio unbounded; the attachment task
    /// carries them out once the attachment is ready).
    fn register_agent(
        &self,
        session_id: SessionId,
    ) -> (
        AgentSessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<AgentCommand>,
    ) {
        let (command_tx, command_rx) = unbounded::<AgentCommand>();
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgentUpdate>(256);
        let route = self.routes.register_agent(session_id, event_tx);

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
                commands: command_tx,
                events: Some(event_rx),
                route,
                routes: self.routes.clone(),
            },
            bridge_rx,
        )
    }

    pub fn session_list(&self) -> Result<Vec<SessionSummary>, String> {
        request(&self.ops, |reply| agent::Op::SessionList { reply }).map_err(|error| {
            error.describe(
                "session runtime stopped before the agent list was sent",
                "the agent list did not complete in time",
                "session runtime stopped before the agent list completed",
            )
        })?
    }

    /// Call from a background task: sets the session's config option
    /// `config_id` to the entry `value` (the model picker's and the effort
    /// picker's confirm path). The reply carries the agent's refusal; on
    /// success the `config_option_update` on the attachment's event channel
    /// is the UI's confirmation.
    pub fn set_session_option(
        &self,
        session_id: SessionId,
        config_id: String,
        value: String,
    ) -> Result<(), String> {
        request(&self.ops, |reply| agent::Op::SetSessionOption {
            session_id,
            config_id,
            value,
            reply,
        })
        .map_err(|error| {
            error.describe(
                "session runtime stopped before the option was sent",
                "setting the option did not complete in time",
                "session runtime stopped before the option was set",
            )
        })?
    }

    /// Stops the runtime and waits for its child to end: its input is
    /// closed, and it flushes its session logs before it exits.
    pub fn stop_and_wait(&self) {
        self.control.cancel();
        self.control.wait_stopped();
    }
}

impl TerminaldHandle {
    pub fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.routes, &other.routes)
    }

    /// Starts the terminal-runtime connection. Same eager, non-blocking
    /// contract as [`AgentAcpHandle::start`] — a `create_terminal` queued
    /// before the daemon is even up is dispatched once it is.
    pub fn start(socket_path: &Path, control_socket: &Path) -> Self {
        let (handle, ops) = Self::parts();
        terminal::spawn(
            socket_path.to_path_buf(),
            control_socket.to_path_buf(),
            ops,
            handle.routes.clone(),
            handle.control.clone(),
        );
        handle
    }

    fn parts() -> (Self, tokio::sync::mpsc::UnboundedReceiver<terminal::Op>) {
        let (ops_tx, ops_rx) = tokio::sync::mpsc::unbounded_channel();
        let routes = Arc::new(TerminalRoutes::new());
        let control = Arc::new(RuntimeControl::new(Timeouts::default()));
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
            ops_rx,
        )
    }

    #[cfg(test)]
    fn start_on_stream<S>(stream: S) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        let (handle, ops) = Self::parts();
        terminal::spawn_test_stream(stream, ops, handle.routes.clone(), handle.control.clone());
        handle
    }

    pub fn start_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> TerminalSessionHandle {
        let (handle, commands) = self.register_terminal(session_id);
        let _ = self.ops.send(terminal::Op::CreateTerminal {
            route: handle.route,
            spec: Box::new(spec),
            commands,
        });
        handle
    }

    /// The terminal twin of [`AgentAcpHandle::register_agent`]. The bridge's
    /// sending half is also registered with [`TerminalRoutes`] so a
    /// broadcast ([`Self::broadcast_terminal_color_scheme`]) can inject
    /// commands without a pane's handle.
    fn register_terminal(
        &self,
        session_id: Uuid,
    ) -> (
        TerminalSessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<TerminalCommand>,
    ) {
        let (command_tx, command_rx) = unbounded::<TerminalCommand>();
        let (event_tx, event_rx) = unbounded::<TerminalUpdate>();
        let (bridge_tx, bridge_rx) = tokio::sync::mpsc::unbounded_channel();
        let route = (self.routes).register_terminal(session_id, event_tx, bridge_tx.clone());

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
                events: event_rx,
                route,
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
    pub fn broadcast_terminal_color_scheme(
        &self,
        scheme: horizon_terminal_core::TerminalColorScheme,
    ) {
        self.routes
            .broadcast_terminal_command(TerminalCommand::SetColorScheme(scheme));
    }

    pub fn terminal_list(&self) -> Result<Vec<TerminalSummary>, String> {
        request(&self.ops, |reply| terminal::Op::TerminalList { reply }).map_err(|error| {
            error.describe(
                "terminal runtime stopped before terminal list was sent",
                "the terminal list did not complete in time",
                "terminal runtime stopped before the terminal list completed",
            )
        })?
    }

    pub fn attach_terminals(&self, session_ids: Vec<Uuid>) -> Vec<(Uuid, TerminalSessionHandle)> {
        let mut pending = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let (handle, commands) = self.register_terminal(session_id);
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            let sent = self
                .ops
                .send(terminal::Op::AttachTerminal {
                    route: handle.route,
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
            .filter_map(|(session_id, handle, reply)| {
                match reply.recv_timeout(SYNC_REPLY_TIMEOUT) {
                    // Only an explicit successful attach may claim the
                    // session -- not-found (or a failed/timed-out call)
                    // drops the handle, whose Drop unregisters the routes
                    // it claimed.
                    Ok(true) => Some((session_id, handle)),
                    Ok(false) | Err(_) => None,
                }
            })
            .collect()
    }

    /// Asks the daemon to hand its terminals to its next image
    /// (`docs/terminald-handover-design.md`), and waits for what that came
    /// to. A runtime not connected has nothing to hand over.
    pub fn hand_over(&self) -> HandOver {
        if !self.control.is_established() {
            return HandOver::Refused("the terminal runtime is not connected".to_string());
        }
        let (reply, handed) = crossbeam_channel::bounded(1);
        if self.ops.send(terminal::Op::HandOver { reply }).is_err() {
            return HandOver::Refused("the terminal runtime has stopped".to_string());
        }
        handed.recv().unwrap_or_else(|_| {
            HandOver::Refused("the terminal runtime stopped during the handover".to_string())
        })
    }

    fn drain(&self) {
        let _ = self.ops.send(terminal::Op::Drain);
    }

    /// Drains the daemon — see [`AgentAcpHandle::begin_reload`] for the
    /// return contract. Draining terminald kills every PTY it hosts, so
    /// `Reload Terminal Runtime` calls this only for a daemon that cannot
    /// [`hand_over`](Self::hand_over).
    pub fn begin_reload(&self) -> bool {
        if self.control.is_established() {
            self.drain();
            true
        } else {
            self.control.cancel();
            false
        }
    }

    pub fn stop_and_wait(&self) {
        self.control.cancel();
        self.control.wait_stopped();
    }
}

/// Waits for a drained daemon's socket to stop accepting connections — the
/// observable completion signal for both daemons' drains (a drain exit
/// leaves the socket *file* behind, so file existence proves nothing).
pub fn wait_for_drain(socket_path: &Path) -> Result<(), String> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL: std::time::Duration = std::time::Duration::from_millis(50);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_err() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the daemon at {} did not drain within {:.1}s",
                socket_path.display(),
                TIMEOUT.as_secs_f64()
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests;
