use std::collections::HashMap;
use std::sync::Mutex;

use crossbeam_channel::Sender;
use horizon_agent::contract::{self, Event, ProviderEvent};
use horizon_agent::wire::{self, AgentWireEvent, HostToolRequest};
use horizon_terminal_core::{TerminalCommand, TerminalFrame, TerminalUpdate};
use uuid::Uuid;

/// The client-side fan-out table: which pane's channels receive a given
/// session's events/updates. The v10 cutover deleted its other half — the
/// `request_id` correlation maps (`pending_terminal_lists`/`attaches`,
/// `pending_session_list`) are gone because rtc calls return futures, and
/// envelope `kind` dispatch is gone because channel identity *is* the
/// route. What remains is exactly the state that is genuinely the UI's:
/// per-session crossbeam senders into panes, the process-wide host-tool /
/// workspace-root channels, and the sticky failure that fans a terminal
/// runtime error out to everything registered.
pub(super) struct Routes {
    state: Mutex<RouteState>,
    host_tools: Sender<HostToolRequest>,
    /// The live-announcement counterpart of `host_tools` above: a
    /// process-wide channel (not the per-session `agent` map) since
    /// `wire::AgentWireEvent::WorkspaceRootResolved` corrects the
    /// *workspace model*, not a `contract::ProviderEvent` any per-session
    /// `AgentSession` transcript would fold -- see `WorkspaceShell::
    /// wire_workspace_root_updates`.
    workspace_roots: Sender<(contract::SessionId, wire::WorkspaceRootResolved)>,
}

struct RouteState {
    agent: HashMap<contract::SessionId, Sender<ProviderEvent>>,
    /// The pane-facing frame stream: since wire v11 the frame path is a
    /// `watch<TerminalFrame>`, so the transport delivers full frames here
    /// (separate from `terminal_events`).
    terminal_frames: HashMap<Uuid, Sender<TerminalFrame>>,
    /// The pane-facing non-frame events (title/bell/clipboard/exit/error).
    terminal_events: HashMap<Uuid, Sender<TerminalUpdate>>,
    /// The local (sync-sendable) half of each terminal's command bridge —
    /// the same queue the handle's own forwarding thread feeds; registered
    /// here so a broadcast (`SessiondHandle::broadcast_terminal_color_scheme`)
    /// can inject a command without going through a pane's handle.
    terminal_commands: HashMap<Uuid, tokio::sync::mpsc::UnboundedSender<TerminalCommand>>,
    failure: Option<String>,
}

