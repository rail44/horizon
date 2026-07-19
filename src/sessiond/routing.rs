use std::collections::HashMap;
use std::sync::Mutex;

use crossbeam_channel::Sender;
use horizon_agent::contract::{self, Event, ProviderEvent};
use horizon_agent::wire::{self, Control, EnvelopeBody, HostToolRequest};
use horizon_session_protocol::{Envelope as RawEnvelope, SessionControl, SESSION_CONTROL_KIND};
use horizon_terminal_core::{
    decode_terminal_control, decode_terminal_update, TerminalAttachResult, TerminalControl,
    TerminalSummary, TerminalUpdate, TERMINAL_CONTROL_KIND, TERMINAL_UPDATE_KIND,
};
use uuid::Uuid;

pub(super) enum Incoming {
    Pong(RawEnvelope),
    Handled,
}

pub(super) struct Routes {
    state: Mutex<RouteState>,
    host_tools: Sender<HostToolRequest>,
    /// The live-announcement counterpart of `host_tools` above: a
    /// process-wide channel (not the per-session `agent` map) since
    /// `wire::Control::WorkspaceRootResolved` corrects the *workspace
    /// model*, not a `contract::ProviderEvent` any per-session `AgentSession`
    /// transcript would fold -- see `WorkspaceShell::
    /// wire_workspace_root_updates`.
    workspace_roots: Sender<(contract::SessionId, wire::WorkspaceRootResolved)>,
}

struct RouteState {
    agent: HashMap<contract::SessionId, Sender<ProviderEvent>>,
    terminal: HashMap<Uuid, Sender<TerminalUpdate>>,
    pending_session_list: Option<Sender<Result<Vec<wire::SessionSummary>, String>>>,
    pending_terminal_lists: HashMap<Uuid, Sender<Result<Vec<TerminalSummary>, String>>>,
    pending_terminal_attaches: HashMap<Uuid, (Uuid, Sender<Result<TerminalAttachResult, String>>)>,
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
                terminal: HashMap::new(),
                pending_session_list: None,
                pending_terminal_lists: HashMap::new(),
                pending_terminal_attaches: HashMap::new(),
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

