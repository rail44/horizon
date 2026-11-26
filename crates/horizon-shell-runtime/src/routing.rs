//! The client-side fan-out tables: which pane's channels receive a given
//! session's traffic. One table per daemon connection, each with its own
//! sticky failure, so a failure of one runtime is visible only to that
//! runtime's panes.
//!
//! The agent table routes the ACP connection's inbound traffic by session
//! id: `session/update`, the `_horizon/*` notifications, and
//! `session/request_permission` (with its responder) go to the running
//! attachment task of that session, if any.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::attachment::{AgentCommand, AgentUpdate, AttachmentState, Inbound};
use crate::agent_model::AgentEvent;
use agent_client_protocol::v2::Responder;
use agent_client_protocol::v2::schema as v2;
use crossbeam_channel::Sender;
use horizon_terminal_core::{TerminalCommand, TerminalUpdate};
use horizon_wire::SessionId;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

/// A pane's commands to its session, held by one attachment task at a
/// time.
pub(super) type Commands = Arc<tokio::sync::Mutex<UnboundedReceiver<AgentCommand>>>;

/// Identifies one local attachment, including replacements of the same session.
/// This identity never crosses the wire: late events and handle cleanup must
/// only affect the channels registered by their own attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RouteKey<I> {
    session_id: I,
    generation: Uuid,
}

impl<I: Copy> RouteKey<I> {
    fn new(session_id: I) -> Self {
        Self {
            session_id,
            generation: Uuid::new_v4(),
        }
    }

    pub(super) fn session_id(self) -> I {
        self.session_id
    }
}

/// What the agent runtime tells the shell outside any session, in the
/// order it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeNews {
    /// A session's working directory, as `session/list` has it after a
    /// `session_info_update`.
    Directory(SessionId, PathBuf),
    /// The session the shell started as `was` is `is` to the agent, which
    /// names its sessions.
    Named { was: SessionId, is: SessionId },
    /// The session the shell started as `was` did not start, and has no
    /// name.
    Unnamed { was: SessionId, error: String },
}

struct AgentRoute {
    key: RouteKey<SessionId>,
    events: tokio::sync::mpsc::Sender<AgentUpdate>,
    cancelled: tokio::sync::watch::Sender<bool>,
    /// The running attachment task's queue, once the op that opens the
    /// session has been dispatched.
    inbound: Option<tokio::sync::mpsc::UnboundedSender<Inbound>>,
    /// Whether the attachment has reached `Ready`.
    ready: bool,
    /// The pane's commands, once the session's opening op was dispatched:
    /// an attachment task holds them while it runs, and the next one on
    /// another connection takes them up.
    commands: Option<Commands>,
    /// The connection the attachment was on is gone: it is opened again on
    /// the next.
    reopen: bool,
}

impl Drop for AgentRoute {
    fn drop(&mut self) {
        self.cancelled.send_replace(true);
    }
}

/// The `horizon-agentd` connection's routes: per-agent-session channels
/// plus the two process-wide channels its connection-global traffic feeds.
pub(super) struct AgentRoutes {
    state: Mutex<AgentRouteState>,
    news: Sender<RuntimeNews>,
    /// The agent's name of each session the shell started, by the shell's
    /// own id: what the agent is asked by, and what it says back.
    names: Mutex<Names>,
    /// Each subagent's parent session, as `subagent_update` announced it:
    /// where a subagent's permission requests are shown.
    parents: Mutex<HashMap<SessionId, SessionId>>,
}

/// The two ways round between the shell's session ids and the agent's.
#[derive(Default)]
struct Names {
    agent: HashMap<SessionId, SessionId>,
    local: HashMap<SessionId, SessionId>,
}

struct AgentRouteState {
    agent: HashMap<SessionId, AgentRoute>,
    failure: Option<String>,
}

/// The `horizon-terminald` connection's routes: the three per-terminal
/// channels a pane holds, plus its own sticky failure.
pub(super) struct TerminalRoutes {
    state: Mutex<TerminalRouteState>,
}

struct TerminalRouteState {
    terminals: HashMap<Uuid, TerminalRoute>,
    failure: Option<String>,
}

/// The three channels of one attachment retire together. A connection
/// failure closes only commands, keeping the pane's diagnostic channels.
/// What is told that an event has arrived.
pub(super) type Heard = Arc<dyn Fn() + Send + Sync>;

