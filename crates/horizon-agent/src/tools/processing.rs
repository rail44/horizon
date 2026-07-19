use crate::contract::{Command, Event, ProviderEvent, SessionId};
use crate::policy::horizon_events_for_provider_event;
use crate::tools::bash;
use crate::tools::state::ToolSessionState;
use crate::tools::{execution::execute_agent_tool, Execution, HostTools};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Processing {
    pub horizon_events: Vec<ProviderEvent>,
    pub provider_commands: Vec<Command>,
}

pub fn process_agent_provider_event(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    session_id: SessionId,
    provider_event: impl Into<ProviderEvent>,
) -> Processing {
    let provider_event = provider_event.into();

    // Ephemeral tool-call progress (`ProviderEvent::tool_call_progress`)
    // carries a placeholder `event` — see its doc comment — so it must not
    // reach the approval/bash-kill/tool-execution logic below, which
    // assumes `event` is real. Pass it through untouched; `LiveState` folds
    // it into the frame and keeps it out of the persisted log.
    if provider_event.tool_call_progress.is_some() {
        return Processing {
            horizon_events: vec![provider_event],
            provider_commands: Vec::new(),
        };
    }

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

    let mut horizon_events = horizon_events_for_provider_event(&event, tool_state)
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            if index == 0 {
                ProviderEvent {
                    event,
                    provider_payload: provider_event.provider_payload.clone(),
                    tool_call_progress: None,
                    session_model: None,
                }
            } else {
                event.into()
            }
        })
        .collect::<Vec<_>>();
    let mut provider_commands = Vec::new();

    if let Event::ToolCallRequested(request) = &event {
        match execute_agent_tool(host, tool_state, session_id, request) {
            // `Denied`/`Unknown` join `Auto` here: any of the three can
            // resolve a call synchronously with a real `ToolCallFinished`
            // (today, only `Unknown`'s does -- an unrecognized tool id --
            // see `execute_agent_tool`'s doc comment), and that result must
            // reach the provider as a `Command::ToolCallResult` exactly the
            // same way, or the model never learns the call finished and the
            // turn stalls waiting on a result that never arrives -- this was
            // the second half of the 2026-07-19 dogfooding bug (the first
            // half was `policy::horizon_events_for_provider_event` routing
            // an unknown tool id through `ApprovalRequested` at all).
            Execution::Auto(events) | Execution::Denied(events) | Execution::Unknown(events) => {
                for result_event in &events {
                    if let Event::ToolCallFinished(result) = result_event {
                        provider_commands.push(Command::ToolCallResult(result.clone()));
                    }
                }
                horizon_events.extend(events.into_iter().map(ProviderEvent::from));
            }
            Execution::Started(events) => {
                // A tier-1-contained `bash` call moved to the background
                // thread (`horizon_sandbox`) -- no `Command::ToolCallResult`
                // yet, exactly like a manually approved bash call's
                // `ApprovalOutcome::Started`. The eventual result (or a
                // retry-without-sandbox prompt) arrives later on the
                // session's `bash_results` channel and is folded by
                // `fold_bash_completion` in `horizon-sessiond`'s session
                // loop.
                horizon_events.extend(events.into_iter().map(ProviderEvent::from));
            }
            Execution::RequiresApproval => {}
        }
    }

    Processing {
        horizon_events,
        provider_commands,
    }
}
