use super::*;
use crate::session::test_support::{drain_events, judge_candidate, judge_test_state};
use crate::session::Connection;
use crossbeam_channel::unbounded;
use horizon_agent::config::AgentConfig;
use horizon_agent::contract::SessionId;
use horizon_agent::contract::{ApprovalKind, ApprovalRequest, ToolCallId};
use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
use horizon_agent::registry::ProviderRegistry;
use horizon_agent::tools::ApprovalCandidate;

#[test]
fn old_async_completions_cannot_answer_a_new_occurrence_with_the_same_call_id() {
    use horizon_agent::contract::OccurrenceId;
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for kind in 0..6 {
        let state = judge_test_state();
        let live = LiveState::with_disabled_persistence();
        let session = SessionId::new();
        let mut outgoing = Connection::new(state.clone()).subscribe_agent(session);
        let (commands, responses) = unbounded();
        let mut old = judge_candidate("reused-call");
        old.request.occurrence_id = Some(OccurrenceId::new());
        old.approval.occurrence_id = old.request.occurrence_id.clone();
        let mut current = old.request.clone();
        current.occurrence_id = Some(OccurrenceId::new());
        let result = ToolCallResult::new(
            old.request.call_id.clone(),
            old.request.occurrence_id.clone(),
            serde_json::json!({"cancelled": true}),
        );
        let history = vec![
            Event::ToolCallRequested(old.request.clone()),
            Event::ToolCallFinished(result.clone()),
            Event::ToolCallRequested(current),
        ];
        live.extend_provider_events(history.iter().cloned().map(Into::into));
        let call_id = old.request.call_id.clone();
        let completion = match kind {
            0 => ToolCompletion::Finished(result),
            1 => ToolCompletion::DomainDenied {
                domains: vec!["example.test".into()],
                result,
            },
            2 => ToolCompletion::FilesystemDenied {
                denials: vec![tree_denial(&dir.path().join("file"))],
                result,
            },
            3 => ToolCompletion::MachServiceDenied {
                services: vec!["com.apple.securityd".into()],
                result,
            },
            4 => ToolCompletion::DomainGrantRequired {
                call_id,
                occurrence_id: old.request.occurrence_id.clone(),
                domains: vec!["example.test".into()],
            },
            _ => ToolCompletion::ApprovalJudged(horizon_agent::tools::ApprovalJudgment {
                candidate: old,
                decision: JudgeDecision::Escalate,
            }),
        };
        fold_tool_completion(&state, &live, &commands, session, completion);
        if live.events() != history || outgoing.try_recv().is_ok() || responses.try_recv().is_ok() {
            failures.push(kind);
        }
    }
    assert!(
        failures.is_empty(),
        "stale variants changed the current attempt: {failures:?}"
    );
}

#[test]
fn reissued_approvals_keep_attempt_identity_and_ignore_missing_or_finished_requests() {
    use horizon_agent::contract::{OccurrenceId, ToolCallRequest};

    let dir = tempfile::tempdir().unwrap();
    for kind in 0..4 {
        let state = judge_test_state();
        let live = LiveState::with_disabled_persistence();
        let session = SessionId::new();
        let mut outgoing = Connection::new(state.clone()).subscribe_agent(session);
        let (commands, responses) = unbounded();
        let call_id = ToolCallId("same-call".into());
        let original = OccurrenceId::new();
        let result = ToolCallResult::new(
            call_id.clone(),
            Some(original.clone()),
            serde_json::json!({"exit_code": 0}),
        );
        let completion = match kind {
            0 => ToolCompletion::DomainDenied {
                domains: vec!["example.test".into()],
                result,
            },
            1 => ToolCompletion::FilesystemDenied {
                denials: vec![tree_denial(&dir.path().join("file"))],
                result,
            },
            2 => ToolCompletion::MachServiceDenied {
                services: vec!["com.apple.securityd".into()],
                result,
            },
            _ => ToolCompletion::DomainGrantRequired {
                occurrence_id: None,
                call_id: call_id.clone(),
                domains: vec!["example.test".into()],
            },
        };
        fold_tool_completion(&state, &live, &commands, session, completion.clone());
        assert!(
            outgoing.try_recv().is_err(),
            "missing request must not create an approval"
        );
        let request = ToolCallRequest {
            call_id: call_id.clone(),
            occurrence_id: Some(original.clone()),
            tool_id: if kind == 3 { "web_fetch" } else { "bash" }.into(),
            input: serde_json::json!({}).into(),
        };
        live.extend_provider_events(std::iter::once(Event::ToolCallRequested(request).into()));
        fold_tool_completion(&state, &live, &commands, session, completion.clone());
        let forwarded = drain_events(&mut outgoing);
        let [Event::ToolCallRequested(reissued), Event::ApprovalRequested(approval), Event::StateChanged(SessionState::WaitingForApproval)] =
            forwarded.as_slice()
        else {
            panic!("unexpected reissue ordering: {forwarded:?}");
        };
        assert!(reissued.occurrence_id.is_some());
        assert_ne!(reissued.occurrence_id, Some(original.clone()));
        assert_eq!(approval.occurrence_id, reissued.occurrence_id);
        match &approval.kind {
            ApprovalKind::DomainDenialRetry { prior_result, .. }
            | ApprovalKind::FilesystemDenialRetry { prior_result, .. }
            | ApprovalKind::MachServiceGrant { prior_result, .. } => {
                assert_eq!(prior_result.occurrence_id, Some(original));
                assert_eq!(prior_result.output["exit_code"], 0);
            }
            ApprovalKind::DomainGrant { .. } if kind == 3 => {}
            other => panic!("unexpected approval kind: {other:?}"),
        }
        live.extend_provider_events(std::iter::once(
            Event::ToolCallFinished(ToolCallResult::new(
                call_id,
                reissued.occurrence_id.clone(),
                serde_json::json!({"is_error": true}),
            ))
            .into(),
        ));
        fold_tool_completion(&state, &live, &commands, session, completion);
        assert!(
            outgoing.try_recv().is_err(),
            "finished occurrence must not reopen approval"
        );
        assert!(
            responses.try_recv().is_err(),
            "reissue must not advance the provider"
        );
    }
}

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
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
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
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
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

