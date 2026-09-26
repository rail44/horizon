//! The client-side fan-out tables: which pane's channels receive a given
//! session's events/updates. The v10 cutover deleted the other half — the
//! `request_id` correlation maps (`pending_terminal_lists`/`attaches`,
//! `pending_session_list`) are gone because rtc calls return futures, and
//! envelope `kind` dispatch is gone because channel identity *is* the
//! route. What remains is exactly the state that is genuinely the UI's:
//! per-session crossbeam senders into panes, the process-wide host-tool /
//! workspace-root channels, and the sticky failure that fans a runtime
//! error out to everything registered.
//!
//! Split in two by the terminald split (`docs/terminald-split-design.md`):
//! one table per daemon connection, each with its *own* sticky failure. That
//! separation is load-bearing, not cosmetic — before it, one `Routes` served
//! both domains, so a agentd failure fanned `TerminalUpdate::Error` out to
//! every terminal pane and poisoned later terminal registrations. Now an
//! agent-runtime failure is visible only to agent sessions, which is the
//! client-side half of "terminals do not care what the agent daemon is
//! doing".

use std::collections::HashMap;
use std::sync::Mutex;

use super::attachment::{AgentUpdate, AttachmentState};
use crossbeam_channel::Sender;
use horizon_agent::contract::{self, ProviderEvent};
use horizon_agent::wire::{self, AgentWireEvent, HostToolRequest};
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

struct AgentRoute {
    key: RouteKey<contract::SessionId>,
    events: tokio::sync::mpsc::Sender<AgentUpdate>,
    cancelled: tokio::sync::watch::Sender<bool>,
}

impl Drop for AgentRoute {
    fn drop(&mut self) {
        self.cancelled.send_replace(true);
    }
}

/// The `horizon-agentd` connection's routes: per-agent-session event
/// senders plus the two process-wide channels its connection-global
/// exchanges feed.
pub(super) struct AgentRoutes {
    state: Mutex<AgentRouteState>,
    host_tools: Sender<HostToolRequest>,
    /// The live-announcement counterpart of `host_tools` above: a
    /// process-wide channel (not the per-session `agent` map) since
    /// `wire::AgentWireEvent::WorkspaceRootResolved` corrects the
    /// *workspace model*, not a `contract::ProviderEvent` any per-session
    /// `AgentSession` transcript would fold -- see `WorkspaceShell::
    /// wire_workspace_root_updates`.
    workspace_roots: Sender<(contract::SessionId, wire::WorkspaceRootResolved)>,
}

struct AgentRouteState {
    agent: HashMap<contract::SessionId, AgentRoute>,
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

impl AgentRoutes {
    pub(super) fn new(
        host_tools: Sender<HostToolRequest>,
        workspace_roots: Sender<(contract::SessionId, wire::WorkspaceRootResolved)>,
    ) -> Self {
        Self {
            state: Mutex::new(AgentRouteState {
                agent: HashMap::new(),
                failure: None,
            }),
            host_tools,
            workspace_roots,
        }
    }

    pub(super) fn register_agent(
        &self,
        session_id: contract::SessionId,
        sender: tokio::sync::mpsc::Sender<AgentUpdate>,
    ) -> RouteKey<contract::SessionId> {
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
            },
        );
        key
    }

    pub(super) fn unregister_agent(&self, key: RouteKey<contract::SessionId>) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&key.session_id)
            .is_some_and(|route| route.key == key)
        {
            state.agent.remove(&key.session_id);
        }
    }

    pub(super) async fn send_agent(
        &self,
        key: RouteKey<contract::SessionId>,
        event: AgentUpdate,
    ) -> bool {
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

    /// One incoming event from an agent attachment's channel, fanned to
    /// the pane (or the process-wide workspace-root channel).
    pub(super) async fn route_agent_event(
        &self,
        key: RouteKey<contract::SessionId>,
        event: AgentWireEvent,
    ) -> bool {
        let provider = match event {
            AgentWireEvent::Event(event) => ProviderEvent::from(event),
            AgentWireEvent::ToolCallProgress(progress) => {
                ProviderEvent::tool_call_progress(progress)
            }
            AgentWireEvent::ToolCallProgressClosed(key) => {
                ProviderEvent::ToolCallProgressClosed(key)
            }
            AgentWireEvent::TaskProgress(progress) => ProviderEvent::task_progress(progress),
            AgentWireEvent::SessionModel(model) => ProviderEvent::session_model(model),
            AgentWireEvent::SessionSelection(selection) => {
                ProviderEvent::session_selection(selection.provider, selection.model)
            }
            AgentWireEvent::WorkspaceRootResolved(resolved) => {
                let state = self.state.lock().unwrap();
                if state
                    .agent
                    .get(&key.session_id)
                    .is_some_and(|route| route.key == key)
                {
                    let _ = self.workspace_roots.send((key.session_id, resolved));
                    return true;
                }
                return false;
            }
            AgentWireEvent::ReplayStarted
            | AgentWireEvent::ReplayComplete
            | AgentWireEvent::AttachmentClosed(_) => {
                unreachable!("attachment control is consumed before routing")
            }
        };
        self.send_agent(key, AgentUpdate::Event(Box::new(provider)))
            .await
    }

    pub(super) fn host_tool_request(&self, request: HostToolRequest) {
        let _ = self.host_tools.send(request);
    }

    /// Transport failures are attachment state, never conversation events.
    pub(super) fn agent_failed(&self, key: RouteKey<contract::SessionId>, message: String) {
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

    pub(super) fn connection_failed(&self, message: String) {
        let mut state = self.state.lock().unwrap();
        state.failure = Some(message.clone());
        for (_, route) in state.agent.drain() {
            let _ = route
                .events
                .try_send(AgentUpdate::State(AttachmentState::Failed(message.clone())));
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
