use crate::agent::contract::{Command, Event, ProviderEvent};
use crate::agent::policy::horizon_events_for_provider_event;
use crate::agent::tools::{execution::execute_agent_tool, Execution};
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Processing {
    pub(crate) horizon_events: Vec<ProviderEvent>,
    pub(crate) provider_commands: Vec<Command>,
}

pub(crate) fn process_agent_provider_event(
    workspace: &Workspace,
    provider_event: impl Into<ProviderEvent>,
) -> Processing {
    let provider_event = provider_event.into();
    let event = provider_event.event.clone();
    let mut horizon_events = horizon_events_for_provider_event(&event)
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            if index == 0 {
                ProviderEvent {
                    event,
                    provider_payload: provider_event.provider_payload.clone(),
                }
            } else {
                event.into()
            }
        })
        .collect::<Vec<_>>();
    let mut provider_commands = Vec::new();

    if let Event::ToolCallRequested(request) = &event {
        match execute_agent_tool(workspace, request) {
            Execution::Auto(events) => {
                for result_event in &events {
                    if let Event::ToolCallFinished(result) = result_event {
                        provider_commands.push(Command::ToolCallResult(result.clone()));
                    }
                }
                horizon_events.extend(events.into_iter().map(ProviderEvent::from));
            }
            Execution::RequiresApproval => {}
            Execution::Denied(events) | Execution::Unknown(events) => {
                horizon_events.extend(events.into_iter().map(ProviderEvent::from));
            }
        }
    }

    Processing {
        horizon_events,
        provider_commands,
    }
}
