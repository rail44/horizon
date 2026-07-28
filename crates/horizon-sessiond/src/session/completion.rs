//! Folding an asynchronous tool completion: the judge's verdict, a
//! finished bash/web call, and the denial outcomes that reissue an approval
//! instead of returning a result to the provider.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{
    ApprovalKind, ApprovalRequest, Command, Event, SessionId, SessionState, ToolCallId,
    ToolCallResult,
};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    resolve_auto_approval, should_fold_completion, JudgeDecision, ToolCompletion,
};
use horizon_agent::wire::AgentWireEvent;

use super::approval::{begin_reissued_approval, emit_human_approval, forward_approval_outcome};
use super::events::send_session_event;
use super::state::SessiondState;

/// The async-execution analogue of `run::handle_provider_event`'s fold, for a
/// bash or host-side web call whose result has now arrived on its own
/// channel -- the same shape the deleted
/// in-process agent runtime's `fold_bash_completion` used to have,
/// forwarding the same events over the wire instead of updating a local
/// `Frames` signal, except the trailing `StateChanged` is no longer
/// unconditional (see below).
///
/// Bash and web tools complete asynchronously here; fs/config tools resolve synchronously
/// inside `agent::tools::approval::resolve_synchronous_tool` (folded
/// straight into `dispatch_inbound_command`'s `resolve_and_forward`) -- so
/// this is the one place a completion can land after *other* tool-call
/// approvals from the same turn are still outstanding.
pub(super) fn fold_tool_completion(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: ToolCompletion,
) {
    match completion {
        ToolCompletion::ApprovalJudged(judgment) => {
            fold_approval_judgment(state, live_state, commands_tx, session_id, judgment)
        }
        ToolCompletion::Finished(result) => {
            fold_finished_bash_result(state, live_state, commands_tx, session_id, result)
        }
        ToolCompletion::DomainDenied {
            call_id,
            domains,
            result,
        } => fold_domain_denied(state, live_state, session_id, call_id, domains, result),
        ToolCompletion::DomainGrantRequired { call_id, domains } => {
            fold_domain_grant_required(state, live_state, session_id, call_id, domains)
        }
        ToolCompletion::FilesystemDenied {
            call_id,
            denials,
            result,
        } => fold_filesystem_denied(state, live_state, session_id, call_id, denials, result),
    }
}

fn fold_approval_judgment(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    judgment: horizon_agent::tools::ApprovalJudgment,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &judgment.candidate.request.call_id)
        || frame.has_tool_call_started(&judgment.candidate.request.call_id)
    {
        return;
    }
    if frame
        .actionable_pending_approval_call_ids()
        .contains(&judgment.candidate.request.call_id)
    {
        // A duplicate/stale verdict must not duplicate a prompt or overturn
        // a verdict that has already escalated to the human.
        return;
    }
    match judgment.decision {
        JudgeDecision::AutoApprove => {
            let logged_call_id = judgment.candidate.request.call_id.clone();
            let outcome = resolve_auto_approval(&frame, session_id, &judgment.candidate);
            forward_approval_outcome(state, commands_tx, session_id, logged_call_id, outcome);
        }
        JudgeDecision::Escalate => {
            emit_human_approval(state, live_state, session_id, judgment.candidate.approval);
        }
    }
}

#[cfg(test)]
fn fold_bash_completion(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: ToolCompletion,
) {
    fold_tool_completion(state, live_state, commands_tx, session_id, completion);
}

