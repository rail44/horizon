//! Coordinate one provider event with its tool plan and acknowledged lifecycle.
use super::{execute_agent_tool, Execution, HostTools, ToolSessionState, ToolUpdate};
use crate::contract::{Command, Event, ProviderEvent, SessionId};
use crate::judge::ApprovalCandidate;
use crate::live::LiveState;

#[derive(Debug)]
pub struct Processing {
    /// Already persisted and applied. Publish without appending a second time.
    pub horizon_events: Vec<ProviderEvent>,
    pub provider_commands: Vec<Command>,
    pub approval: Option<ApprovalCandidate>,
}

pub fn process_agent_provider_event(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    session_id: SessionId,
    live: &LiveState,
    provider_event: impl Into<ProviderEvent>,
) -> Result<Processing, String> {
    let provider_event = provider_event.into();
    live.extend_provider_events([provider_event.clone()])?;
    let mut processing = Processing {
        horizon_events: vec![provider_event.clone()],
        provider_commands: Vec::new(),
        approval: None,
    };
    match provider_event.as_event() {
        Some(Event::ToolCallRequested(request)) => {
            match execute_agent_tool(host, tool_state, session_id, live, request)? {
                Execution::AwaitApproval(candidate) => processing.approval = Some(*candidate),
                Execution::Applied(update) => {
                    let events = match update {
                        ToolUpdate::Started { events } => events,
                        ToolUpdate::Finished { events, result } => {
                            processing
                                .provider_commands
                                .push(Command::ToolCallResult(result));
                            events
                        }
                    };
                    processing
                        .horizon_events
                        .extend(events.into_iter().map(Into::into));
                }
            }
        }
        // Task children are session-owned and survive turn cancellation.
        Some(Event::ToolCallFinished(result)) => super::cancel_tool_execution(
            session_id,
            &crate::contract::ToolCallIdentity {
                call_id: result.call_id.clone(),
                occurrence_id: result.occurrence_id.clone(),
            },
        ),
        _ => {}
    }
    Ok(processing)
}
