use crate::agent::contract::{ApprovalRequest, Error, Event, SessionState, ToolPermission};

pub(crate) fn horizon_events_for_provider_event(event: &Event) -> Vec<Event> {
    let mut events = vec![event.clone()];
    if let Event::ToolCallRequested(request) = event {
        match crate::agent::tools::permission_for_tool(&request.tool_id)
            .unwrap_or(ToolPermission::RequireApproval)
        {
            ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi => {}
            ToolPermission::RequireApproval => {
                events.push(Event::ApprovalRequested(ApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: format!(
                        "`{}` requested Horizon approval for this tool call.",
                        request.tool_id
                    ),
                }));
                events.push(Event::StateChanged(SessionState::WaitingForApproval));
            }
            ToolPermission::Deny => {
                events.push(Event::Error(Error {
                    message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
                }));
            }
        }
    }

    events
}
