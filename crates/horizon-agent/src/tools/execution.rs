use super::input::PreparedCall;
use super::output::{Response, error as error_output};
use super::tool::{Dispatch, RunContext};
use super::transition::ToolUpdate;
use crate::contract::{Event, SessionId, ToolCallRequest, ToolCallResult};
use crate::judge::ApprovalCandidate;
use crate::live::LiveState;
use crate::policy::{ToolPlan, plan_prepared_call};
use crate::tools::state::ToolSessionState;
use serde_json::Value;

/// Boundary for tools needing shell-owned state, such as `workspace_snapshot`.
/// The daemon supplies its host-channel adapter; implementations return `None`
/// only for unrecognized ids. Local synchronous tools use the closed registry.
pub trait HostTools {
    /// Executes a host-owned auto-allow tool, returning `None` if `tool_id`
    /// isn't one this implementation handles.
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value>;

    /// Requests that `session` move into a fresh worktree branched from its
    /// current `HEAD` (`worktree_enter`), returning the resolved base commit.
    /// `Err` carries a message for the model, and nothing is requested.
    fn enter_worktree(&self, _session: SessionId) -> Result<String, String> {
        Err("worktree_enter is unavailable for this session".into())
    }
}

/// A policy-approved execution or the exact candidate requiring a decision.
#[derive(Clone, Debug, PartialEq)]
pub enum Execution {
    Applied(ToolUpdate),
    AwaitApproval(Box<ApprovalCandidate>),
}

/// Tool-specific output, with domain records that must precede its result.
/// Lifecycle events are owned by ToolUpdate rather than individual handlers.
pub(crate) struct ToolOutput {
    pub output: Response,
    pub events: Vec<Event>,
}

impl From<Value> for ToolOutput {
    fn from(output: Value) -> Self {
        Self {
            output: Response::external(output),
            events: Vec::new(),
        }
    }
}

impl From<Response> for ToolOutput {
    fn from(output: Response) -> Self {
        Self {
            output,
            events: Vec::new(),
        }
    }
}

pub fn execute_agent_tool(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    session_id: SessionId,
    live: &LiveState,
    request: &ToolCallRequest,
) -> Result<Execution, String> {
    let prepared = match PreparedCall::new(request) {
        Ok(prepared) => prepared,
        Err(message) => {
            return ToolUpdate::finish(live, request.identity().finish(error_output(message)))
                .map(Execution::Applied);
        }
    };
    match plan_prepared_call(tool_state, &prepared) {
        ToolPlan::Approval(approval) => Ok(Execution::AwaitApproval(Box::new(ApprovalCandidate {
            request: request.clone(),
            approval: *approval,
        }))),
        ToolPlan::Reject(output) => {
            ToolUpdate::finish(live, request.identity().finish(output)).map(Execution::Applied)
        }
        ToolPlan::Automatic => {
            execute_automatic(host, tool_state, session_id, live, &prepared).map(Execution::Applied)
        }
    }
}

fn execute_automatic(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    session_id: SessionId,
    live: &LiveState,
    request: &PreparedCall<'_>,
) -> Result<ToolUpdate, String> {
    // Every dispatch, including a synchronous effect, starts beyond this acknowledged boundary.
    let started = ToolUpdate::start(live, request, None)?;
    let Some(tool) = super::tool::find(tool_state.packages(), &request.tool_id) else {
        return started.complete(
            live,
            request.identity().finish(error_output(format!(
                "Tool `{}` cannot be executed automatically.",
                request.tool_id
            ))),
            Vec::new(),
        );
    };
    let cx = RunContext {
        host,
        state: tool_state,
        session_id,
    };
    match tool.run(&cx, request) {
        Dispatch::Done(output) => started.complete(
            live,
            request.identity().finish(output.output),
            output.events,
        ),
        Dispatch::Spawned => Ok(started),
    }
}

/// The mock provider's acknowledgement of a tool result.
#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn tool_result_message(result: &ToolCallResult) -> Event {
    Event::MessageCommitted(crate::contract::Message {
        role: crate::contract::MessageRole::Assistant,
        text: format!("Tool result received for {}.", result.call_id.0),
    })
}

/// Finish one cancelled execution with the identity of its original request.
pub fn cancelled_tool_call_result(identity: crate::contract::ToolCallIdentity) -> ToolCallResult {
    ToolCallResult::cancelled(identity)
}

