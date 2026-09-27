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
use std::sync::Mutex;

use super::attachment::{AgentUpdate, AttachmentState, Inbound};
use crate::agent::model::AgentEvent;
use agent_client_protocol::schema::v2;
use agent_client_protocol::Responder;
use crossbeam_channel::Sender;
use horizon_acp::{
    read_horizon_meta, AttachmentEnd, HostToolRequest, HostToolResponse, SessionEventNotification,
    SessionId, SessionInfoMeta,
};
use horizon_terminal_core::{TerminalCommand, TerminalFrame, TerminalUpdate};
use uuid::Uuid;

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

/// A session's authoritative workspace root and parent, from
/// `SessionInfoMeta` on `session/new`'s response or a
/// `session_info_update`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceRootUpdate {
    pub(crate) workspace_root: PathBuf,
    pub(crate) parent_session_id: Option<SessionId>,
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
    host_tools: Sender<HostToolRequest>,
    /// Held `_horizon/host_tool` responders, by request id.
    host_tool_responders: Mutex<HashMap<String, Responder<HostToolResponse>>>,
    workspace_roots: Sender<(SessionId, WorkspaceRootUpdate)>,
    /// The first extension-version rejection a request met on the current
    /// connection, and its wakeup for the connection's op loop.
    version_mismatch: Mutex<Option<String>>,
    version_mismatch_signal: tokio::sync::Notify,
}

/// The message prefix of the daemon's rejection when the extension
/// versions differ.
pub(super) const EXT_VERSION_MISMATCH: &str = "horizon ext version mismatch";

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
struct TerminalRoute {
    key: RouteKey<Uuid>,
    /// The pane-facing frame stream: since wire v11 the frame path is a
    /// `watch<TerminalFrame>`, so the transport delivers full frames here
    /// (separate from `terminal_events`).
    frames: tokio::sync::watch::Sender<TerminalFrame>,
    /// The pane-facing non-frame events (title/bell/clipboard/exit/error).
    events: Sender<TerminalUpdate>,
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
    pub(super) fn new(
        host_tools: Sender<HostToolRequest>,
        workspace_roots: Sender<(SessionId, WorkspaceRootUpdate)>,
    ) -> Self {
        Self {
            state: Mutex::new(AgentRouteState {
                agent: HashMap::new(),
                failure: None,
            }),
            host_tools,
            host_tool_responders: Mutex::new(HashMap::new()),
            workspace_roots,
            version_mismatch: Mutex::new(None),
            version_mismatch_signal: tokio::sync::Notify::new(),
        }
    }

    /// Records a request error; an extension-version rejection wakes
    /// [`Self::version_mismatch`].
    pub(super) fn note_error(&self, error: &agent_client_protocol::Error) {
        if error.message.starts_with(EXT_VERSION_MISMATCH) {
            *self.version_mismatch.lock().unwrap() = Some(error.message.clone());
            self.version_mismatch_signal.notify_one();
        }
    }

    /// Forgets a rejection recorded on an earlier connection.
    pub(super) fn clear_version_mismatch(&self) {
        self.version_mismatch.lock().unwrap().take();
    }

    /// Resolves with the message of the next extension-version rejection a
    /// request meets.
    pub(super) async fn version_mismatch(&self) -> String {
        loop {
            if let Some(message) = self.version_mismatch.lock().unwrap().take() {
                return message;
            }
            self.version_mismatch_signal.notified().await;
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
        }
    }