struct TerminalRoute {
    key: RouteKey<Uuid>,
    /// The pane-facing events: the state and the bytes after it, and
    /// title/bell/clipboard/exit/error.
    events: Sender<TerminalUpdate>,
    /// Told when an event has been put on `events`: whoever takes them
    /// from there is not waiting on it.
    heard: Option<Heard>,
    /// The local (sync-sendable) half of each terminal's command bridge —
    /// the same queue the handle's own forwarding thread feeds; registered
    /// here so a broadcast (`TerminaldHandle::broadcast_terminal_color_scheme`)
    /// can inject a command without going through a pane's handle.
    commands: Option<tokio::sync::mpsc::UnboundedSender<TerminalCommand>>,
}

/// Parses an ACP session id; the daemon uses the `SessionId` uuid string.
pub(super) fn parse_session_id(id: &v2::SessionId) -> Option<SessionId> {
    Uuid::parse_str(&id.0).ok().map(SessionId::from_uuid)
}

impl AgentRoutes {
    pub(super) fn new(news: Sender<RuntimeNews>) -> Self {
        Self {
            state: Mutex::new(AgentRouteState {
                agent: HashMap::new(),
                failure: None,
            }),
            news,
            names: Mutex::new(Names::default()),
            parents: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn register_agent(
        &self,
        session_id: SessionId,
        sender: tokio::sync::mpsc::Sender<AgentUpdate>,
    ) -> RouteKey<SessionId> {
        let key = RouteKey::new(session_id);
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.try_send(AgentUpdate::State(AttachmentState::Failed(message)));
            return key;
        }
        state.agent.insert(
            session_id,
            AgentRoute {
                key,
                events: sender,
                cancelled: tokio::sync::watch::channel(false).0,
                inbound: None,
                ready: false,
                commands: None,
                reopen: false,
            },
        );
        key
    }

    pub(super) fn unregister_agent(&self, key: RouteKey<SessionId>) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&key.session_id)
            .is_some_and(|route| route.key == key)
        {
            state.agent.remove(&key.session_id);
            let mut names = self.names.lock().unwrap();
            if let Some(named) = names.agent.remove(&key.session_id) {
                names.local.remove(&named);
            }
        }
    }

    /// Opens the inbound queue of `key`'s attachment task, and hands it the
    /// pane's commands: `commands` when the session is first opened, the
    /// ones kept from before when it is opened again. `None` when the route
    /// has already been replaced or retired.
    pub(super) fn open_inbound(
        &self,
        key: RouteKey<SessionId>,
        commands: Option<UnboundedReceiver<AgentCommand>>,
    ) -> Option<(UnboundedReceiver<Inbound>, Commands)> {
        let mut state = self.state.lock().unwrap();
        let route = state
            .agent
            .get_mut(&key.session_id)
            .filter(|route| route.key == key)?;
        if let Some(commands) = commands {
            route.commands = Some(Arc::new(tokio::sync::Mutex::new(commands)));
        }
        let commands = route.commands.clone()?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        route.inbound = Some(sender);
        Some((receiver, commands))
    }

    /// Records that `key`'s attachment reached `Ready`.
    pub(super) fn mark_ready(&self, key: RouteKey<SessionId>) {
        let mut state = self.state.lock().unwrap();
        if let Some(route) = state
            .agent
            .get_mut(&key.session_id)
            .filter(|route| route.key == key)
        {
            route.ready = true;
        }
    }

    /// Queues `inbound` for the session's current attachment task. Hands
    /// it back when no task is running for the session.
    pub(super) fn deliver(&self, session_id: SessionId, inbound: Inbound) -> Option<Inbound> {
        let session_id = self.local_of(session_id);
        let state = self.state.lock().unwrap();
        match state
            .agent
            .get(&session_id)
            .and_then(|route| route.inbound.as_ref())
        {
            Some(sender) => sender.send(inbound).err().map(|error| error.0),
            None => Some(inbound),
        }
    }

    /// Queues `inbound` only if `key` is still the session's current route.
    pub(super) fn deliver_to(&self, key: RouteKey<SessionId>, inbound: Inbound) {
        let state = self.state.lock().unwrap();
        if let Some(sender) = state
            .agent
            .get(&key.session_id)
            .filter(|route| route.key == key)
            .and_then(|route| route.inbound.as_ref())
        {
            let _ = sender.send(inbound);
        }
    }

    pub(super) async fn send_agent(&self, key: RouteKey<SessionId>, event: AgentUpdate) -> bool {
        let registration = self
            .state
            .lock()
            .unwrap()
            .agent
            .get(&key.session_id)
            .filter(|route| route.key == key)
            .map(|route| (route.events.clone(), route.cancelled.subscribe()));
        let Some((sender, mut cancelled)) = registration else {
            return false;
        };
        tokio::select! {
            biased;
            _ = cancelled.changed() => false,
            permit = sender.reserve() => {
                let Ok(permit) = permit else { return false; };
                // Replacement/failure may have occurred while waiting for a
                // slow view. Recheck under the same lock as route retirement.
                let state = self.state.lock().unwrap();
                if state.agent.get(&key.session_id).is_none_or(|route| route.key != key) { return false; }
                permit.send(event);
                true
            }
        }
    }

    /// One `session/update` from the connection.
    pub(super) fn route_update(&self, notification: v2::UpdateSessionNotification) {
        let Some(session_id) = parse_session_id(&notification.session_id) else {
            return;
        };
        if let v2::SessionUpdate::SubagentUpdate(subagent) = &notification.update
            && let Some(child) = parse_session_id(&subagent.session_id)
        {
            let parent = self.local_of(session_id);
            self.parents.lock().unwrap().insert(child, parent);
        }
        let _ = self.deliver(
            session_id,
            Inbound::Event(AgentEvent::Update(Box::new(notification.update))),
        );
    }

    /// Forwards a session's resolved workspace root to the process-wide
    /// channel while that session has a registered pane.
    pub(super) fn route_session_info(&self, session_id: SessionId, workspace_root: PathBuf) {
        let local = self.local_of(session_id);
        if self.state.lock().unwrap().agent.contains_key(&local) {
            let _ = self
                .news
                .send(RuntimeNews::Directory(session_id, workspace_root));
        }
    }

    /// Records that the session the shell started as `local` is `named` to
    /// the agent, and tells the shell.
    pub(super) fn name(&self, local: SessionId, named: SessionId) {
        let mut names = self.names.lock().unwrap();
        names.agent.insert(local, named);
        names.local.insert(named, local);
        drop(names);
        let _ = self.news.send(RuntimeNews::Named {
            was: local,
            is: named,
        });
    }

    /// Tells the shell the session it started as `local` did not start.
    pub(super) fn not_named(&self, local: SessionId, error: String) {
        let _ = self.news.send(RuntimeNews::Unnamed { was: local, error });
    }

    /// The shell's id of the session the agent names `session_id`: an
    /// attachment by that name itself, made later, is the one routed to.
    fn local_of(&self, session_id: SessionId) -> SessionId {
        if self.state.lock().unwrap().agent.contains_key(&session_id) {
            return session_id;
        }
        (self.names.lock().unwrap().local.get(&session_id).copied()).unwrap_or(session_id)
    }

    /// The agent's name of the session the shell knows as `session_id`.
    pub(super) fn agent_of(&self, session_id: SessionId) -> SessionId {
        (self.names.lock().unwrap().agent.get(&session_id).copied()).unwrap_or(session_id)
    }

    /// The session the agent names `session_id` is no longer running.
    pub(super) fn route_ended(&self, session_id: SessionId, reason: String) {
        let _ = self.deliver(session_id, Inbound::Ended(reason));
    }

    /// One `session/request_permission`: held by the session's attachment
    /// task, or answered `Cancelled` when no pane holds the session.
    pub(super) fn route_permission(
        &self,
        request: v2::RequestPermissionRequest,
        responder: Responder<v2::RequestPermissionResponse>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Some(session_id) = parse_session_id(&request.session_id) else {
            return responder.respond(cancelled_permission());
        };
        // A subagent's request is shown where its parent is.
        let local = self.local_of(session_id);
        let routed = self.state.lock().unwrap().agent.contains_key(&local);
        let parent = (!routed)
            .then(|| self.parents.lock().unwrap().get(&session_id).copied())
            .flatten();
        let session_id = parent.unwrap_or(local);
        match self.deliver(
            session_id,
            Inbound::Permission(Box::new(request), responder),
        ) {
            Some(Inbound::Permission(_, responder)) => responder.respond(cancelled_permission()),
            _ => Ok(()),
        }
    }

    /// Transport failures are attachment state, never conversation events.
    pub(super) fn agent_failed(&self, key: RouteKey<SessionId>, message: String) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&key.session_id)
            .is_some_and(|route| route.key == key)
        {
            let route = state.agent.remove(&key.session_id).unwrap();
            let _ = route
                .events
                .try_send(AgentUpdate::State(AttachmentState::Failed(message)));
        }
    }

    /// The connection is gone, and another is to come: each attachment task
    /// on it ends (its inbound queue closes), and its session is opened
    /// again on the next connection ([`Self::reopened`]). The routes, and
    /// the panes' channels, stay.
    pub(super) fn connection_lost(&self) {
        let mut state = self.state.lock().unwrap();
        for route in state.agent.values_mut() {
            if route.inbound.take().is_some() {
                route.reopen = true;
                route.ready = false;
            }
        }
    }

    /// The sessions to open again on a new connection.
    pub(super) fn reopened(&self) -> Vec<RouteKey<SessionId>> {
        let mut state = self.state.lock().unwrap();
        (state.agent.values_mut())
            .filter_map(|route| std::mem::take(&mut route.reopen).then_some(route.key))
            .collect()
    }

    /// The connection is gone for good: a ready attachment is
    /// disconnected, one still opening has failed. Later registrations
    /// inherit the failure.
    pub(super) fn connection_failed(&self, message: String) {
        let mut state = self.state.lock().unwrap();
        state.failure = Some(message.clone());
        for (_, route) in state.agent.drain() {
            let mut phase = if route.ready {
                AttachmentState::Ready
            } else {
                AttachmentState::Failed(message.clone())
            };
            phase.stream_ended();
            let _ = route.events.try_send(AgentUpdate::State(phase));
        }
        drop(state);
    }
}

