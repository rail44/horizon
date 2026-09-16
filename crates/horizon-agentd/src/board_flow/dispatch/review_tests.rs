use super::*;
use crate::board_flow::tests::record;
use horizon_agent::contract::{Event, SessionInputOutcome, SessionWorktree};
use horizon_agent::persistence::event_log::{WriterHandle, WriterInit};

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("init")
        .arg("-q")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

fn answer(reply_to: String) -> Pending {
    Pending::Answer {
        sequence: 3,
        at: 1,
        outcome: SessionInputOutcome {
            input_ids: vec!["review-request".into()],
            delivery_id: "review-result".into(),
            reply_to: Some(reply_to),
            outcome: InputResult::Success {
                text: "Detailed code findings".into(),
            },
        },
    }
}

#[tokio::test]
async fn replayed_review_result_preserves_board_destination_and_receipt() {
    let dir = repository();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let log = root.join("agent.jsonl");
    let task_session = SessionId::new();
    let reviewer = SessionId::new();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    state
        .agent_config
        .lock()
        .unwrap()
        .persistence
        .event_log_path = log.clone();
    let (writer, ready) = WriterHandle::open(&log);
    assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
    state.set_writer(Some(writer.clone()));
    let receiver = state.install_test_session(task_session);
    for session in [task_session, reviewer] {
        writer
            .append(record(
                session,
                Event::EnvironmentActivated(SessionWorktree {
                    repository: root.clone(),
                    path: root.clone(),
                    branch: "test".into(),
                    base: "base".into(),
                }),
            ))
            .unwrap();
    }
    let reply = ReplyAddress::review_result(&root, 27, task_session).unwrap();
    let pending = answer(reply);
    assert_eq!(
        recipient_key(&pending),
        Some(format!("session:{}", task_session.as_uuid()))
    );
    let Pending::Answer { outcome, .. } = pending else {
        unreachable!()
    };
    writer
        .append(record(reviewer, Event::InputOutcome(outcome)))
        .unwrap();
    writer.flush().unwrap();

    // The route is recovered from persisted records, not the requester's
    // current active turn or the most recently associated reviewer.
    let mut runner = Runner::new(state.clone(), log.clone());
    runner.tick().await.unwrap();
    let Command::SessionInput(input) = receiver.try_recv().unwrap() else {
        panic!("expected review input")
    };
    assert_eq!(input.text, "Detailed code findings");
    assert_eq!(
        input.reply_to,
        Some(ReplyAddress::board(&root, 27).unwrap())
    );
    assert!(
        !input.resume_work,
        "A review must not resume an owner-paused queue"
    );

    // A restart before acceptance retries the same identity and destination.
    let mut runner = Runner::new(state.clone(), log.clone());
    runner.tick().await.unwrap();
    assert!(
        matches!(receiver.try_recv().unwrap(), Command::SessionInput(retried) if retried == input)
    );
    writer
        .append(record(task_session, Event::InputAccepted(input)))
        .unwrap();
    writer.flush().unwrap();

    let mut runner = Runner::new(state, log);
    runner.tick().await.unwrap();
    runner.tick().await.unwrap();
    assert!(
        receiver.try_recv().is_err(),
        "Accepted review result must not be delivered again"
    );
    assert!(runner.index.pending.is_empty());
}

#[tokio::test]
async fn ordinary_session_reply_keeps_its_terminal_destination() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let target = SessionId::new();
    let receiver = state.install_test_session(target);
    let runner = Runner::new(state, dir.path().join("agent.jsonl"));
    let legacy_route = format!(r#"{{"kind":"session","id":"{}"}}"#, target.as_uuid());
    runner
        .deliver_one(SessionId::new(), "result", answer(legacy_route))
        .await
        .unwrap();
    let Command::SessionInput(input) = receiver.try_recv().unwrap() else {
        panic!("expected session input")
    };
    assert!(input.reply_to.is_none());
    assert!(!input.resume_work);
}

#[tokio::test]
async fn review_continuation_rejects_a_different_project_at_either_session() {
    let dir = repository();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let reviewer = SessionId::new();
    let target = SessionId::new();
    let receiver = state.install_test_session(target);
    let mut runner = Runner::new(state, root.join("agent.jsonl"));
    let reply = ReplyAddress::review_result(&root, 27, target).unwrap();
    for mismatch in [reviewer, target] {
        runner.index.projects.insert(reviewer, root.clone());
        runner.index.projects.insert(target, root.clone());
        runner
            .index
            .projects
            .insert(mismatch, root.join("different-project"));
        assert!(runner
            .deliver_one(reviewer, "result", answer(reply.clone()))
            .await
            .is_err());
        assert!(receiver.try_recv().is_err());
    }
}
