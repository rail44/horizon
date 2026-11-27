//! `board_session`, on a host-provided capability so the agent runtime and
//! board data crate remain independent. The board's other tools reach a
//! session as an MCP server its client names (`horizon mcp board`).

use serde_json::{Value, json};

use super::execution::ToolOutput;
use crate::contract::{Event, SessionId, ToolCallRequest};
use crate::tools::error_output;
use crate::tools::state::ToolSessionState;

pub(crate) fn session_schema() -> Value {
    json!({"type":"object", "additionalProperties":false, "required":["action"],
    "properties": {
        "action":{"type":"string","enum":["consult","implement","review","send"]},
        "id":{"type":"integer","minimum":1},
        "text":{"type":"string"},
        "base":{"type":"string"},"tip":{"type":"string"},
        "checks":{"type":"string"},
        "session_id":{"type":"string"},
        "reply_to":{"type":["string","null"],"description":"Session UUID for a requested reply. Omit for a passive notification."}
    }})
}

/// The host capability `board_session` is built on. Implemented by
/// `horizon-board-flow` (`AgentHostBoardHost`) and installed on each
/// session's `ToolSessionState` via [`ToolSessionState::with_board_host`].
/// Synchronous: the host blocks on its own async work internally.
pub trait BoardHost: Send + Sync {
    /// Performs a task-session operation (`board_session`). The host
    /// supplies board types and session routing; this crate keeps the two
    /// domains independent. Caller identity comes from the tool runtime.
    /// Outgoing requests are returned as events so the runtime persists them
    /// before returning the tool result to the provider.
    fn operate(
        &self,
        _session: SessionId,
        _request: &ToolCallRequest,
    ) -> Result<(Value, Vec<Event>), String> {
        Err("Board operations are unavailable for this session".into())
    }
}

pub(crate) fn execute_operation(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> ToolOutput {
    let result = tool_state
        .board_host()
        .ok_or_else(|| "No board host is installed".to_string())
        .and_then(|host| host.operate(session_id, request));
    let (output, events) = result.unwrap_or_else(|error| (error_output(error), Vec::new()));
    with_events(request, output, events)
}

fn with_events(_request: &ToolCallRequest, output: Value, events: Vec<Event>) -> ToolOutput {
    ToolOutput {
        output: super::output::Response::external(output),
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{SessionInput, ToolCallId};
    use std::sync::Arc;

    struct SendingHost(SessionId);
    impl BoardHost for SendingHost {
        fn operate(
            &self,
            _: SessionId,
            _: &ToolCallRequest,
        ) -> Result<(Value, Vec<Event>), String> {
            Ok((
                json!({"queued": true}),
                vec![Event::SessionInputSent {
                    session_id: self.0,
                    input: SessionInput {
                        id: "review-request".into(),
                        origin: "requester".into(),
                        text: "Review the changes".into(),
                        reply_to: None,
                        resume_work: false,
                    },
                }],
            ))
        }
    }

    #[test]
    fn operation_returns_its_outbox_with_the_output_for_the_coordinator() {
        let recipient = SessionId::new();
        let state = ToolSessionState::without_root()
            .with_board_host(Some(Arc::new(SendingHost(recipient))));
        let request = ToolCallRequest {
            call_id: ToolCallId("request".into()),
            tool_id: "board_session".into(),
            occurrence_id: crate::contract::OccurrenceId::new(),
            input: json!({"action":"review","id":1}).into(),
        };
        let result = execute_operation(&state, SessionId::new(), &request);
        assert_eq!(result.output.to_json(), json!({"queued": true}));
        std::assert_matches!(
            result.events.as_slice(), [Event::SessionInputSent { session_id, input }]
            if *session_id == recipient && input.id == "review-request"
        );
    }
}
