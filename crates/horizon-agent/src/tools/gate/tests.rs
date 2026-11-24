use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::contract::{ToolCallId, ToolCallResult, ToolOutcome};
use crate::decision::{DecisionClient, DecisionRequest, DecisionResponse};
use crate::judge::JudgeHandle;
use crate::live::LiveState;
use crate::tools::state::{
    ToolSessionBuilder, register_session_runtime, unregister_session_runtime,
};
use crate::tools::{RecallContext, ToolCompletion};

/// A judge that takes every call and answers long after the test looked.
struct Unhurried;

#[async_trait]
impl DecisionClient for Unhurried {
    async fn decide(&self, _request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        anyhow::bail!("never answers in time")
    }
}

struct Session {
    id: SessionId,
    live: LiveState,
    root: tempfile::TempDir,
    _results: crossbeam_channel::Receiver<ToolCompletion>,
}

impl Drop for Session {
    fn drop(&mut self) {
        unregister_session_runtime(self.id);
    }
}

/// A registered session that may read only its own root, with or without a
/// person to ask and a judge to offer calls to.
fn session(unattended: bool, judged: bool) -> Session {
    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let judge = judged.then(|| {
        let log = root.path().join("events.jsonl");
        let (writer, _ready) = crate::persistence::event_log::WriterHandle::open(&log);
        JudgeHandle::for_test_decision("judge", Arc::new(Unhurried), writer)
    });
    let tool_state = ToolSessionBuilder::for_root(
        canonical,
        crate::config::AgentToolsConfig::default(),
        RecallContext::default(),
    )
    .with_unattended(unattended)
    .with_judge(judge)
    .build();
    let id = SessionId::new();
    let live = LiveState::new();
    let (results, receiver) = crossbeam_channel::unbounded();
    register_session_runtime(id, tool_state, live.clone(), results);
    Session {
        id,
        live,
        root,
        _results: receiver,
    }
}

/// A read outside the session's root, requested in `live`, with the
/// approval it needs.
fn outside_read(live: &LiveState, call: &str) -> ApprovalCandidate {
    let request = ToolCallRequest {
        call_id: ToolCallId(call.into()),
        occurrence_id: OccurrenceId::new(),
        tool_id: "fs_read".into(),
        input: serde_json::json!({"path": "/etc/hostname"}).into(),
    };
    live.extend_events([Event::ToolCallRequested(request.clone())]);
    let approval = ApprovalRequest {
        call_id: request.call_id.clone(),
        occurrence_id: request.occurrence_id.clone(),
        kind: ApprovalKind::Standard,
        reason: "read outside the workspace root".into(),
    };
    ApprovalCandidate { request, approval }
}

fn asks(events: &[Event]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::ApprovalRequested(_)))
}

#[test]
fn a_call_nobody_judges_is_put_to_a_person() {
    let session = session(false, false);
    let candidate = outside_read(&session.live, "ask");

    let GateStep::Asked(events) = open(session.id, &session.live, candidate.clone()) else {
        panic!("an attended session asks");
    };
    assert_eq!(
        events,
        [
            Event::ApprovalRequested(candidate.approval.clone()),
            Event::StateChanged(SessionState::WaitingForApproval),
        ]
    );
    let frame = session.live.frame();
    assert!(
        frame
            .actionable_approval(&candidate.request.identity())
            .is_some()
    );
}

#[test]
fn a_session_nobody_can_approve_for_refuses_instead_of_asking() {
    let session = session(true, false);
    let candidate = outside_read(&session.live, "refuse");

    let GateStep::Decided(ApprovalOutcome::Applied(update)) =
        open(session.id, &session.live, candidate.clone())
    else {
        panic!("an unattended session refuses");
    };
    let (events, result) = update.into_parts();
    let result: ToolCallResult = result.expect("the call finishes");
    assert_eq!(result.call_id, candidate.request.call_id);
    assert_eq!(result.outcome, ToolOutcome::Failed);
    let root = session.root.path().canonicalize().unwrap();
    let message = result.output["message"].as_str().unwrap().to_string();
    assert!(message.contains(&root.display().to_string()), "{message}");
    assert!(!asks(&events));
    assert!(
        !asks(&session.live.events()),
        "no prompt for a session nobody watches"
    );
}

#[test]
fn a_call_the_judge_takes_waits_for_it_even_where_nobody_can_approve() {
    let session = session(true, true);
    let candidate = outside_read(&session.live, "judged");

    std::assert_matches!(
        open(session.id, &session.live, candidate),
        GateStep::Pending
    );
    assert!(!asks(&session.live.events()));
    assert!(
        !session
            .live
            .events()
            .iter()
            .any(|event| matches!(event, Event::ToolCallFinished(_))),
        "a call the judge took is not refused before its answer"
    );
}

#[test]
fn a_call_the_judge_gives_back_is_put_to_a_person() {
    let session = session(false, false);
    let candidate = outside_read(&session.live, "escalated");

    let step = judged(
        session.id,
        &session.live,
        ApprovalJudgment {
            candidate: candidate.clone(),
            decision: JudgeDecision::Escalate,
        },
    );
    std::assert_matches!(step, GateStep::Asked(_));
    assert!(
        session
            .live
            .frame()
            .actionable_approval(&candidate.request.identity())
            .is_some()
    );
}

#[test]
fn a_reissued_call_is_a_new_attempt_that_closes_a_fetch_stopped_at_a_new_domain() {
    let session = session(false, false);
    let first = outside_read(&session.live, "fetch").request;

    let reissued = reissue(
        session.id,
        &session.live,
        first.clone(),
        ApprovalKind::DomainGrant {
            domains: vec!["example.org".into()],
        },
        "fetch example.org".into(),
    )
    .expect("recorded");

    let [
        Event::ToolCallFinished(closed),
        Event::ToolCallRequested(retry),
    ] = reissued.events.as_slice()
    else {
        panic!("{:?}", reissued.events);
    };
    assert_eq!(closed.occurrence_id, first.occurrence_id);
    assert_eq!(retry.call_id, first.call_id);
    assert_ne!(retry.occurrence_id, first.occurrence_id);
    std::assert_matches!(
        &reissued.command,
        Command::ToolCallReissued(identity) if identity == &retry.identity()
    );
    std::assert_matches!(reissued.step, GateStep::Asked(_));
}