    pub(super) fn register_terminal(&self, session_id: Uuid, sender: Sender<TerminalUpdate>) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(TerminalUpdate::Error(message));
            return;
        }
        state.terminal.insert(session_id, sender);
    }

    pub(super) fn unregister_agent(&self, session_id: contract::SessionId) {
        self.state.lock().unwrap().agent.remove(&session_id);
    }

    pub(super) fn unregister_terminal(&self, session_id: Uuid) {
        self.state.lock().unwrap().terminal.remove(&session_id);
    }

    /// Every terminal session this client currently has an update route
    /// for -- i.e. every live, attached terminal session, regardless of
    /// which pane (if any) is showing it. The broadcast target for a live
    /// theme apply's color-scheme re-push (`SessiondHandle::
    /// broadcast_terminal_color_scheme`).
    pub(super) fn terminal_session_ids(&self) -> Vec<Uuid> {
        self.state
            .lock()
            .unwrap()
            .terminal
            .keys()
            .copied()
            .collect()
    }

    pub(super) fn set_pending_session_list(
        &self,
        sender: Sender<Result<Vec<wire::SessionSummary>, String>>,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(Err(message));
            return;
        }
        state.pending_session_list = Some(sender);
    }

    pub(super) fn set_pending_terminal_list(
        &self,
        request_id: Uuid,
        sender: Sender<Result<Vec<TerminalSummary>, String>>,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(Err(message));
            return;
        }
        state.pending_terminal_lists.insert(request_id, sender);
    }

    pub(super) fn set_pending_terminal_attach(
        &self,
        request_id: Uuid,
        session_id: Uuid,
        sender: Sender<Result<TerminalAttachResult, String>>,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.failure.clone() {
            let _ = sender.send(Err(message));
            return;
        }
        state
            .pending_terminal_attaches
            .insert(request_id, (session_id, sender));
    }

    pub(super) fn cancel_pending_terminal_list(&self, request_id: Uuid) {
        self.state
            .lock()
            .unwrap()
            .pending_terminal_lists
            .remove(&request_id);
    }

    pub(super) fn cancel_pending_terminal_attach(&self, request_id: Uuid) {
        self.state
            .lock()
            .unwrap()
            .pending_terminal_attaches
            .remove(&request_id);
    }

    pub(super) fn dispatch(&self, raw: RawEnvelope) -> Result<Incoming, String> {
        if raw.kind == SESSION_CONTROL_KIND {
            return match raw.decode_payload::<SessionControl>(SESSION_CONTROL_KIND) {
                Ok(SessionControl::Ping) => RawEnvelope::session_control(&SessionControl::Pong)
                    .map(Incoming::Pong)
                    .map_err(|error| error.to_string()),
                Ok(_) => Ok(Incoming::Handled),
                Err(error) => Err(format!("malformed shared session control: {error}")),
            };
        }

        if raw.kind == TERMINAL_UPDATE_KIND {
            let session_id = raw
                .session_id
                .ok_or_else(|| "terminal update missing session_id".to_string())?;
            let update = decode_terminal_update(&raw)
                .map_err(|error| format!("malformed terminal update: {error}"))?;
            let exited = matches!(update, TerminalUpdate::Exited);
            let mut state = self.state.lock().unwrap();
            if state
                .terminal
                .get(&session_id)
                .is_some_and(|sender| sender.send(update).is_err())
                || exited
            {
                state.terminal.remove(&session_id);
            }
            return Ok(Incoming::Handled);
        }

        if raw.kind == TERMINAL_CONTROL_KIND {
            let control = decode_terminal_control(&raw)
                .map_err(|error| format!("malformed terminal control: {error}"))?;
            let mut state = self.state.lock().unwrap();
            match control {
                TerminalControl::ListResult {
                    request_id,
                    sessions,
                } => {
                    if let Some(reply) = state.pending_terminal_lists.remove(&request_id) {
                        let _ = reply.send(Ok(sessions));
                    }
                }
                TerminalControl::AttachResult { request_id, result } => {
                    let session_id = raw
                        .session_id
                        .ok_or_else(|| "terminal attach result missing session_id".to_string())?;
                    if let Some((expected_session_id, reply)) =
                        state.pending_terminal_attaches.remove(&request_id)
                    {
                        if session_id != expected_session_id {
                            let _ = reply.send(Err(format!(
                                "terminal attach result session mismatch: expected {expected_session_id}, got {session_id}"
                            )));
                            return Err(
                                "terminal attach result targeted the wrong session".to_string()
                            );
                        }
                        let _ = reply.send(Ok(result));
                    }
                }
                TerminalControl::List { .. }
                | TerminalControl::Create(_)
                | TerminalControl::Attach { .. } => {}
                // Skew catch-all (`TerminalControl::Unknown`'s doc): a
                // control this build can't name is dropped; the connection
                // stays up.
                TerminalControl::Unknown(_) => {}
            }
            return Ok(Incoming::Handled);
        }

        let envelope = wire::decode_envelope(raw)
            .map_err(|error| format!("unknown or malformed domain message: {error}"))?;
        match envelope.body {
            EnvelopeBody::Event(event) => {
                if let Some(session_id) = envelope.session_id {
                    self.send_agent(session_id, ProviderEvent::from(event));
                }
            }
            EnvelopeBody::Control(Control::ToolCallProgress(progress)) => {
                if let Some(session_id) = envelope.session_id {
                    self.send_agent(session_id, ProviderEvent::tool_call_progress(progress));
                }
            }
            EnvelopeBody::Control(Control::SessionModel(model)) => {
                if let Some(session_id) = envelope.session_id {
                    self.send_agent(session_id, ProviderEvent::session_model(model));
                }
            }
            EnvelopeBody::Control(Control::HostToolRequest(request)) => {
                let _ = self.host_tools.send(request);
            }
            EnvelopeBody::Control(Control::WorkspaceRootResolved(resolved)) => {
                if let Some(session_id) = envelope.session_id {
                    let _ = self.workspace_roots.send((session_id, resolved));
                }
            }
            EnvelopeBody::Control(Control::SessionListResult(summaries)) => {
                if let Some(reply) = self.state.lock().unwrap().pending_session_list.take() {
                    let _ = reply.send(Ok(summaries));
                }
            }
            EnvelopeBody::Control(_) | EnvelopeBody::Command(_) => {}
        }
        Ok(Incoming::Handled)
    }

    pub(super) fn connection_failed(&self, message: String) {
        let (terminal_routes, agent_routes, pending, terminal_lists, terminal_attaches) = {
            let mut state = self.state.lock().unwrap();
            state.failure = Some(message.clone());
            let terminal_routes = state.terminal.values().cloned().collect::<Vec<_>>();
            let agent_routes = state.agent.values().cloned().collect::<Vec<_>>();
            let pending = state.pending_session_list.take();
            let terminal_lists = state
                .pending_terminal_lists
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>();
            let terminal_attaches = state
                .pending_terminal_attaches
                .drain()
                .map(|(_, (_, sender))| sender)
                .collect::<Vec<_>>();
            (
                terminal_routes,
                agent_routes,
                pending,
                terminal_lists,
                terminal_attaches,
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
        if let Some(reply) = pending {
            let _ = reply.send(Err(message.clone()));
        }
        for reply in terminal_lists {
            let _ = reply.send(Err(message.clone()));
        }
        for reply in terminal_attaches {
            let _ = reply.send(Err(message.clone()));
        }
    }

    fn send_agent(&self, session_id: contract::SessionId, event: ProviderEvent) {
        let mut state = self.state.lock().unwrap();
        if state
            .agent
            .get(&session_id)
            .is_some_and(|sender| sender.send(event).is_err())
        {
            state.agent.remove(&session_id);
        }
    }
}
