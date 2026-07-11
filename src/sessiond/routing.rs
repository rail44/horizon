use std::collections::HashMap;
use std::sync::Mutex;

use crossbeam_channel::Sender;
use horizon_agent::contract::{self, Event, ProviderEvent};
use horizon_agent::wire::{self, Control, EnvelopeBody, HostToolRequest};
use horizon_session_protocol::{Envelope as RawEnvelope, SessionControl, SESSION_CONTROL_KIND};
use horizon_terminal_core::{decode_terminal_update, TerminalUpdate, TERMINAL_UPDATE_KIND};
use uuid::Uuid;

pub(super) enum Incoming {
    Pong(RawEnvelope),
    Handled,
}

pub(super) struct Routes {
    state: Mutex<RouteState>,
    host_tools: Sender<HostToolRequest>,
}

struct RouteState {
    agent: HashMap<contract::SessionId, Sender<ProviderEvent>>,
    terminal: HashMap<Uuid, Sender<TerminalUpdate>>,
    pending_session_list: Option<Sender<Vec<wire::SessionSummary>>>,
    failure: Option<String>,
}

impl Routes {
    pub(super) fn new(host_tools: Sender<HostToolRequest>) -> Self {
        Self {
            state: Mutex::new(RouteState {
                agent: HashMap::new(),
                terminal: HashMap::new(),
                pending_session_list: None,
                failure: None,
            }),
            host_tools,
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

    pub(super) fn set_pending_session_list(&self, sender: Sender<Vec<wire::SessionSummary>>) {
        let mut state = self.state.lock().unwrap();
        if state.failure.is_some() {
            let _ = sender.send(Vec::new());
            return;
        }
        state.pending_session_list = Some(sender);
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
            EnvelopeBody::Control(Control::HostToolRequest(request)) => {
                let _ = self.host_tools.send(request);
            }
            EnvelopeBody::Control(Control::SessionListResult(summaries)) => {
                if let Some(reply) = self.state.lock().unwrap().pending_session_list.take() {
                    let _ = reply.send(summaries);
                }
            }
            EnvelopeBody::Control(_) | EnvelopeBody::Command(_) => {}
        }
        Ok(Incoming::Handled)
    }

    pub(super) fn connection_failed(&self, message: String) {
        let (terminal_routes, agent_routes, pending) = {
            let mut state = self.state.lock().unwrap();
            state.failure = Some(message.clone());
            let terminal_routes = state.terminal.values().cloned().collect::<Vec<_>>();
            let agent_routes = state.agent.values().cloned().collect::<Vec<_>>();
            let pending = state.pending_session_list.take();
            (terminal_routes, agent_routes, pending)
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
            let _ = reply.send(Vec::new());
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
