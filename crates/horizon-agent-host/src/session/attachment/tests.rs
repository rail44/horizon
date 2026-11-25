use super::*;
use crate::session::events::send_session_event;
use crate::session::test_support::test_state;
use horizon_agent::contract::{Message, MessageRole, TaskProgress};

fn message(index: usize) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::Assistant,
        text: format!("message {index}"),
    })
}

fn snapshot(state: &Arc<AgentdState>, session: SessionId, live: &LiveState) -> Bootstrap {
    let (reply, mut receive) = oneshot::channel();
    capture(state, session, live, reply);
    receive.try_recv().unwrap()
}

fn publish(state: &AgentdState, session: SessionId, live: &LiveState, event: Event) {
    live.extend_provider_events([event.clone().into()]).unwrap();
    send_session_event(state, session, AgentWireEvent::Event(event));
}

#[test]
fn snapshot_and_live_updates_join_once_without_republishing_to_observers() {
    let state = test_state();
    let session = SessionId::new();
    let live = LiveState::with_disabled_persistence();
    let observer = state.subscribe_to_session(session);
    publish(&state, session, &live, message(0));
    assert_eq!(observer.events.try_recv().unwrap(), message(0));
    let mut attachment = snapshot(&state, session, &live);
    assert_eq!(attachment.history, vec![message(0)]);
    assert!(observer.events.try_recv().is_err());
    assert!(attachment.events.try_recv().is_err());
    publish(&state, session, &live, message(1));
    assert_eq!(
        attachment.events.try_recv().unwrap(),
        AgentWireEvent::Event(message(1))
    );
    assert_eq!(observer.events.try_recv().unwrap(), message(1));
    assert!(attachment.events.try_recv().is_err());
}

#[test]
fn replacement_revokes_old_commands_and_old_drop_keeps_new_subscription() {
    let state = test_state();
    let session = SessionId::new();
    let commands = state.install_test_session(session);
    let live = LiveState::with_disabled_persistence();
    let previous = snapshot(&state, session, &live);
    let mut current = snapshot(&state, session, &live);
    assert_eq!(*previous.ended.borrow(), Some(AttachmentEnd::Replaced));
    assert!(!previous.lease.command(Command::Suspend));
    drop(previous);
    assert!(current.lease.command(Command::Suspend));
    assert_eq!(commands.try_recv().unwrap(), Command::Suspend);
    assert!(commands.try_recv().is_err());
    publish(&state, session, &live, message(1));
    assert_eq!(
        current.events.try_recv().unwrap(),
        AgentWireEvent::Event(message(1))
    );
    drop(current);
    assert!(
        lock_unpoisoned(&state.agent_subscribers)
            .get(&session)
            .unwrap()
            .subscriber
            .is_none()
    );
}

#[test]
fn cancelled_bootstrap_never_replaces_a_live_attachment_and_dropped_reply_releases_lease() {
    let state = test_state();
    let session = SessionId::new();
    let live = LiveState::with_disabled_persistence();
    let current = snapshot(&state, session, &live);
    let (reply, receive) = oneshot::channel();
    drop(receive);
    capture(&state, session, &live, reply);
    assert_eq!(*current.ended.borrow(), None);
    let (reply, receive) = oneshot::channel();
    capture(&state, session, &live, reply);
    drop(receive);
    assert!(
        lock_unpoisoned(&state.agent_subscribers)
            .get(&session)
            .unwrap()
            .subscriber
            .is_none()
    );
}

/// An attachment that reads nothing for a while is waited for: every event
/// is there for it when it reads again.
#[test]
fn a_slow_attachment_keeps_every_event() {
    let state = test_state();
    let session = SessionId::new();
    let _commands = state.install_test_session(session);
    let live = LiveState::with_disabled_persistence();
    for index in 0..4000 {
        publish(&state, session, &live, message(index));
    }
    let slow = snapshot(&state, session, &live);
    assert_eq!(slow.history.len(), 4000);
    for index in 4000..8000 {
        publish(&state, session, &live, message(index));
    }
    assert_eq!(*slow.ended.borrow(), None);
    assert_eq!(slow.events.len(), 4000);
}

#[test]
fn detached_progress_is_seeded_and_finished_tasks_stay_closed() {
    let state = test_state();
    let session = SessionId::new();
    let live = LiveState::with_disabled_persistence();
    let task = TaskProgress {
        task_session_id: SessionId::new(),
        description: "research".into(),
        state: TaskProgressState::Running,
    };
    send_session_event(&state, session, AgentWireEvent::TaskProgress(task.clone()));
    let first = snapshot(&state, session, &live);
    assert!(
        first
            .metadata
            .contains(&AgentWireEvent::TaskProgress(task.clone()))
    );
    send_session_event(
        &state,
        session,
        AgentWireEvent::TaskProgress(TaskProgress {
            state: TaskProgressState::Finished,
            ..task
        }),
    );
    let next = snapshot(&state, session, &live);
    assert!(next.metadata.is_empty());
}

