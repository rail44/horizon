//! Board tools use a daemon-provided capability so the agent runtime and
//! board data crate remain independent. Tool calls carry the actual session
//! identity; models cannot impersonate another author. Operating policy lives
//! in the board skills, while the host validates ordinary data operations.

use serde_json::{json, Value};

use crate::contract::{Event, SessionId, SessionState, ToolCallRequest, ToolCallResult};
use crate::tools::error_output;
use crate::tools::state::ToolSessionState;
use crate::tools::Execution;

pub(crate) fn update_schema() -> Value {
    json!({"type":"object", "additionalProperties":false, "required":["action"],
    "properties": {
        "action":{"type":"string","enum":["add","edit","parent","dependencies","move","status","complete"]},
        "id":{"type":"integer","minimum":1},
        "title":{"type":"string"},"body":{"type":"string"},
        "parent":{"type":["integer","null"]},
        "depends_on":{"type":"array","items":{"type":"integer","minimum":1}},
        "position":{"type":"string","enum":["first","last","before","after"]},
        "relative_to":{"type":"integer","minimum":1},
        "status":{"type":"string"},"completed":{"type":"boolean"}
    }})
}

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

/// The daemon capability `board.read` and `board.comment` are built on: read
/// the board (list items or show one) and append a comment. Implemented by
/// `horizon-agentd` (`session::AgentdBoardHost`) using `horizon_board::Store`
/// and installed on each session's `ToolSessionState` via
/// [`ToolSessionState::with_board_host`].
///
/// Methods are synchronous: board reads are file folds, and board writes
/// (comment) go through `horizon-logd` via a tokio round-trip the daemon
/// implementation blocks on internally. The caller (the session thread) is
/// never async, so the trait stays sync — mirroring how `ExplorationHost`
/// presents a sync interface despite the daemon's async internals.
///
/// Returns `serde_json::Value` for reads so this crate never imports
/// `horizon-board`'s `Item`/`ListResult` types. The daemon serializes board
/// types to JSON; the tool executor passes them straight through to the model.
pub trait BoardHost: Send + Sync {
    /// Lists board items in rank order, optionally filtered by status.
    /// Returns a JSON array of task records.
    fn list(&self, status_filter: Option<&str>) -> Result<Value, String>;

    /// Shows one item with its full comment thread, or `None` if the id
    /// doesn't exist. Returns a JSON `Item` or `null`.
    fn show(&self, id: u64) -> Result<Value, String>;

    /// Appends a comment to item `id`. `author` is set by the caller (the
    /// daemon, from the session id) — the model never controls the author
    /// field. `Err` carries a message suitable for the model to read.
    fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), String>;

    /// Performs an ordinary task update or a task-session operation. The
    /// daemon supplies board types and session routing; this crate keeps the
    /// two domains independent. Caller identity comes from the tool runtime.
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

/// Executes an auto-allowed board read tool (`board.read`). Returns `None` for
/// any other tool id, so the caller can try elsewhere — same contract as
/// `tools::knowledge::execute_auto`.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    if tool_id != "board.read" {
        return None;
    }
    let Some(host) = tool_state.board_host() else {
        return Some(error_output(
            "board.read is not available: no board host is installed for this session",
        ));
    };
    // If `id` is present, show that item; otherwise list all (optionally
    // filtered by `status`). One tool, two operations — keeps the role
    // allowlist short and the model's surface simple.
    if let Some(id) = input.get("id").and_then(Value::as_u64) {
        match host.show(id) {
            Ok(value) => Some(value),
            Err(message) => Some(error_output(message)),
        }
    } else {
        let status_filter = input
            .get("status")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        match host.list(status_filter.as_deref()) {
            Ok(value) => Some(value),
            Err(message) => Some(error_output(message)),
        }
    }
}

/// Executes `board.comment` — a write, but `AutoAllowRead` so it skips the
/// approval gate (the event log is the audit trail, same as `knowledge.write`).
/// Special-cased in `execute_agent_tool` (not routed through
/// `execute_auto_tool`) because it needs the session id for the comment author,
/// which `execute_auto_tool`'s signature doesn't carry.
pub(crate) fn execute_comment(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let Some(host) = tool_state.board_host() else {
        return synchronous(
            request,
            error_output(
                "board.comment is not available: no board host is installed for this session",
            ),
        );
    };
    let Some(id) = request.input.get("id").and_then(Value::as_u64) else {
        return synchronous(
            request,
            error_output("board.comment requires an `id` integer argument"),
        );
    };
    let Some(text) = request.input.get("text").and_then(Value::as_str) else {
        return synchronous(
            request,
            error_output("board.comment requires a `text` string argument"),
        );
    };
    // The author is the session id — the model never controls it, so a
    // board session cannot impersonate the owner or another session.
    let author = format!("session:{}", session_id.as_uuid());
    let output = match host.comment(id, &author, text) {
        Ok(()) => json!({ "ok": true }),
        Err(message) => error_output(message),
    };
    synchronous(request, output)
}

pub(crate) fn execute_operation(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let result = tool_state
        .board_host()
        .ok_or_else(|| "No board host is installed".to_string())
        .and_then(|host| host.operate(session_id, request));
    let (output, events) = result.unwrap_or_else(|error| (error_output(error), Vec::new()));
    with_events(request, output, events)
}

fn synchronous(request: &ToolCallRequest, output: Value) -> Execution {
    with_events(request, output, Vec::new())
}

fn with_events(request: &ToolCallRequest, output: Value, events: Vec<Event>) -> Execution {
    let mut batch = vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
    ];
    batch.extend(events);
    batch.push(Event::ToolCallFinished(ToolCallResult::new(
        request.call_id.clone(),
        request.occurrence_id.clone(),
        output,
    )));
    Execution::Auto(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{SessionInput, ToolCallId};
    use std::sync::Arc;

    struct SendingHost(SessionId);
    impl BoardHost for SendingHost {
        fn list(&self, _: Option<&str>) -> Result<Value, String> {
            unreachable!()
        }
        fn show(&self, _: u64) -> Result<Value, String> {
            unreachable!()
        }
        fn comment(&self, _: u64, _: &str, _: &str) -> Result<(), String> {
            unreachable!()
        }
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
    fn operation_returns_its_outbox_before_reporting_success() {
        let recipient = SessionId::new();
        let state = ToolSessionState::without_root()
            .with_board_host(Some(Arc::new(SendingHost(recipient))));
        let request = ToolCallRequest {
            call_id: ToolCallId("request".into()),
            tool_id: "board.session".into(),
            occurrence_id: None,
            input: json!({"action":"review","id":1}).into(),
        };
        let Execution::Auto(events) = execute_operation(&state, SessionId::new(), &request) else {
            panic!("board requests must return a persistable automatic batch");
        };
        let send = events.iter().position(|event| matches!(event,
            Event::SessionInputSent { session_id, input } if *session_id == recipient && input.id == "review-request"
        )).expect("durable outgoing request");
        let completion = events.iter().position(|event| matches!(event,
            Event::ToolCallFinished(result) if result.output.get("queued") == Some(&json!(true))
        )).expect("successful tool result");
        assert!(
            send < completion,
            "a replayable request must precede provider success"
        );
    }
}
