//! Client attachment state is transport state, separate from the agent turn.

use super::routing::{AgentRoutes, RouteKey};
use horizon_agent::contract::{Command, ProviderEvent, SessionId};
use horizon_agent::wire::{AgentWireEvent, AttachmentEnd};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AttachmentState {
    #[default]
    Connecting,
    Restoring,
    Ready,
    Failed(String),
    Disconnected(String),
}

#[derive(Clone, Debug)]
pub(crate) enum AgentUpdate {
    Event(Box<ProviderEvent>),
    State(AttachmentState),
}

impl AttachmentState {
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub(crate) fn is_closed(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Disconnected(_))
    }

    /// Returns true for a control marker. Reject malformed ordering instead
    /// of folding a partial or duplicated replay as a usable conversation.
    pub(super) fn observe(&mut self, event: &AgentWireEvent) -> Result<bool, String> {
        match (&self, event) {
            (Self::Connecting, AgentWireEvent::ReplayStarted) => *self = Self::Restoring,
            (Self::Restoring, AgentWireEvent::ReplayComplete) => *self = Self::Ready,
            (_, AgentWireEvent::AttachmentClosed(reason)) => {
                *self = match reason {
                    AttachmentEnd::Lagged => Self::Failed("Session updates exceeded the connection buffer; reopen the session to restore its history".into()),
                    AttachmentEnd::Replaced => Self::Disconnected("Session opened by another attachment".into()),
                    AttachmentEnd::Detached => Self::Disconnected("Session detached".into()),
                    AttachmentEnd::SessionEnded => Self::Disconnected("Session runtime ended".into()),
                };
            }
            (_, AgentWireEvent::ReplayStarted | AgentWireEvent::ReplayComplete) => {
                return Err("Unexpected session replay boundary".into())
            }
            (Self::Restoring | Self::Ready, _) => return Ok(false),
            _ => return Err("Session update arrived outside an active attachment".into()),
        }
        Ok(true)
    }

    pub(crate) fn stream_ended(&mut self) {
        if !self.is_closed() {
            *self = if self.is_ready() {
                Self::Disconnected("Session connection closed; reopen the session".into())
            } else {
                Self::Failed(
                    "Session history restoration was interrupted; reopen the session to retry"
                        .into(),
                )
            };
        }
    }
}

/// One live agent attachment: forwards handle commands to the daemon and
/// routes events to the pane, until either side goes away.
pub(super) async fn run(
    routes: Arc<AgentRoutes>,
    route: RouteKey<SessionId>,
    attachment: horizon_agent::wire::AgentAttachment,
    mut commands: UnboundedReceiver<Command>,
) {
    let horizon_agent::wire::AgentAttachment {
        mut events,
        commands: remote_commands,
    } = attachment;
    let mut phase = AttachmentState::Connecting;
    loop {
        tokio::select! {
            command = commands.recv(), if phase.is_ready() => match command {
                Some(command) => {
                    if let Err(error) = remote_commands.send(command).await {
                        phase = AttachmentState::Failed(format!("Failed to send session command: {error}"));
                        break;
                    }
                }
                None => break,
            },
            event = events.recv() => match event {
                Ok(Some(event)) => match phase.observe(&event) {
                    Ok(true) => {
                        if !routes.send_agent(route, AgentUpdate::State(phase.clone())).await || phase.is_closed() { break; }
                    }
                    Ok(false) => { if !routes.route_agent_event(route, event).await { break; } },
                    Err(error) => { phase = AttachmentState::Failed(error); break; }
                },
                Ok(None) => break,
                // Skipping even one history event would falsely declare an
                // incomplete replay ready. Every decode error ends attachment.
                Err(error) => { phase = AttachmentState::Failed(format!("Failed to read session update: {error}")); break; }
            },
            _ = tokio::time::sleep(Duration::from_secs(120)), if !phase.is_ready() => {
                phase = AttachmentState::Failed("Timed out restoring session history".into());
                break;
            },
        }
    }
    phase.stream_ended();
    routes.send_agent(route, AgentUpdate::State(phase)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::contract::{Event, SessionState};

    fn update() -> AgentWireEvent {
        AgentWireEvent::Event(Event::StateChanged(SessionState::Running))
    }

    #[test]
    fn only_the_explicit_boundary_makes_an_attachment_ready() {
        let mut state = AttachmentState::Connecting;
        assert!(state.observe(&update()).is_err());
        assert!(state.observe(&AgentWireEvent::ReplayComplete).is_err());
        assert!(state.observe(&AgentWireEvent::ReplayStarted).unwrap());
        assert!(!state.observe(&update()).unwrap());
        assert!(!state.is_ready());
        assert!(state.observe(&AgentWireEvent::ReplayStarted).is_err());
        assert!(state.observe(&AgentWireEvent::ReplayComplete).unwrap());
        assert!(state.is_ready());
        assert!(!state.observe(&update()).unwrap());
        assert!(state.observe(&AgentWireEvent::ReplayComplete).is_err());
    }

    #[test]
    fn incomplete_replay_and_live_disconnect_have_distinct_outcomes() {
        for initial in [AttachmentState::Connecting, AttachmentState::Restoring] {
            let mut state = initial;
            state.stream_ended();
            assert!(matches!(state, AttachmentState::Failed(_)));
        }
        let mut state = AttachmentState::Ready;
        state.stream_ended();
        assert!(matches!(state, AttachmentState::Disconnected(_)));
        assert!(state.observe(&update()).is_err());
        let mut state = AttachmentState::Ready;
        state
            .observe(&AgentWireEvent::AttachmentClosed(AttachmentEnd::Lagged))
            .unwrap();
        let failure = state.clone();
        state.stream_ended();
        assert_eq!(state, failure);
    }
}