#[tokio::test]
async fn missing_or_ended_session_is_an_error_not_empty_history() {
    let state = test_state();
    let connection = crate::session::Connection::new(state.clone());
    let session = SessionId::new();
    assert!(
        connection
            .attach(session)
            .await
            .err()
            .unwrap()
            .contains("Unknown")
    );
    let _commands = state.install_test_session(session);
    assert!(
        connection
            .attach(session)
            .await
            .err()
            .unwrap()
            .contains("ended")
    );
}

#[tokio::test]
async fn timed_out_request_cannot_replace_the_current_attachment_later() {
    let state = test_state();
    let session = SessionId::new();
    let _commands = state.install_test_session(session);
    let (requests, receive) = crossbeam_channel::unbounded();
    lock_unpoisoned(&state.sessions)
        .get_mut(&session)
        .unwrap()
        .replay = requests;
    let live = LiveState::with_disabled_persistence();
    let current = snapshot(&state, session, &live);
    let failure = crate::session::Connection::new(state.clone())
        .attach_with_timeout(session, std::time::Duration::from_millis(5))
        .await
        .err()
        .unwrap();
    assert!(failure.contains("Timed out"));
    capture(&state, session, &live, receive.try_recv().unwrap());
    assert_eq!(*current.ended.borrow(), None);
}

#[test]
fn session_end_drains_queued_final_events_before_closing() {
    let state = test_state();
    let session = SessionId::new();
    let live = LiveState::with_disabled_persistence();
    let mut attached = snapshot(&state, session, &live);
    publish(&state, session, &live, message(1));
    lock_unpoisoned(&state.agent_subscribers).remove(&session);
    assert_eq!(*attached.ended.borrow(), None);
    assert!(attached.ended.has_changed().is_ok());
    assert_eq!(
        attached.events.try_recv().unwrap(),
        AgentWireEvent::Event(message(1))
    );
    std::assert_matches!(
        attached.events.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    );
}

#[test]
fn duplicate_creation_preserves_the_existing_session_command_owner() {
    let state = test_state();
    let session = SessionId::new();
    let commands = state.install_test_session(session);
    let connection = crate::session::Connection::new(state.clone());
    let request = horizon_agent::hosting::SessionNew {
        session_id: session,
        provider_id: horizon_agent::contract::ProviderId("builtin.agent.mock".into()),
        role_id: None,
        workspace_root: None,
        spawn_source_session_id: None,
    };
    assert!(
        connection
            .handle_session_new(request)
            .unwrap_err()
            .contains("already exists")
    );
    assert!(state.send_command(session, Command::Suspend));
    assert_eq!(commands.try_recv().unwrap(), Command::Suspend);
}

/// A `task` child's approval shown through its requester
/// (`docs/agent-subagent-design.md` decision 4) is asked again of a new
/// attachment while it is pending, and an answer reaches the child only
/// while it is: settled, the record is gone and the answer is refused.
#[test]
fn a_childs_pending_approval_is_replayed_and_answered_to_the_child_only_while_pending() {
    let state = test_state();
    let requester = SessionId::new();
    let child = SessionId::new();
    let child_commands = state.install_test_session(child);
    let live = LiveState::with_disabled_persistence();
    let request = horizon_agent::contract::ApprovalRequest {
        call_id: horizon_agent::contract::ToolCallId("call-1".into()),
        occurrence_id: horizon_agent::contract::OccurrenceId("occ-1".into()),
        reason: "read outside the workspace".into(),
        kind: horizon_agent::contract::ApprovalKind::Standard,
    };
    send_session_event(
        &state,
        requester,
        AgentWireEvent::TaskApproval {
            task_session_id: child,
            request: request.clone(),
        },
    );

    let attachment = snapshot(&state, requester, &live);
    assert!(attachment.metadata.contains(&AgentWireEvent::TaskApproval {
        task_session_id: child,
        request: request.clone(),
    }));

    let connection = super::super::connection::Connection::new(state.clone());
    let approve = Command::ApproveToolCall {
        identity: request.identity(),
    };
    assert!(
        connection
            .answer_task_approval(requester, "occ-1", approve.clone())
            .is_ok()
    );
    assert_eq!(child_commands.try_recv().unwrap(), approve);
    assert!(
        connection
            .answer_task_approval(SessionId::new(), "occ-1", approve.clone())
            .is_err(),
        "only the requester that showed it can answer it"
    );

    send_session_event(
        &state,
        requester,
        AgentWireEvent::TaskApprovalSettled {
            task_session_id: child,
            occurrence_id: request.occurrence_id.clone(),
        },
    );
    assert!(
        connection
            .answer_task_approval(requester, "occ-1", approve)
            .is_err()
    );
    assert!(child_commands.try_recv().is_err());
    let replayed = snapshot(&state, requester, &live);
    assert!(
        !replayed
            .metadata
            .iter()
            .any(|event| matches!(event, AgentWireEvent::TaskApproval { .. }))
    );
}