/// Untagged legacy completions still acquire the live request identity.
#[test]
fn fold_finished_bash_result_stamps_the_requests_occurrence_on_the_result() {
    use horizon_agent::contract::{OccurrenceId, ToolCallResult};

    let agent_config = AgentConfig::from_env_and_provider(None, None);
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
        Vec::new(),
    ));
    let live_state = LiveState::with_disabled_persistence();
    let session_id = SessionId::new();
    let connection = Connection::new(state.clone());
    let mut outgoing_rx = connection.subscribe_agent(session_id);
    let call_id = ToolCallId("bash-occ".to_string());
    let occurrence = OccurrenceId("occ-live".to_string());

    live_state.extend_provider_events(
        vec![
            Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "bash".to_string(),
                input: serde_json::json!({ "command": "echo hi" }).into(),
                occurrence_id: Some(occurrence.clone()),
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
        // Exactly what `exec::run`/`exec::run_sandboxed` build.
        ToolCompletion::Finished(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "exit_code": 0 }),
        )),
    );

    let forwarded = drain_events(&mut outgoing_rx);
    let finished = forwarded
        .iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.clone()),
            _ => None,
        })
        .expect("a finished result is forwarded");
    assert_eq!(finished.occurrence_id, Some(occurrence.clone()));
    assert!(matches!(
        commands_rx.try_recv(),
        Ok(Command::ToolCallResult(result))
            if result.occurrence_id == Some(occurrence.clone())
    ));
}

/// A result that already names its occurrence keeps it. The
/// asynchronous worker binds it before the call is queued. Prior denial
/// results are delivered synchronously by the approval resolution path.
#[test]
fn fold_finished_bash_result_keeps_an_occurrence_the_result_already_carries() {
    use horizon_agent::contract::{OccurrenceId, ToolCallResult};

    let agent_config = AgentConfig::from_env_and_provider(None, None);
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
        Vec::new(),
    ));
    let live_state = LiveState::with_disabled_persistence();
    let session_id = SessionId::new();
    let connection = Connection::new(state.clone());
    let mut outgoing_rx = connection.subscribe_agent(session_id);
    let call_id = ToolCallId("bash-occ".to_string());
    let first = OccurrenceId("occ-first".to_string());
    let reissued = OccurrenceId("occ-reissued".to_string());

    live_state.extend_provider_events(
        vec![
            Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "bash".to_string(),
                input: serde_json::json!({ "command": "echo hi" }).into(),
                occurrence_id: Some(first.clone()),
            }),
            Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "bash".to_string(),
                input: serde_json::json!({ "command": "echo hi" }).into(),
                occurrence_id: Some(reissued.clone()),
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
            Some(reissued.clone()),
            serde_json::json!({ "exit_code": 0 }),
        )),
    );

    let forwarded = drain_events(&mut outgoing_rx);
    let finished = forwarded
        .iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.clone()),
            _ => None,
        })
        .expect("a finished result is forwarded");
    assert_eq!(finished.occurrence_id, Some(reissued));
}

