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

use crossbeam_channel::Sender;
use horizon_agent::contract::{self, Event, ProviderEvent};
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
    events: Sender<ProviderEvent>,
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
        sender: Sender<ProviderEvent>,
    ) -> RouteKey<contract::SessionId> {
        let key = RouteKey::new(session_id);
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(ProviderEvent::from(Event::Error(contract::Error {
                message,
            })));
            return key;
        }
        state.agent.insert(
            session_id,
            AgentRoute {
                key,
                events: sender,
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

    pub(super) fn send_agent(&self, key: RouteKey<contract::SessionId>, event: ProviderEvent) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&key.session_id)
            .is_some_and(|route| route.key == key && route.events.send(event).is_err())
        {
            state.agent.remove(&key.session_id);
        }
    }

    /// One incoming event from an agent attachment's channel, fanned to
    /// the pane (or the process-wide workspace-root channel).
    pub(super) fn route_agent_event(
        &self,
        key: RouteKey<contract::SessionId>,
        event: AgentWireEvent,
    ) {
        match event {
            AgentWireEvent::Event(event) => self.send_agent(key, ProviderEvent::from(event)),
            AgentWireEvent::ToolCallProgress(progress) => {
                self.send_agent(key, ProviderEvent::tool_call_progress(progress));
            }
            AgentWireEvent::TaskProgress(progress) => {
                self.send_agent(key, ProviderEvent::task_progress(progress));
            }
            AgentWireEvent::SessionModel(model) => {
                self.send_agent(key, ProviderEvent::session_model(model));
            }
            AgentWireEvent::SessionSelection(selection) => {
                self.send_agent(
                    key,
                    ProviderEvent::session_selection(selection.provider, selection.model),
                );
            }
            AgentWireEvent::WorkspaceRootResolved(resolved) => {
                let state = self.state.lock().unwrap();
                if state
                    .agent
                    .get(&key.session_id)
                    .is_some_and(|route| route.key == key)
                {
                    let _ = self.workspace_roots.send((key.session_id, resolved));
                }
            }
        }
    }

    pub(super) fn host_tool_request(&self, request: HostToolRequest) {
        let _ = self.host_tools.send(request);
    }

    /// An agent attach/spawn call failed outright — surfaced into the
    /// session's own transcript channel as an error event, the same shape
    /// a connection-wide failure takes.
    pub(super) fn agent_failed(&self, key: RouteKey<contract::SessionId>, message: String) {
        self.send_agent(
            key,
            ProviderEvent::from(Event::Error(contract::Error { message })),
        );
    }

    /// The agent runtime is gone: every registered agent session hears
    /// about it, and later registrations inherit the sticky failure. No
    /// terminal is touched — that is a different connection with a
    /// different table.
    pub(super) fn connection_failed(&self, message: String) {
        let agent_routes = {
            let mut state = self.state.lock().unwrap();
            state.failure = Some(message.clone());
            state
                .agent
                .values()
                .map(|route| route.events.clone())
                .collect::<Vec<_>>()
        };
        for sender in agent_routes {
            let _ = sender.send(ProviderEvent::from(Event::Error(contract::Error {
                message: message.clone(),
            })));
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
