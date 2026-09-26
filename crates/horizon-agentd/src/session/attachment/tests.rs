use super::*;
use crate::session::events::send_session_event;
use crate::session::test_support::test_state;
use horizon_agent::contract::{Message, MessageRole, TaskProgress, ToolCallProgress};

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
    assert!(!previous.lease.command(Command::ContinueTurn));
    drop(previous);
    assert!(current.lease.command(Command::ContinueTurn));
    assert_eq!(commands.try_recv().unwrap(), Command::ContinueTurn);
    assert!(commands.try_recv().is_err());
    publish(&state, session, &live, message(1));
    assert_eq!(
        current.events.try_recv().unwrap(),
        AgentWireEvent::Event(message(1))
    );
    drop(current);
    assert!(lock_unpoisoned(&state.agent_subscribers)
        .get(&session)
        .unwrap()
        .subscriber
        .is_none());
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
    assert!(lock_unpoisoned(&state.agent_subscribers)
        .get(&session)
        .unwrap()
        .subscriber
        .is_none());
}

#[test]
fn slow_attachment_is_revoked_and_reopening_recovers_every_committed_event() {
    let state = test_state();
    let session = SessionId::new();
    let _commands = state.install_test_session(session);
    let live = LiveState::with_disabled_persistence();
    for index in 0..4000 {
        publish(&state, session, &live, message(index));
    }
    let slow = snapshot(&state, session, &live);
    assert_eq!(slow.history.len(), 4000);
    for index in 4000..4000 + LIVE_CAPACITY + 1 {
        publish(&state, session, &live, message(index));
    }
    assert_eq!(*slow.ended.borrow(), Some(AttachmentEnd::Lagged));
    assert!(!slow.lease.command(Command::ContinueTurn));
    assert_eq!(slow.events.len(), LIVE_CAPACITY);
    let recovered = snapshot(&state, session, &live);
    assert_eq!(
        recovered.history,
        (0..4000 + LIVE_CAPACITY + 1)
            .map(message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn detached_progress_is_seeded_and_closed_previews_and_tasks_stay_closed() {
    let state = test_state();
    let session = SessionId::new();
    let live = LiveState::with_disabled_persistence();
    let preview = AgentWireEvent::ToolCallProgress(ToolCallProgress {
        key: "preview".into(),
        tool_id: None,
        bytes: 500,
    });
    let task = TaskProgress {
        task_session_id: SessionId::new(),
        description: "research".into(),
        state: TaskProgressState::Running,
        activity: Some("fs.read".into()),
        started_at_epoch_ms: 0,
    };
    send_session_event(&state, session, preview.clone());
    send_session_event(&state, session, AgentWireEvent::TaskProgress(task.clone()));
    let first = snapshot(&state, session, &live);
    assert!(first.metadata.contains(&preview));
    assert!(first
        .metadata
        .contains(&AgentWireEvent::TaskProgress(task.clone())));
    send_session_event(
        &state,
        session,
        AgentWireEvent::ToolCallProgressClosed("preview".into()),
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
    assert!(connection
        .attach(session)
        .await
        .err()
        .unwrap()
        .contains("Unknown"));
    let _commands = state.install_test_session(session);
    assert!(connection
        .attach(session)
        .await
        .err()
        .unwrap()
        .contains("ended"));
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
    assert!(matches!(
        attached.events.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn duplicate_creation_preserves_the_existing_session_command_owner() {
    let state = test_state();
    let session = SessionId::new();
    let commands = state.install_test_session(session);
    let connection = crate::session::Connection::new(state.clone());
    let request = horizon_agent::wire::SessionNew {
        session_id: session,
        provider_id: horizon_agent::contract::ProviderId("builtin.agent.mock".into()),
        role_id: None,
        workspace_root: None,
        spawn_source_session_id: None,
        isolate: false,
    };
    assert!(connection
        .handle_session_new(request)
        .unwrap_err()
        .contains("already exists"));
    assert!(state.send_command(session, Command::ContinueTurn));
    assert_eq!(commands.try_recv().unwrap(), Command::ContinueTurn);
}