/// Folds one `FilesystemDenied` completion and returns the approval
/// request it produced, so the shaping tests below differ only in the
/// attempts they feed in.
fn approval_for_denials(
    denials: Vec<horizon_sandbox::FilesystemDenial>,
) -> horizon_agent::contract::ApprovalRequest {
    let agent_config = AgentConfig::from_env_and_provider(None, None);
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
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
            excluded_subpaths: Vec::new(),
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
            excluded_subpaths: Vec::new(),
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
            excluded_subpaths: Vec::new(),
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

/// An out-of-root read in a session nobody watches: the judge is still
/// asked, and only its escalation — the verdict that would have gone to
/// a human — becomes a refusal. The call resolves with an error result
/// the model receives, and no prompt reaches the client.
#[test]
fn a_judge_escalation_in_an_unattended_session_refuses_instead_of_prompting() {
    let root = std::env::temp_dir().canonicalize().unwrap();
    let state = judge_test_state();
    let live_state = LiveState::with_disabled_persistence();
    let session_id = SessionId::new();
    let connection = Connection::new(state.clone());
    let mut outgoing_rx = connection.subscribe_agent(session_id);
    let (results_tx, _results_rx) = unbounded::<ToolCompletion>();
    horizon_agent::tools::register_session_runtime(
        session_id,
        horizon_agent::tools::ToolSessionBuilder::for_root(
            root.clone(),
            horizon_agent::config::AgentToolsConfig::default(),
            horizon_agent::tools::RecallContext::default(),
        )
        .with_unattended(true)
        .build(),
        live_state.clone(),
        results_tx,
    );

    let request = horizon_agent::contract::ToolCallRequest {
        call_id: ToolCallId("unattended-escalated".to_string()),
        tool_id: "fs.read".to_string(),
        input: serde_json::json!({ "path": "/etc/hostname" }).into(),
        occurrence_id: None,
    };
    live_state.extend_provider_events(std::iter::once(
        Event::ToolCallRequested(request.clone()).into(),
    ));
    let (commands_tx, commands_rx) = unbounded::<Command>();

    fold_approval_judgment(
        &state,
        &live_state,
        &commands_tx,
        session_id,
        horizon_agent::tools::ApprovalJudgment {
            candidate: ApprovalCandidate {
                approval: ApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: "outside the workspace root".to_string(),
                    kind: ApprovalKind::Standard,
                    occurrence_id: None,
                },
                request: request.clone(),
            },
            decision: JudgeDecision::Escalate,
        },
    );

    let forwarded = commands_rx.try_recv().expect("the model gets a result");
    let Command::ToolCallResult(result) = forwarded else {
        panic!("expected a tool result, got {forwarded:?}");
    };
    assert_eq!(result.call_id, request.call_id);
    assert!(result.is_error);
    assert!(result.output["message"]
        .as_str()
        .unwrap()
        .contains(&root.display().to_string()));

    let fanned = drain_events(&mut outgoing_rx);
    assert!(
        !fanned
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))),
        "a session nobody watches must never be shown a prompt: {fanned:?}"
    );
    horizon_agent::tools::unregister_session_runtime(session_id);
}

/// The judge's other verdict is untouched by the refusal: a call it
/// allows runs, out-of-root read included.
#[test]
fn a_judge_approved_out_of_root_read_still_runs_in_an_unattended_session() {
    let root = std::env::temp_dir().canonicalize().unwrap();
    let outside = root.join(format!("horizon-unattended-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).unwrap();
    let file = outside.join("allowed.txt");
    std::fs::write(&file, "judge said yes\n").unwrap();
    // A workspace root the file is genuinely outside of.
    let workspace = outside.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let state = judge_test_state();
    let live_state = LiveState::with_disabled_persistence();
    let session_id = SessionId::new();
    let (results_tx, _results_rx) = unbounded::<ToolCompletion>();
    horizon_agent::tools::register_session_runtime(
        session_id,
        horizon_agent::tools::ToolSessionBuilder::for_root(
            workspace,
            horizon_agent::config::AgentToolsConfig::default(),
            horizon_agent::tools::RecallContext::default(),
        )
        .with_unattended(true)
        .build(),
        live_state.clone(),
        results_tx,
    );

    let request = horizon_agent::contract::ToolCallRequest {
        call_id: ToolCallId("unattended-allowed".to_string()),
        tool_id: "fs.read".to_string(),
        input: serde_json::json!({ "path": file.display().to_string() }).into(),
        occurrence_id: None,
    };
    live_state.extend_provider_events(std::iter::once(
        Event::ToolCallRequested(request.clone()).into(),
    ));
    let (commands_tx, commands_rx) = unbounded::<Command>();

    fold_approval_judgment(
        &state,
        &live_state,
        &commands_tx,
        session_id,
        horizon_agent::tools::ApprovalJudgment {
            candidate: ApprovalCandidate {
                approval: ApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: "outside the workspace root".to_string(),
                    kind: ApprovalKind::Standard,
                    occurrence_id: None,
                },
                request: request.clone(),
            },
            decision: JudgeDecision::AutoApprove,
        },
    );

    let forwarded = commands_rx.try_recv().expect("the read produces a result");
    let Command::ToolCallResult(result) = forwarded else {
        panic!("expected a tool result, got {forwarded:?}");
    };
    assert!(!result.is_error, "{:?}", result.output);
    assert!(result.output["content"]
        .as_str()
        .unwrap()
        .contains("judge said yes"));

    horizon_agent::tools::unregister_session_runtime(session_id);
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn duplicate_escalation_verdict_does_not_duplicate_the_human_prompt() {
    let agent_config = AgentConfig::from_env_and_provider(None, None);
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
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
    let state = Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
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
            occurrence_id: None,
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
    // agentd layer in this test path), so we check that the reissue
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