    /// Opens the inbound queue of `key`'s attachment task. `None` when the
    /// route has already been replaced or retired.
    pub(super) fn open_inbound(
        &self,
        key: RouteKey<SessionId>,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<Inbound>> {
        let mut state = self.state.lock().unwrap();
        let route = state
            .agent
            .get_mut(&key.session_id)
            .filter(|route| route.key == key)?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        route.inbound = Some(sender);
        Some(receiver)
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
        if let v2::SessionUpdate::SessionInfoUpdate(info) = &notification.update {
            if let agent_client_protocol::schema::MaybeUndefined::Value(meta) = &info.meta {
                if let Some(Ok(meta)) = read_horizon_meta::<SessionInfoMeta>(Some(meta)) {
                    self.route_session_info(session_id, &meta);
                }
            }
        }
        let _ = self.deliver(
            session_id,
            Inbound::Event(AgentEvent::Update(Box::new(notification.update))),
        );
    }

    /// Forwards a session's resolved workspace root to the process-wide
    /// channel while that session has a registered pane.
    pub(super) fn route_session_info(&self, session_id: SessionId, meta: &SessionInfoMeta) {
        let Some(workspace_root) = meta.workspace_root.clone() else {
            return;
        };
        if self.state.lock().unwrap().agent.contains_key(&session_id) {
            let _ = self.workspace_roots.send((
                session_id,
                WorkspaceRootUpdate {
                    workspace_root,
                    parent_session_id: meta.parent_session_id,
                },
            ));
        }
    }

    /// One `_horizon/session_event` from the connection.
    pub(super) fn route_session_event(&self, event: SessionEventNotification) {
        let session_id = match &event {
            SessionEventNotification::SkippedLines { summary } => {
                // No pane consumes the startup corruption summary; the log
                // keeps it visible.
                eprintln!("horizon-agentd event log: {summary}");
                return;
            }
            SessionEventNotification::AttachmentClosed { session_id, reason } => {
                let _ = self.deliver(*session_id, Inbound::Closed(*reason));
                return;
            }
            SessionEventNotification::SessionResumed { session_id }
            | SessionEventNotification::ProviderRateLimited { session_id, .. }
            | SessionEventNotification::HistoryCleared { session_id, .. }
            | SessionEventNotification::Error { session_id, .. }
            | SessionEventNotification::Exited { session_id, .. } => *session_id,
        };
        let _ = self.deliver(session_id, Inbound::Event(AgentEvent::Session(event)));
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
        match self.deliver(
            session_id,
            Inbound::Permission(Box::new(request), responder),
        ) {
            Some(Inbound::Permission(_, responder)) => responder.respond(cancelled_permission()),
            _ => Ok(()),
        }
    }

    pub(super) fn host_tool_request(
        &self,
        request: HostToolRequest,
        responder: Responder<HostToolResponse>,
    ) {
        self.host_tool_responders
            .lock()
            .unwrap()
            .insert(request.request_id.clone(), responder);
        let _ = self.host_tools.send(request);
    }

    pub(super) fn respond_host_tool(&self, request_id: &str, output: serde_json::Value) {
        let responder = self.host_tool_responders.lock().unwrap().remove(request_id);
        if let Some(responder) = responder {
            let _ = responder.respond(HostToolResponse { output });
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

    /// The connection is gone: a ready attachment is disconnected, one
    /// still opening has failed. Later registrations inherit the failure.
    pub(super) fn connection_failed(&self, message: String) {
        self.settle_routes(message, true);
    }

    /// The connection is being replaced: every attachment on it ends as in
    /// [`Self::connection_failed`], but later registrations are not failed.
    pub(super) fn connection_reset(&self, message: String) {
        self.settle_routes(message, false);
    }

    fn settle_routes(&self, message: String, sticky: bool) {
        let mut state = self.state.lock().unwrap();
        if sticky {
            state.failure = Some(message.clone());
        }
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
        self.host_tool_responders.lock().unwrap().clear();
    }
}

pub(super) fn cancelled_permission() -> v2::RequestPermissionResponse {
    v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Cancelled)
}

/// A session's attachment closed by the daemon, as the pane reads it.
pub(super) fn attachment_end_state(reason: AttachmentEnd) -> AttachmentState {
    match reason {
        AttachmentEnd::Lagged => AttachmentState::Failed(
            "Session updates exceeded the connection buffer; reopen the session to restore its history"
                .into(),
        ),
        AttachmentEnd::Replaced => {
            AttachmentState::Disconnected("Session opened by another attachment".into())
        }
        AttachmentEnd::Detached => AttachmentState::Disconnected("Session detached".into()),
        AttachmentEnd::SessionEnded => {
            AttachmentState::Disconnected("Session runtime ended".into())
        }
    }
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
        frames: tokio::sync::watch::Sender<TerminalFrame>,
        events: Sender<TerminalUpdate>,
        commands: tokio::sync::mpsc::UnboundedSender<TerminalCommand>,
    ) -> RouteKey<Uuid> {
        let key = RouteKey::new(session_id);
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            // A dead runtime surfaces as an error event; the frame stream
            // simply never delivers (it carries frames, not errors).
            let _ = events.send(TerminalUpdate::Error(message));
            return key;
        }
        state.terminals.insert(
            session_id,
            TerminalRoute {
                key,
                frames,
                events,
                commands: Some(commands),
            },
        );
        key
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

    /// One incoming full frame from a terminal attachment's `frames` watch,
    /// routed to its pane. A dead pane retires the whole route.
    pub(super) fn route_terminal_frame(&self, key: RouteKey<Uuid>, frame: TerminalFrame) {
        let mut state = self.state.lock().unwrap();
        if state
            .terminals
            .get(&key.session_id)
            .is_some_and(|route| route.key == key && route.frames.send(frame).is_err())
        {
            state.terminals.remove(&key.session_id);
        }
    }

    /// One incoming non-frame event from a terminal attachment's `events`
    /// channel, routed to its pane. `Exited` also retires the route,
    /// exactly as the JSONL dispatch did.
    pub(super) fn route_terminal_update(&self, key: RouteKey<Uuid>, update: TerminalUpdate) {
        let exited = matches!(update, TerminalUpdate::Exited);
        let mut state = self.state.lock().unwrap();
        if state
            .terminals
            .get(&key.session_id)
            .is_some_and(|route| route.key == key && (route.events.send(update).is_err() || exited))
        {
            state.terminals.remove(&key.session_id);
        }
    }

    /// A terminal-scoped failure that concerns only one session — e.g. a
    /// `create_terminal` call's spawn error, which the JSONL wire used to
    /// deliver as a `TerminalUpdate::Error` on the update stream.
    pub(super) fn terminal_failed(&self, key: RouteKey<Uuid>, message: String) {
        self.route_terminal_update(key, TerminalUpdate::Error(message));
    }

    /// The terminal runtime is gone: every registered terminal pane hears
    /// about it on its event stream (the frame watch just stops
    /// delivering), and later registrations inherit the sticky failure.
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
