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
    role_id: Option<horizon_agent::roles::RoleId>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    agentd_connection: RwSignal<Option<AgentdConnection>>,
    config_reload_requests: RwSignal<u64>,
) {
    let Some(connection) = agentd_connection.get_untracked() else {
        surface_agent_runtime_unavailable(session_id, frames, agent_state_status);
        return;
    };

    let provider_id = agent::ProviderRegistry::default().default_provider_id();
    // `role_id` is `Some(..)` only for the `New Configuration Agent`
    // command today (`app::command_actions`); every other spawn path stays
    // role-less -- see `horizon_agent::roles` for what a role changes.
    let handle = connection.start_session(session_id.into(), provider_id, role_id);
    fold_agent_session_events(session_id, handle, frames, sessions, config_reload_requests);
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
    // `with_untracked`, not `update` -- see `agentd_runtime::
    // fold_agent_session_events`'s matching comment for why an agent-frame
    // write must not also notify the outer `RwSignal<Frames>`.
    frames.with_untracked(|frames| {
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
            None,
            frames,
            sessions,
            agent_state_status,
            agentd_connection,
            RwSignal::new(0_u64),
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
            None,
            frames,
            sessions,
            agent_state_status,
            agentd_connection,
            RwSignal::new(0_u64),
        );

        assert!(
            sessions
                .with_untracked(|registry| registry.agent_sender(session_id))
                .is_some(),
            "a connected agentd should register the session in the registry"
        );
    }
}