impl Routes {
    pub(super) fn new(
        host_tools: Sender<HostToolRequest>,
        workspace_roots: Sender<(contract::SessionId, wire::WorkspaceRootResolved)>,
    ) -> Self {
        Self {
            state: Mutex::new(RouteState {
                agent: HashMap::new(),
                terminal_frames: HashMap::new(),
                terminal_events: HashMap::new(),
                terminal_commands: HashMap::new(),
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
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(ProviderEvent::from(Event::Error(contract::Error {
                message,
            })));
            return;
        }
        state.agent.insert(session_id, sender);
    }

    pub(super) fn register_terminal(
        &self,
        session_id: Uuid,
        frames: Sender<TerminalFrame>,
        events: Sender<TerminalUpdate>,
        commands: tokio::sync::mpsc::UnboundedSender<TerminalCommand>,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            // A dead runtime surfaces as an error event; the frame stream
            // simply never delivers (it carries frames, not errors).
            let _ = events.send(TerminalUpdate::Error(message));
            return;
        }
        state.terminal_frames.insert(session_id, frames);
        state.terminal_events.insert(session_id, events);
        state.terminal_commands.insert(session_id, commands);
    }

    pub(super) fn unregister_agent(&self, session_id: contract::SessionId) {
        self.state.lock().unwrap().agent.remove(&session_id);
    }

    pub(super) fn unregister_terminal(&self, session_id: Uuid) {
        let mut state = self.state.lock().unwrap();
        state.terminal_frames.remove(&session_id);
        state.terminal_events.remove(&session_id);
        state.terminal_commands.remove(&session_id);
    }

    /// Injects `command` into every registered terminal's command bridge —
    /// the broadcast target for a live theme apply's color-scheme re-push
    /// (`SessiondHandle::broadcast_terminal_color_scheme`). Fire-and-forget,
    /// same as every per-session command send.
    pub(super) fn broadcast_terminal_command(&self, command: TerminalCommand) {
        let state = self.state.lock().unwrap();
        for sender in state.terminal_commands.values() {
            let _ = sender.send(command.clone());
        }
    }

    /// One incoming full frame from a terminal attachment's `frames` watch,
    /// routed to its pane. A dead pane retires the whole route.
    pub(super) fn route_terminal_frame(&self, session_id: Uuid, frame: TerminalFrame) {
        let mut state = self.state.lock().unwrap();
        if state
            .terminal_frames
            .get(&session_id)
            .is_some_and(|sender| sender.send(frame).is_err())
        {
            state.terminal_frames.remove(&session_id);
            state.terminal_events.remove(&session_id);
            state.terminal_commands.remove(&session_id);
        }
    }

    /// One incoming non-frame event from a terminal attachment's `events`
    /// channel, routed to its pane. `Exited` also retires the route,
    /// exactly as the JSONL dispatch did.
    pub(super) fn route_terminal_update(&self, session_id: Uuid, update: TerminalUpdate) {
        let exited = matches!(update, TerminalUpdate::Exited);
        let mut state = self.state.lock().unwrap();
        if state
            .terminal_events
            .get(&session_id)
            .is_some_and(|sender| sender.send(update).is_err())
            || exited
        {
            state.terminal_frames.remove(&session_id);
            state.terminal_events.remove(&session_id);
            state.terminal_commands.remove(&session_id);
        }
    }

    /// A terminal-scoped failure that concerns only one session — e.g. a
    /// `create_terminal` call's spawn error, which the JSONL wire used to
    /// deliver as a `TerminalUpdate::Error` on the update stream.
    pub(super) fn terminal_failed(&self, session_id: Uuid, message: String) {
        self.route_terminal_update(session_id, TerminalUpdate::Error(message));
    }

    pub(super) fn send_agent(&self, session_id: contract::SessionId, event: ProviderEvent) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&session_id)
            .is_some_and(|sender| sender.send(event).is_err())
        {
            state.agent.remove(&session_id);
        }
    }

    /// One incoming event from an agent attachment's channel, fanned to
    /// the pane (or the process-wide workspace-root channel).
    pub(super) fn route_agent_event(&self, session_id: contract::SessionId, event: AgentWireEvent) {
        match event {
            AgentWireEvent::Event(event) => self.send_agent(session_id, ProviderEvent::from(event)),
            AgentWireEvent::ToolCallProgress(progress) => {
                self.send_agent(session_id, ProviderEvent::tool_call_progress(progress));
            }
            AgentWireEvent::SessionModel(model) => {
                self.send_agent(session_id, ProviderEvent::session_model(model));
            }
            AgentWireEvent::WorkspaceRootResolved(resolved) => {
                let _ = self.workspace_roots.send((session_id, resolved));
            }
            // Skew catch-all: an event this build can't name is skipped;
            // the channel stays up (adoption condition 2).
            AgentWireEvent::Unknown => {}
        }
    }

    pub(super) fn host_tool_request(&self, request: HostToolRequest) {
        let _ = self.host_tools.send(request);
    }

    /// An agent attach/spawn call failed outright — surfaced into the
    /// session's own transcript channel as an error event, the same shape
    /// a connection-wide failure takes.
    pub(super) fn agent_failed(&self, session_id: contract::SessionId, message: String) {
        self.send_agent(
            session_id,
            ProviderEvent::from(Event::Error(contract::Error { message })),
        );
    }

    pub(super) fn connection_failed(&self, message: String) {
        let (terminal_routes, agent_routes) = {
            let mut state = self.state.lock().unwrap();
            state.failure = Some(message.clone());
            state.terminal_commands.clear();
            (
                // The error rides the event stream; the frame watch just
                // stops delivering.
                state.terminal_events.values().cloned().collect::<Vec<_>>(),
                state.agent.values().cloned().collect::<Vec<_>>(),
            )
        };
        for sender in terminal_routes {
            let _ = sender.send(TerminalUpdate::Error(message.clone()));
        }
        for sender in agent_routes {
            let _ = sender.send(ProviderEvent::from(Event::Error(contract::Error {
                message: message.clone(),
            })));
        }
    }
}
