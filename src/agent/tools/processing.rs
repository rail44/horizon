use crate::agent::contract::{Command, Event, ProviderEvent};
use crate::agent::policy::horizon_events_for_provider_event;
use crate::agent::tools::bash;
use crate::agent::tools::state::ToolSessionState;
use crate::agent::tools::{execution::execute_agent_tool, Execution};
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Processing {
    pub(crate) horizon_events: Vec<ProviderEvent>,
    pub(crate) provider_commands: Vec<Command>,
}

pub(crate) fn process_agent_provider_event(
    workspace: &Workspace,
    tool_state: &ToolSessionState,
    provider_event: impl Into<ProviderEvent>,
) -> Processing {
    let provider_event = provider_event.into();
    let event = provider_event.event.clone();

    // A provider-originated `ToolCallFinished` is the turn-cancellation (or
    // loop-guard-halt) signal for any call still pending on the provider's
    // side, `bash` included (see `docs/agent-tools-design.md`, "Bash
    // Semantics": "Cancelling a turn kills the process group of any
    // in-flight command"). This never fires for `bash`'s own genuine
    // completion — that's delivered straight to the UI thread over
    // `SessionRuntime::bash_results`, bypassing this function entirely — so
    // it only ever needs to kill a call that's still actually running.
    // A miss (not a bash call, or already finished) is a harmless no-op.
    if let Event::ToolCallFinished(result) = &event {
        bash::kill_if_running(&result.call_id);
    }

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
        match execute_agent_tool(workspace, tool_state, request) {
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