/// The ordinary case: a bash call actually finished (successfully or not).
/// Unchanged behavior from before [`BashCompletion`] grew a second variant.
fn fold_finished_bash_result(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    result: ToolCallResult,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &result.call_id) {
        return;
    }

    // Honest trailing state: a second approval-gated call from the same
    // turn (another `bash` approved earlier, or a sibling fs/config
    // request still awaiting a decision) can still be outstanding when
    // this one finishes -- reporting `WaitingForUser` then is exactly the
    // backlog #34 bug (status line blanks, stop button vanishes, while a
    // decision is still actionable). `actionable_pending_approval_call_ids`
    // (not the plain `pending_approval_call_ids`) is the right reader here
    // for the same reason it's the required one on every dispatch path
    // (see its doc comment): it excludes a *ghost* request whose own turn
    // already ended, which no live daemon-side gate can ever answer, so a
    // ghost alone must never hold the reported state at `WaitingForApproval`
    // forever. `result.call_id` itself is still in that list at this point
    // -- only a *folded* `ToolCallFinished` clears an id, and this call's
    // hasn't been folded yet -- so it's excluded explicitly rather than
    // re-reading the frame after folding.
    //
    // If nothing else is actionable, the turn is still running: the result
    // is about to be handed back to the provider via `commands_tx.send`,
    // which will run another completion. Reporting `WaitingForUser` here
    // would tell observers the turn finished and would make the persistence
    // turn tracker close the turn prematurely. `Running` keeps the stop
    // button enabled and the composer in the "running" placeholder until
    // the provider itself emits `TurnEnded` and the real `WaitingForUser`
    // at the turn boundary.
    let approval_still_pending = frame
        .actionable_pending_approval_call_ids()
        .into_iter()
        .any(|id| id != result.call_id);
    let trailing_state = if approval_still_pending {
        SessionState::WaitingForApproval
    } else {
        SessionState::Running
    };

    let events = vec![
        Event::ToolCallFinished(result.clone()),
        Event::StateChanged(trailing_state),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }

    let _ = commands_tx.send(Command::ToolCallResult(result));
}

/// A tier-1 sandboxed `bash` call's network egress was refused for one or
/// more `domains` (`docs/agent-approval-design.md` leg 4b) -- surface a
/// fresh, differently-named approval offer ("allow domain X for this
/// session and retry") instead of handing `result` straight to the
/// provider. It folds a fresh `ToolCallRequested` right before the
/// `ApprovalRequested`, so the eventual Approve/Deny is not misclassified
/// as `AlreadyResolved`. `result` is the genuine completed outcome, carried
/// on the pending request's own [`ApprovalKind::DomainDenialRetry`] so a
/// later deny can forward it as-is (`tools::approval::
/// resolve_domain_denial_retry`).
fn fold_domain_denied(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    call_id: ToolCallId,
    domains: Vec<String>,
    result: ToolCallResult,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &call_id) {
        return;
    }
    let Some(original_request) = frame.tool_call_request(&call_id).cloned() else {
        // Should be unreachable (this call_id was necessarily requested to
        // have gotten this far) -- nothing sane to reissue against.
        return;
    };

    let domain_list = domains.join(", ");
    let reason = format!(
        "`{}` tried to reach {domain_list}, but it isn't allowed \
         for this session yet. Allow {} for this session and retry?",
        original_request.tool_id,
        if domains.len() == 1 { "it" } else { "them" }
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request.clone(),
        ApprovalRequest {
            call_id,
            // `begin_reissued_approval` mints a fresh `OccurrenceId` for
            // the reissued request and stamps it on both the new
            // `ToolCallRequest` and the `ApprovalRequest` (see
            // `session/approval.rs`). The `prior_result` here, by
            // contrast, is the *first* attempt's outcome -- the bash
            // executor constructed it without an in-scope request, so
            // its `occurrence_id` is `None`. We stamp the original
            // request's `occurrence_id` onto it now so the transcript
            // and analytics attribute this result to the same
            // occurrence the originating `ToolCallRequested` carries,
            // not to whichever request happens to share its `call_id`
            // at fold time.
            occurrence_id: None,
            reason,
            kind: ApprovalKind::DomainDenialRetry {
                domains,
                prior_result: ToolCallResult {
                    occurrence_id: original_request.occurrence_id.clone(),
                    ..result
                },
            },
        },
    );
}

fn fold_domain_grant_required(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    call_id: ToolCallId,
    domains: Vec<String>,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &call_id) {
        return;
    }
    let Some(original_request) = frame.tool_call_request(&call_id).cloned() else {
        return;
    };
    let domain_list = domains.join(", ");
    let reason = format!(
        "`{}` needs to contact {domain_list}, but no request was sent to that domain. Allow {} \
         for this session and retry from the original URL?",
        original_request.tool_id,
        if domains.len() == 1 { "it" } else { "them" }
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request,
        ApprovalRequest {
            call_id,
            // See the matching site in `fold_domain_denied` above --
            // `begin_reissued_approval` overwrites this with the fresh
            // `OccurrenceId` it mints for the reissued request, so we
            // leave it as `None` here.
            occurrence_id: None,
            reason,
            kind: ApprovalKind::DomainGrant { domains },
        },
    );
}