pub(super) fn cancelled_permission() -> v2::RequestPermissionResponse {
    v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Cancelled(
        v2::CancelledPermissionOutcome::new(),
    ))
}

impl TerminalRoutes {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(TerminalRouteState {
                terminals: HashMap::new(),
                failure: None,
            }),
        }
    }

    pub(super) fn register_terminal(
        &self,
        session_id: Uuid,
        events: Sender<TerminalUpdate>,
        commands: tokio::sync::mpsc::UnboundedSender<TerminalCommand>,
    ) -> RouteKey<Uuid> {
        let key = RouteKey::new(session_id);
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            // A dead runtime surfaces as an error event.
            let _ = events.send(TerminalUpdate::Error(message));
            return key;
        }
        state.terminals.insert(
            session_id,
            TerminalRoute {
                key,
                events,
                heard: None,
                commands: Some(commands),
            },
        );
        key
    }

    /// Has `heard` told of every event routed to the attachment from now
    /// on.
    pub(super) fn hear_terminal_events(&self, key: RouteKey<Uuid>, heard: Heard) {
        let mut state = self.state.lock().unwrap();
        if let Some(route) =
            (state.terminals.get_mut(&key.session_id)).filter(|route| route.key == key)
        {
            route.heard = Some(heard);
        }
    }

    pub(super) fn unregister_terminal(&self, key: RouteKey<Uuid>) {
        let mut state = self.state.lock().unwrap();
        if state
            .terminals
            .get(&key.session_id)
            .is_some_and(|route| route.key == key)
        {
            state.terminals.remove(&key.session_id);
        }
    }

    /// Injects `command` into every registered terminal's command bridge —
    /// the broadcast target for a live theme apply's color-scheme re-push
    /// (`TerminaldHandle::broadcast_terminal_color_scheme`). Fire-and-forget,
    /// same as every per-session command send.
    pub(super) fn broadcast_terminal_command(&self, command: TerminalCommand) {
        let state = self.state.lock().unwrap();
        for route in state.terminals.values() {
            if let Some(sender) = &route.commands {
                let _ = sender.send(command.clone());
            }
        }
    }

    /// One incoming non-frame event from a terminal attachment's `events`
    /// channel, routed to its pane. `Exited` also retires the route,
    /// exactly as the JSONL dispatch did.
    pub(super) fn route_terminal_update(&self, key: RouteKey<Uuid>, update: TerminalUpdate) {
        let exited = matches!(update, TerminalUpdate::Exited);
        let mut state = self.state.lock().unwrap();
        let route = (state.terminals.get(&key.session_id)).filter(|route| route.key == key);
        let Some(route) = route else {
            return;
        };
        let heard = route.heard.clone();
        if route.events.send(update).is_err() || exited {
            state.terminals.remove(&key.session_id);
        }
        // (Told with the routes free: what it wakes may come back here.)
        drop(state);
        if let Some(heard) = heard {
            heard();
        }
    }

    /// A terminal-scoped failure that concerns only one session — e.g. a
    /// `create_terminal` call's spawn error, which the JSONL wire used to
    /// deliver as a `TerminalUpdate::Error` on the update stream.
    pub(super) fn terminal_failed(&self, key: RouteKey<Uuid>, message: String) {
        self.route_terminal_update(key, TerminalUpdate::Error(message));
    }

    /// The terminal runtime is gone: every registered terminal pane hears
    /// about it on its event stream, and later registrations inherit the
    /// sticky failure.
    pub(super) fn connection_failed(&self, message: String) {
        let terminal_routes = {
            let mut state = self.state.lock().unwrap();
            state.failure = Some(message.clone());
            state
                .terminals
                .values_mut()
                .map(|route| {
                    route.commands = None;
                    route.events.clone()
                })
                .collect::<Vec<_>>()
        };
        for sender in terminal_routes {
            let _ = sender.send(TerminalUpdate::Error(message.clone()));
        }
    }
}

#[cfg(test)]
mod tests;
