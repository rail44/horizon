use super::*;
use crate::contract::ToolCallId;
fn identity(name: &str) -> ToolCallIdentity {
    crate::test_support::tool_identity(&ToolCallId(name.into()))
}
use std::sync::atomic::{AtomicUsize, Ordering};

fn call(session: SessionId, name: &str, kind: WorkKind) -> Registration {
    Registration::new(session, Lifetime::Call(identity(name)), kind)
}
#[test]
fn cancelling_a_call_does_not_cancel_session_or_pass_children() {
    let session = SessionId::new();
    let tool = call(session, "call", WorkKind::Tool);
    let judge = call(session, "call", WorkKind::Judgment);
    let child = Registration::new(session, Lifetime::Session, WorkKind::Child);
    let pass_id = Uuid::new_v4();
    let proposer = Registration::new(session, Lifetime::Pass(pass_id), WorkKind::Child);
    cancel_call(session, &identity("call"));
    assert!(tool.is_cancelled());
    assert!(judge.is_cancelled());
    assert!(!child.is_cancelled());
    assert!(!proposer.is_cancelled());
    cancel_pass(session, pass_id);
    assert!(proposer.is_cancelled());
    assert!(!child.is_cancelled());
    close_session(session);
    assert!(child.is_cancelled());
}
#[test]
fn old_worker_retirement_cannot_cancel_or_unregister_replacement() {
    let session = SessionId::new();
    let old = call(session, "same", WorkKind::Tool);
    let other = call(SessionId::new(), "same", WorkKind::Tool);
    let new = call(session, "same", WorkKind::Tool);
    assert!(!old.finish());
    drop(old);
    assert!(!session_tool_work_settled(session));
    cancel_call(session, &identity("same"));
    assert!(new.is_cancelled());
    assert!(!other.is_cancelled());
}
#[test]
fn cancellation_before_resource_acquisition_stops_the_late_resource() {
    let session = SessionId::new();
    let work = call(session, "late", WorkKind::Tool);
    cancel_call(session, &identity("late"));
    let count = Arc::new(AtomicUsize::new(0));
    let stopped = count.clone();
    work.on_stop(move || {
        stopped.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(!work.finish());
    drop(work);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[test]
fn closed_session_rejects_new_work_and_waits_for_actual_retirement() {
    let session = SessionId::new();
    let owner = SessionOwner::new(session);
    let work = call(session, "first", WorkKind::Tool);
    close_session(session);
    let late = call(session, "late", WorkKind::Tool);
    assert!(late.is_cancelled());
    assert!(!drain_session_work(session, Duration::ZERO));
    assert!(!session_tool_work_settled(session));
    drop(work);
    drop(late);
    assert!(drain_session_work(session, Duration::ZERO));
    drop(owner);
}
#[test]
fn finish_and_cancel_compete_for_one_stop_action() {
    for _ in 0..32 {
        let session = SessionId::new();
        let work = call(session, "race", WorkKind::Tool);
        let count = Arc::new(AtomicUsize::new(0));
        let stopped = count.clone();
        work.on_stop(move || {
            stopped.fetch_add(1, Ordering::SeqCst);
        });
        let barrier = Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                cancel_call(session, &identity("race"));
            });
            barrier.wait();
            let finished = work.finish();
            assert_eq!(finished, !work.is_cancelled());
        });
        drop(work);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn unwinding_releases_resources_and_wakes_a_session_drain() {
    let session = SessionId::new();
    let owner = SessionOwner::new(session);
    let stopped = Arc::new(AtomicUsize::new(0));
    let work = call(session, "panic", WorkKind::Tool);
    let count = stopped.clone();
    work.on_stop(move || {
        count.fetch_add(1, Ordering::SeqCst);
    });
    let worker = std::thread::spawn(move || {
        let _work = work;
        panic!("worker failed");
    });
    assert!(worker.join().is_err());
    assert!(drain_session_work(session, Duration::from_secs(1)));
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    drop(owner);
}
#[test]
fn a_panicking_stop_does_not_skip_other_resources_or_accounting() {
    let session = SessionId::new();
    let first = call(session, "panic", WorkKind::Tool);
    first.on_stop(|| panic!("host failed"));
    let second = call(session, "other", WorkKind::Tool);
    close_session(session);
    assert!(second.is_cancelled());
    drop(first);
    drop(second);
    assert!(drain_session_work(session, Duration::ZERO));
}
#[test]
fn environment_waits_for_tools_through_delivery_but_not_children_or_judge() {
    let session = SessionId::new();
    let _judge = call(session, "approval", WorkKind::Judgment);
    let _child = Registration::new(session, Lifetime::Session, WorkKind::Child);
    assert!(session_tool_work_settled(session));
    let tool = call(session, "tool", WorkKind::Tool);
    assert!(tool.finish());
    assert!(!session_tool_work_settled(session));
    drop(tool);
    assert!(session_tool_work_settled(session));
}

#[test]
fn replacing_runtime_tool_state_keeps_the_session_work_owner() {
    let session = SessionId::new();
    let (tx, _) = crossbeam_channel::unbounded();
    let live = crate::live::LiveState::new();
    for _ in 0..2 {
        crate::tools::register_session_runtime(
            session,
            crate::tools::ToolSessionState::new(std::env::temp_dir()),
            live.clone(),
            tx.clone(),
        );
    }
    let work = call(session, "after-activation", WorkKind::Tool);
    assert!(!work.is_cancelled());
    crate::tools::unregister_session_runtime(session);
    assert!(work.is_cancelled());
}

#[test]
fn a_late_provider_completion_does_not_cancel_a_new_occurrence() {
    let session = SessionId::new();
    let old = identity("reused");
    let new = ToolCallIdentity {
        occurrence_id: crate::contract::OccurrenceId::new(),
        ..old.clone()
    };
    let old_work = Registration::new(session, Lifetime::Call(old.clone()), WorkKind::Tool);
    let new_work = Registration::new(session, Lifetime::Call(new.clone()), WorkKind::Tool);
    assert!(old_work.is_cancelled());
    crate::tools::cancel_tool_execution(session, &old);
    drop(old_work);
    assert!(!new_work.is_cancelled());
    crate::tools::cancel_tool_execution(session, &new);
    assert!(new_work.is_cancelled());
}