fn fold_filesystem_denied(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    call_id: ToolCallId,
    denials: Vec<horizon_sandbox::FilesystemDenial>,
    result: ToolCallResult,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &call_id) {
        return;
    }
    let Some(original_request) = frame.tool_call_request(&call_id).cloned() else {
        return;
    };
    let attempted = denials
        .iter()
        .map(|denial| denial.attempted_path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    // The shaping is generic over path structure only -- see
    // `horizon_sandbox::suggest_grants`. Sessiond supplies the two facts it
    // owns (this session's workspace root, this account's `$HOME`) and
    // nothing about what command was run.
    let grants = horizon_sandbox::suggest_grants(
        &denials,
        session_workspace_root(state, session_id).as_deref(),
        horizon_sandbox::home_dir().as_deref(),
    );
    let offered = grants
        .iter()
        .map(|grant| {
            format!(
                "{:?} {:?} access to {}",
                grant.access,
                grant.scope,
                grant.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    // The prompt states what approval actually buys. It used to offer whole
    // -call host authority; it now offers exactly these grants, and the
    // command still runs sandboxed with them (`docs/containment-denial-
    // narrow-grants-design.md`'s 2026-07-26 decision).
    let reason = format!(
        "`bash` was refused access outside its workspace: attempted {attempted}. \
         Grant {offered} to this session and retry the same call, still sandboxed? \
         The grant lasts for this session only."
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request.clone(),
        ApprovalRequest {
            call_id,
            // See the matching site in `fold_domain_denied` --
            // `begin_reissued_approval` mints a fresh `OccurrenceId` for
            // the reissued request and stamps it on both the new
            // `ToolCallRequest` and the `ApprovalRequest`, so we leave
            // this as `None` here.
            occurrence_id: None,
            reason,
            kind: ApprovalKind::FilesystemDenialRetry {
                denials,
                grants,
                // Same prior_result fixup as `fold_domain_denied` --
                // bash constructed the result without an in-scope
                // request, so stamp the original request's
                // `occurrence_id` onto it now so the transcript and
                // analytics attribute it to the right occurrence.
                prior_result: ToolCallResult {
                    occurrence_id: original_request.occurrence_id.clone(),
                    ..result
                },
            },
        },
    );
}

/// This session's confinement root, as recorded when it started -- the
/// workspace half of the suggestion shaping's input. `None` for a session
/// with no root (or one sessiond no longer tracks), in which case every
/// attempt is treated as outside, which is the conservative reading.
fn session_workspace_root(state: &Arc<SessiondState>, session_id: SessionId) -> Option<PathBuf> {
    let root = state
        .sessions
        .lock()
        .unwrap()
        .get(&session_id)
        .and_then(|entry| entry.workspace_root.clone())?;
    // Canonical on both sides or the containment test is meaningless: the
    // supervisor reports resolved paths, and this entry holds whatever the
    // spawn was given.
    std::fs::canonicalize(root).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::{drain_events, judge_candidate, judge_test_state};
    use crate::session::Connection;
    use crossbeam_channel::unbounded;
    use horizon_agent::config::AgentConfig;
    use horizon_agent::contract::{ProviderRegistry, SessionId};
    use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
    use horizon_agent::tools::ApprovalCandidate;

    /// Regression test for backlog #34: `SessionState::WaitingForUser`
    /// reported while a tool-call approval is still pending. Two `bash`
    /// calls are approval-gated in the same turn; only the first has been
    /// approved (its `ToolRunning`/`ToolCallStarted` pair already folded,
    /// mirroring `agent::tools::approval::resolve_bash`'s `Started` outcome)
    /// when its async completion reaches `fold_bash_completion`. The second
    /// call's `ApprovalRequested` is still unresolved at that point, so the
    /// trailing state this emits must be `WaitingForApproval`, not
    /// `WaitingForUser` -- exactly the dishonest-state bug the backlog item
    /// describes (status line blanks, stop button vanishes, while a
    /// decision is still actionable). Once the second call is also approved
    /// and finishes, the state must stay `Running`: the result is handed
    /// back to the provider and the turn continues, so `WaitingForUser` is
    /// reserved for the real turn boundary (`TurnEnded` + provider state).
    #[test]
    fn fold_bash_completion_reports_running_once_no_approval_remains_pending() {
        use horizon_agent::contract::{ApprovalRequest, ToolCallResult};

        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let call_a = ToolCallId("bash-a".to_string());
        let call_b = ToolCallId("bash-b".to_string());

        live_state.extend_provider_events(
            vec![
                Event::StateChanged(SessionState::WaitingForApproval),
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: call_a.clone(),
                    reason: "bash".to_string(),
                    kind: ApprovalKind::Standard,

                    occurrence_id: None,
                }),
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: call_b.clone(),
                    reason: "bash".to_string(),
                    kind: ApprovalKind::Standard,

                    occurrence_id: None,
                }),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_a.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            ToolCompletion::Finished(ToolCallResult::new(
                call_a.clone(),
                None,
                serde_json::json!({ "exit_code": 0 }),
            )),
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForApproval)),
            "call_b's approval is still outstanding, so the reported state must \
             stay WaitingForApproval, got: {forwarded:?}"
        );
        assert!(matches!(
            commands_rx.try_recv(),
            Ok(Command::ToolCallResult(result)) if result.call_id == call_a
        ));

        // Approving `call_b` folds its own running pair the same way
        // `call_a`'s did, then its completion arrives too.
        live_state.extend_provider_events(
            vec![
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_b.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            ToolCompletion::Finished(ToolCallResult::new(
                call_b.clone(),
                None,
                serde_json::json!({ "exit_code": 0 }),
            )),
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::Running)),
            "every approval is resolved but the turn is still running, so the \
             reported state must stay Running, got: {forwarded:?}"
        );
    }

    /// A finished async tool call with no remaining approvals must not flip
    /// the reported state to `WaitingForUser`: the result is being forwarded
    /// to the provider and the turn continues. This is the state-level half of
    /// the bug where mid-turn `WaitingForUser` emissions made the persistence
    /// turn tracker close a single unfinished turn repeatedly.
    #[test]
    fn fold_bash_completion_reports_running_when_no_approval_is_pending() {
        use horizon_agent::contract::ToolCallResult;

        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let call_id = ToolCallId("bash-1".to_string());

        live_state.extend_provider_events(
            vec![
                Event::StateChanged(SessionState::Running),
                Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "bash".to_string(),
                    input: serde_json::json!({ "command": "echo hi" }).into(),
                    occurrence_id: None,
                }),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        let (commands_tx, _commands_rx) = unbounded::<Command>();
        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            ToolCompletion::Finished(ToolCallResult::new(
                call_id.clone(),
                None,
                serde_json::json!({ "exit_code": 0 }),
            )),
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::Running)),
            "no approval is pending, so the finished tool must leave the turn \
             running, got: {forwarded:?}"
        );
    }

    /// Folds one `FilesystemDenied` completion and returns the approval
    /// request it produced, so the shaping tests below differ only in the
    /// attempts they feed in.
    fn approval_for_denials(
        denials: Vec<horizon_sandbox::FilesystemDenial>,
    ) -> horizon_agent::contract::ApprovalRequest {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let call_id = ToolCallId("bash-filesystem-denied".to_string());
        live_state.extend_provider_events(
            vec![
                Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "bash".to_string(),
                    input: serde_json::json!({ "command": "echo hi" }).into(),
                    occurrence_id: None,
                }),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );
        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            ToolCompletion::FilesystemDenied {
                call_id: call_id.clone(),
                denials,
                result: ToolCallResult::new(
                    call_id.clone(),
                    None,
                    serde_json::json!({ "exit_code": 0 }),
                ),
            },
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForApproval))
        );
        assert!(commands_rx.try_recv().is_err());
        forwarded
            .iter()
            .find_map(|event| match event {
                Event::ApprovalRequested(request) => Some(request.clone()),
                _ => None,
            })
            .expect("filesystem approval request")
    }

    fn tree_denial(attempted: &std::path::Path) -> horizon_sandbox::FilesystemDenial {
        horizon_sandbox::FilesystemDenial {
            attempted_path: attempted.to_path_buf(),
            grant: horizon_sandbox::FilesystemGrant {
                path: attempted
                    .parent()
                    .expect("attempt has a parent")
                    .canonicalize()
                    .expect("parent exists"),
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            },
        }
    }

    /// Issue 009's shape end to end: several refused attempts under one
    /// cache directory become a single tree grant at that directory, and
    /// the prompt offers exactly that -- no longer whole-call host
    /// authority.
    #[test]
    fn fold_filesystem_denial_offers_one_tree_at_the_attempts_common_ancestor() {
        let cache = tempfile::tempdir().expect("create temp cache");
        let canonical = std::fs::canonicalize(cache.path()).unwrap();
        let nested = canonical.join("build").join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        let first = canonical.join(".package-cache-mutate");
        let second = nested.join(".build-lock");

        let approval = approval_for_denials(vec![tree_denial(&first), tree_denial(&second)]);

        assert!(approval.reason.contains(&first.display().to_string()));
        assert!(approval.reason.contains(&second.display().to_string()));
        assert!(
            approval.reason.contains("still sandboxed"),
            "the prompt must say the retry stays contained: {}",
            approval.reason
        );
        assert!(
            !approval.reason.contains("host process"),
            "the prompt must no longer offer host authority: {}",
            approval.reason
        );
        let ApprovalKind::FilesystemDenialRetry {
            denials, grants, ..
        } = &approval.kind
        else {
            panic!("expected a filesystem-denial retry: {:?}", approval.kind);
        };
        assert_eq!(denials.len(), 2, "the attempts stay recorded as evidence");
        assert_eq!(
            grants,
            &vec![horizon_sandbox::FilesystemGrant {
                path: canonical,
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            }]
        );
    }

    /// The clamp: attempts whose only shared ancestor is a system root
    /// produce per-attempt grants rather than an offer no one should
    /// accept.
    #[test]
    fn fold_filesystem_denial_falls_back_when_no_honest_ancestor_exists() {
        let attempted = std::path::PathBuf::from("/outside/new.txt");
        let denial = horizon_sandbox::FilesystemDenial {
            attempted_path: attempted.clone(),
            grant: horizon_sandbox::FilesystemGrant {
                path: std::path::PathBuf::from("/outside"),
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            },
        };

        let approval = approval_for_denials(vec![denial.clone()]);

        let ApprovalKind::FilesystemDenialRetry { grants, .. } = &approval.kind else {
            panic!("expected a filesystem-denial retry: {:?}", approval.kind);
        };
        assert_eq!(
            grants,
            &vec![denial.grant],
            "an unresolvable ancestor keeps the per-attempt grant instead of widening"
        );
    }

    #[test]
    fn auto_approval_verdict_forwards_existing_approved_path_without_prompt() {
        let state = judge_test_state();
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let candidate = judge_candidate("judge-auto");
        live_state.extend_provider_events(std::iter::once(
            Event::ToolCallRequested(candidate.request.clone()).into(),
        ));
        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_approval_judgment(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            horizon_agent::tools::ApprovalJudgment {
                candidate: candidate.clone(),
                decision: JudgeDecision::AutoApprove,
            },
        );

        assert!(matches!(
            commands_rx.try_recv(),
            Ok(Command::ApproveToolCall { call_id }) if call_id == candidate.request.call_id
        ));
        assert!(drain_events(&mut outgoing_rx).is_empty());
        assert!(live_state
            .frame()
            .actionable_pending_approval_call_ids()
            .is_empty());
    }

    #[test]
    fn late_or_started_verdict_is_ignored() {
        for terminal_event in [
            Event::ToolCallStarted(ToolCallId("judge-stale".to_string())),
            Event::ToolCallFinished(ToolCallResult::new(
                ToolCallId("judge-stale".to_string()),
                None,
                serde_json::json!({ "cancelled": true }),
            )),
        ] {
            let state = judge_test_state();
            let live_state = LiveState::with_disabled_persistence();
            let session_id = SessionId::new();
            let connection = Connection::new(state.clone());
            let mut outgoing_rx = connection.subscribe_agent(session_id);
            let candidate = judge_candidate("judge-stale");
            live_state.extend_provider_events(
                [
                    Event::ToolCallRequested(candidate.request.clone()),
                    terminal_event,
                ]
                .into_iter()
                .map(Into::into),
            );
            let (commands_tx, commands_rx) = unbounded::<Command>();

            fold_approval_judgment(
                &state,
                &live_state,
                &commands_tx,
                session_id,
                horizon_agent::tools::ApprovalJudgment {
                    candidate,
                    decision: JudgeDecision::Escalate,
                },
            );

            assert!(commands_rx.try_recv().is_err());
            assert!(drain_events(&mut outgoing_rx).is_empty());
        }
    }

    #[test]
    fn duplicate_escalation_verdict_does_not_duplicate_the_human_prompt() {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let request = horizon_agent::contract::ToolCallRequest {
            call_id: ToolCallId("duplicate-judge".to_string()),
            tool_id: "mock.approval_required".to_string(),
            input: serde_json::json!({}).into(),

            occurrence_id: None,
        };
        live_state.extend_provider_events(std::iter::once(
            Event::ToolCallRequested(request.clone()).into(),
        ));
        let judgment = horizon_agent::tools::ApprovalJudgment {
            candidate: ApprovalCandidate {
                approval: ApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: "ask once".to_string(),
                    kind: ApprovalKind::Standard,

                    occurrence_id: None,
                },
                request,
            },
            decision: JudgeDecision::Escalate,
        };
        let (commands_tx, _commands_rx) = unbounded::<Command>();

        fold_approval_judgment(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            judgment.clone(),
        );
        assert_eq!(drain_events(&mut outgoing_rx).len(), 2);

        fold_approval_judgment(&state, &live_state, &commands_tx, session_id, judgment);
        assert!(drain_events(&mut outgoing_rx).is_empty());
    }

    #[test]
    fn fold_domain_grant_required_reissues_the_fetch_without_contacting_the_provider() {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let connection = Connection::new(state.clone());
        let mut outgoing_rx = connection.subscribe_agent(session_id);
        let call_id = ToolCallId("web-fetch-redirect-domain".to_string());
        let original_request = horizon_agent::contract::ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "web_fetch".to_string(),
            input: serde_json::json!({ "url": "https://example.com/start" }).into(),

            occurrence_id: None,
        };
        live_state.extend_provider_events(
            vec![
                Event::ToolCallRequested(original_request.clone()),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );
        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_tool_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            ToolCompletion::DomainGrantRequired {
                call_id: call_id.clone(),
                domains: vec!["redirect.example".to_string()],
            },
        );

        let forwarded = drain_events(&mut outgoing_rx);
        // The reissued `ToolCallRequested` keeps the original `call_id`,
        // `tool_id`, and `input` but picks up a fresh `OccurrenceId` --
        // see `begin_reissued_approval`'s doc comment (and
        // `backlog 42 / 55`). The test only seeded the original request
        // with `occurrence_id: None` (no provider-side identity yet at the
        // sessiond layer in this test path), so we check that the reissue
        // stamp is *some* `Some(_)` and that it doesn't match the
        // original's `None` -- the important invariant is "the reissue is
        // a distinct occurrence, not a verbatim forward".
        let reissued_occurrence_id = forwarded
            .iter()
            .find_map(|event| match event {
                Event::ToolCallRequested(request) if request.call_id == call_id => {
                    Some(request.occurrence_id.clone())
                }
                _ => None,
            })
            .expect("reissued ToolCallRequested event");
        assert!(
            reissued_occurrence_id.is_some(),
            "begin_reissued_approval must mint a fresh OccurrenceId"
        );
        assert_ne!(
            reissued_occurrence_id, original_request.occurrence_id,
            "reissued occurrence_id must differ from the original's None"
        );
        assert!(forwarded.iter().any(|event| {
            matches!(
                event,
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: approval_call_id,
                    kind: ApprovalKind::DomainGrant { domains },
                    occurrence_id: Some(ref occ),
                    ..
                }) if approval_call_id == &call_id
                    && domains == &["redirect.example".to_string()]
                    // The approval's `occurrence_id` must match the
                    // reissued request's -- see
                    // `begin_reissued_approval` in
                    // `session/approval.rs`.
                    && Some(occ) == reissued_occurrence_id.as_ref()
            )
        }));
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForApproval))
        );
        assert!(commands_rx.try_recv().is_err());
    }
}
