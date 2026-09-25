use super::processing::Processing;
use super::*;
use crate::contract::{Event, ProviderEvent, SessionId, ToolCallRequest};
use crate::live::LiveState;

fn live(session_id: SessionId) -> LiveState {
    state::session_runtime(session_id)
        .map(|runtime| runtime.live_state)
        .unwrap_or_else(LiveState::with_disabled_persistence)
}

pub(crate) fn execute_agent_tool(
    host: &dyn HostTools,
    state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    super::execute_agent_tool(host, state, session_id, &live(session_id), request).unwrap()
}

pub(crate) fn process_agent_provider_event(
    host: &dyn HostTools,
    state: &ToolSessionState,
    session_id: SessionId,
    event: impl Into<ProviderEvent>,
) -> Processing {
    super::process_agent_provider_event(host, state, session_id, &live(session_id), event).unwrap()
}

pub(crate) fn policy_events(
    event: &Event,
    state: &ToolSessionState,
    _session: SessionId,
) -> Vec<Event> {
    let mut events = vec![event.clone()];
    if let Event::ToolCallRequested(request) = event {
        if let crate::policy::ToolPlan::Approval(approval) =
            crate::policy::plan_tool_call(state, request)
        {
            events.extend([
                Event::ApprovalRequested(approval),
                Event::StateChanged(crate::contract::SessionState::WaitingForApproval),
            ]);
        }
    }
    events
}
