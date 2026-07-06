use floem::prelude::*;

use crate::agent::agentd_runtime::{fold_agent_session_events, AgentdConnection};
use crate::agent::contract as agent;
use crate::agent::frame::{AgentFrame, AgentFrameItem};
use crate::session::{Frames, Registry, SessionId};

/// The only place agent sessions run as of step 4
/// (`docs/agent-runtime-split-design.md` retired the in-process fallback
/// step 3 kept behind `[agent].agentd`): asks `horizon-agentd` to host the
/// session (`AgentdConnection::start_session`, which sends `session_new` and
/// hands back a `SessionHandle`) and folds its event stream into the frame
/// the pane renders via [`fold_agent_session_events`] -- the same fold a
/// reconnected/resumed session's handle goes through
/// (`agent::agentd_runtime::attach_sessions`), because "the fold must not
/// know which transport delivered the events, or whether this session's
/// history predates this connection".
///
/// `agentd_connection` is `None` only when the startup connection (or the
/// last `Reload Agent Runtime`) failed; there is no in-process fallback
/// left to route through in that case, so the pane gets an explanatory
/// error frame instead of silently doing nothing.
pub(super) fn spawn_agent_session(
    session_id: SessionId,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    agentd_connection: RwSignal<Option<AgentdConnection>>,
) {
    let Some(connection) = agentd_connection.get_untracked() else {
        surface_agent_runtime_unavailable(session_id, frames, agent_state_status);
        return;
    };

    let provider_id = agent::ProviderRegistry::default().default_provider_id();
    // Role-less for every spawn path today -- the GUI has no role-picking
    // command yet (see `docs/plans/agent-foundation/03-roles-and-config-agent.md`;
    // a future "New Configuration Agent" command is the intended caller of
    // a `Some(..)` role here).
    let handle = connection.start_session(session_id.into(), provider_id, None);
    fold_agent_session_events(session_id, handle, frames, sessions);
}

/// The pane-facing side of "agentd is unreachable": an explanatory error
/// frame (so the pane isn't just silently blank) plus the same
/// `agent_state_status` message a failed startup connect or a failed
/// `Reload Agent Runtime` already latches, in case this pane opens before
/// either of those got a chance to.
fn surface_agent_runtime_unavailable(
    session_id: SessionId,
    frames: RwSignal<Frames>,
    agent_state_status: RwSignal<Option<String>>,
) {
    let message =
        "Agent runtime unavailable -- use \"Reload Agent Runtime\" to reconnect.".to_string();
    frames.update(|frames| {
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: None,
                items: vec![AgentFrameItem::Error(agent::Error {
                    message: message.clone(),
                })],
            },
        );
    });
    agent_state_status.set(Some(message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_agent_session_surfaces_an_error_frame_when_agentd_is_unavailable() {
        let session_id = SessionId::new();
        let frames = RwSignal::new(Frames::default());
        let sessions = RwSignal::new(Registry::default());
        let agent_state_status = RwSignal::new(None::<String>);
        let agentd_connection: RwSignal<Option<AgentdConnection>> = RwSignal::new(None);

        spawn_agent_session(
            session_id,
            frames,
            sessions,
            agent_state_status,
            agentd_connection,
        );

        let frame = frames.with_untracked(|frames| frames.agent_frame(session_id));
        assert!(
            matches!(frame.items.as_slice(), [AgentFrameItem::Error(_)]),
            "expected a single error item, got: {:?}",
            frame.items
        );
        assert!(
            agent_state_status.get_untracked().is_some(),
            "the unavailable-runtime message must also latch into agent_state_status"
        );
        assert!(
            sessions
                .with_untracked(|registry| registry.agent_sender(session_id))
                .is_none(),
            "no session should be registered when agentd is unavailable"
        );
    }

    #[test]
    fn spawn_agent_session_routes_through_agentd_when_connected() {
        let session_id = SessionId::new();
        let frames = RwSignal::new(Frames::default());
        let sessions = RwSignal::new(Registry::default());
        let agent_state_status = RwSignal::new(None::<String>);
        let agentd_connection = RwSignal::new(Some(AgentdConnection::for_test()));

        spawn_agent_session(
            session_id,
            frames,
            sessions,
            agent_state_status,
            agentd_connection,
        );

        assert!(
            sessions
                .with_untracked(|registry| registry.agent_sender(session_id))
                .is_some(),
            "a connected agentd should register the session in the registry"
        );
    }
}