/// Stop turn-owned asynchronous work before recording its cancellation. Task
/// children are session-owned and deliberately remain running.
pub fn cancel_tool_execution(session_id: SessionId, identity: &crate::contract::ToolCallIdentity) {
    super::background::cancel_call(session_id, identity);
    super::web::clear_approved_domains(session_id, identity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OccurrenceId, ToolCallId};
    use crate::persistence::event_log::{WriterHandle, WriterInit};
    use crate::tools::{register_session_runtime, unregister_session_runtime};

    struct MustNotRun;
    impl HostTools for MustNotRun {
        fn execute_auto(&self, _: &str, _: &Value) -> Option<Value> {
            panic!("host effect before a persisted start");
        }
    }

    #[test]
    fn invalid_builtin_inputs_finish_before_approval_start_or_host_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tools::ToolSessionBuilder::new(dir.path().to_path_buf()).build();
        for (id, mut input) in crate::tools::input::tests::samples() {
            input["unexpected"] = serde_json::json!(true);
            let request = ToolCallRequest {
                call_id: ToolCallId(id.into()),
                occurrence_id: OccurrenceId::new(),
                tool_id: id.into(),
                input: input.into(),
            };
            let live = LiveState::with_disabled_persistence();
            let Execution::Applied(ToolUpdate::Finished { events, result }) =
                execute_agent_tool(&MustNotRun, &state, SessionId::new(), &live, &request).unwrap()
            else {
                panic!("{id} must reject invalid input before asking for approval or starting");
            };
            assert!(result.is_error(), "{id}");
            assert_eq!(result.occurrence_id, request.occurrence_id);
            assert!(
                !events.iter().any(|event| matches!(
                    event,
                    Event::ToolCallStarted(_) | Event::ApprovalRequested(_)
                )),
                "{id}"
            );
        }
    }

    #[test]
    fn restored_invalid_approvals_cannot_write_spawn_or_install_grants() {
        use crate::contract::{ApprovalKind, ApprovalRequest};
        use crate::tools::{
            ApprovalDecision, ApprovalOutcome, resolve_approval, resolve_auto_approval,
        };
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        for judged in [false, true] {
            for (id, input, kind) in [
                (
                    "fs_write",
                    serde_json::json!({"path": marker, "content": "wrong", "extra": 1}),
                    ApprovalKind::Standard,
                ),
                (
                    "fs_edit",
                    serde_json::json!({"edits": [{"path": marker, "old_string": "original", "new_string": "wrong"}, {"path": marker}]}),
                    ApprovalKind::Standard,
                ),
                (
                    "bash",
                    serde_json::json!({"command": "echo must-not-run", "timeout_secs": "1"}),
                    ApprovalKind::Standard,
                ),
                (
                    "web_fetch",
                    serde_json::json!({"url": "https://example.com", "max_characters": "1"}),
                    ApprovalKind::DomainGrant {
                        domains: vec!["example.com".into()],
                    },
                ),
            ] {
                std::fs::write(&marker, "original").unwrap();
                let session = SessionId::new();
                let state = crate::tools::ToolSessionBuilder::new(dir.path().to_path_buf()).build();
                let live = LiveState::with_disabled_persistence();
                let request = ToolCallRequest {
                    call_id: ToolCallId(id.into()),
                    occurrence_id: OccurrenceId::new(),
                    tool_id: id.into(),
                    input: input.into(),
                };
                let candidate = ApprovalCandidate {
                    approval: ApprovalRequest {
                        call_id: request.call_id.clone(),
                        occurrence_id: request.occurrence_id.clone(),
                        kind,
                        reason: "restored approval".into(),
                    },
                    request: request.clone(),
                };
                let mut history = vec![Event::ToolCallRequested(request.clone())];
                if !judged {
                    history.push(Event::ApprovalRequested(candidate.approval.clone()));
                }
                live.extend_provider_events(history.into_iter().map(Into::into))
                    .unwrap();
                let (tx, rx) = crossbeam_channel::unbounded();
                register_session_runtime(session, state.clone(), live.clone(), tx);
                let frame = live.frame();
                let outcome = if judged {
                    resolve_auto_approval(&frame, session, &candidate)
                } else {
                    resolve_approval(
                        &frame,
                        session,
                        request.identity(),
                        ApprovalDecision::Approve,
                    )
                };
                let ApprovalOutcome::Applied(ToolUpdate::Finished { events, result }) = outcome
                else {
                    panic!(
                        "{id}: malformed restored input must finish without execution: {outcome:?}"
                    );
                };
                assert!(result.is_error(), "{id}");
                assert!(
                    !events
                        .iter()
                        .any(|event| matches!(event, Event::ToolCallStarted(_))),
                    "{id}"
                );
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), "original");
                assert!(!state.is_domain_allowed("example.com"));
                assert!(rx.try_recv().is_err(), "{id}");
                unregister_session_runtime(session);
            }
        }
    }

    #[test]
    fn every_automatic_dispatch_stops_before_effects_when_its_start_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("must-not-exist");
        let state = crate::tools::ToolSessionBuilder::new(dir.path().to_path_buf()).build();
        for (tool_id, input) in [
            ("workspace_snapshot", serde_json::json!({})),
            (
                "fs_write",
                serde_json::json!({"path": marker, "content":"unsafe"}),
            ),
            (
                "bash",
                serde_json::json!({"command": format!("touch {}", marker.display())}),
            ),
            (
                "web_search",
                serde_json::json!({"query":"must not be sent"}),
            ),
        ] {
            let session = SessionId::new();
            let request = ToolCallRequest {
                call_id: ToolCallId(tool_id.into()),
                occurrence_id: OccurrenceId::new(),
                tool_id: tool_id.into(),
                input: input.into(),
            };
            let history = vec![Event::ToolCallRequested(request.clone())];
            let (writer, ready) = WriterHandle::open(dir.path());
            assert!(matches!(ready.recv().unwrap(), WriterInit::Failed(_)));
            let live =
                LiveState::with_event_log_and_history(session, None, None, writer, history.clone());
            let (tx, rx) = crossbeam_channel::unbounded();
            register_session_runtime(session, state.clone(), live.clone(), tx);
            assert!(
                execute_automatic(
                    &MustNotRun,
                    &state,
                    session,
                    &live,
                    &PreparedCall::new(&request).unwrap(),
                )
                .is_err()
            );
            assert_eq!(live.events(), history);
            assert!(!marker.exists());
            assert!(rx.try_recv().is_err());
            unregister_session_runtime(session);
        }
    }
}
