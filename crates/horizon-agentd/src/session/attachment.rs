//! Atomic history/live handoff, owned by the session loop. Replay is private
//! to its attachment; it never passes through the live observer fan-out.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use horizon_agent::contract::{
    Command, Event, SessionId, TaskProgress, TaskProgressState, ToolCallProgress,
};
use horizon_agent::live::LiveState;
use horizon_agent::wire::{AgentWireEvent, AttachmentEnd, WorkspaceRootResolved};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use super::state::{lock_unpoisoned, AgentdState};

// A slow attachment must not block the session or retain unbounded updates.
const LIVE_CAPACITY: usize = 256;

pub(super) type AttachRequest = oneshot::Sender<Bootstrap>;

#[derive(Default)]
pub(super) struct SessionStream {
    subscriber: Option<Subscriber>,
    previews: BTreeMap<String, ToolCallProgress>,
    tasks: BTreeMap<Uuid, TaskProgress>,
}

struct Subscriber {
    id: Uuid,
    events: mpsc::Sender<AgentWireEvent>,
    end: watch::Sender<Option<AttachmentEnd>>,
}

pub(super) type Streams = std::sync::Mutex<HashMap<SessionId, SessionStream>>;

impl SessionStream {
    pub(super) fn publish(&mut self, event: AgentWireEvent) {
        match &event {
            AgentWireEvent::ToolCallProgress(progress) => {
                self.previews.insert(progress.key.clone(), progress.clone());
            }
            AgentWireEvent::ToolCallProgressClosed(key) => {
                self.previews.remove(key);
            }
            AgentWireEvent::TaskProgress(progress) => {
                let key = progress.task_session_id.as_uuid();
                if progress.state == TaskProgressState::Running {
                    self.tasks.insert(key, progress.clone());
                } else {
                    self.tasks.remove(&key);
                }
            }
            _ => {}
        }
        if let Some(subscriber) = &self.subscriber {
            if let Err(error) = subscriber.events.try_send(event) {
                let reason = match error {
                    mpsc::error::TrySendError::Full(_) => AttachmentEnd::Lagged,
                    mpsc::error::TrySendError::Closed(_) => AttachmentEnd::Detached,
                };
                subscriber.end.send_replace(Some(reason));
                self.subscriber = None;
            }
        }
    }

    fn install(
        &mut self,
        capacity: usize,
    ) -> (
        Uuid,
        mpsc::Receiver<AgentWireEvent>,
        watch::Receiver<Option<AttachmentEnd>>,
    ) {
        let (events, receiver) = mpsc::channel(capacity);
        let (end, ended) = watch::channel(None);
        let id = Uuid::new_v4();
        if let Some(previous) = self.subscriber.replace(Subscriber { id, events, end }) {
            previous.end.send_replace(Some(AttachmentEnd::Replaced));
        }
        (id, receiver, ended)
    }
}

pub(crate) struct Bootstrap {
    pub(crate) history: Vec<Event>,
    pub(crate) metadata: Vec<AgentWireEvent>,
    pub(crate) events: mpsc::Receiver<AgentWireEvent>,
    pub(crate) ended: watch::Receiver<Option<AttachmentEnd>>,
    pub(crate) lease: AttachmentLease,
}

pub(crate) struct AttachmentLease {
    state: Arc<AgentdState>,
    session: SessionId,
    id: Uuid,
    // Keep the cancellation signal open while queued final events drain.
    _end: watch::Sender<Option<AttachmentEnd>>,
}

impl AttachmentLease {
    /// Holding the stream lock through enqueue makes replacement and command
    /// acceptance ordered: a replaced attachment cannot enqueue more work.
    pub(crate) fn command(&self, command: Command) -> bool {
        let streams = lock_unpoisoned(&self.state.agent_subscribers);
        if streams
            .get(&self.session)
            .and_then(|stream| stream.subscriber.as_ref())
            .is_none_or(|subscriber| subscriber.id != self.id)
        {
            return false;
        }
        self.state.send_command(self.session, command)
    }
}

impl Drop for AttachmentLease {
    fn drop(&mut self) {
        let mut streams = lock_unpoisoned(&self.state.agent_subscribers);
        if let Some(stream) = streams.get_mut(&self.session) {
            if stream
                .subscriber
                .as_ref()
                .is_some_and(|subscriber| subscriber.id == self.id)
            {
                stream
                    .subscriber
                    .take()
                    .unwrap()
                    .end
                    .send_replace(Some(AttachmentEnd::Detached));
            }
        }
    }
}

/// Only called on the session thread, between event folds. Concurrent task
/// progress uses the same stream lock, so its snapshot and updates also join
/// without a gap. A timed-out caller cannot leave a subscription installed.
pub(super) fn capture(
    state: &Arc<AgentdState>,
    session: SessionId,
    live: &LiveState,
    reply: AttachRequest,
) {
    if reply.is_closed() {
        return;
    }
    let history = live.replay_events();
    let mut metadata = {
        let sessions = lock_unpoisoned(&state.sessions);
        let mut metadata = Vec::new();
        if let Some(entry) = sessions.get(&session) {
            metadata.extend(entry.model.clone().map(AgentWireEvent::SessionModel));
            metadata.extend(
                entry
                    .selection
                    .clone()
                    .map(AgentWireEvent::SessionSelection),
            );
            metadata.extend(entry.workspace_root.clone().map(|workspace_root| {
                AgentWireEvent::WorkspaceRootResolved(WorkspaceRootResolved {
                    workspace_root,
                    parent_session_id: entry.parent_session_id,
                })
            }));
        }
        metadata
    };
    let (id, events, ended, end) = {
        let mut streams = lock_unpoisoned(&state.agent_subscribers);
        let stream = streams.entry(session).or_default();
        metadata.extend(
            stream
                .previews
                .values()
                .cloned()
                .map(AgentWireEvent::ToolCallProgress),
        );
        let mut tasks: Vec<_> = stream.tasks.values().cloned().collect();
        tasks.sort_by_key(|task| (task.started_at_epoch_ms, task.task_session_id.as_uuid()));
        metadata.extend(tasks.into_iter().map(AgentWireEvent::TaskProgress));
        let (id, events, ended) = stream.install(LIVE_CAPACITY);
        (
            id,
            events,
            ended,
            stream.subscriber.as_ref().unwrap().end.clone(),
        )
    };
    let _ = reply.send(Bootstrap {
        history,
        metadata,
        events,
        ended,
        lease: AttachmentLease {
            state: state.clone(),
            session,
            id,
            _end: end,
        },
    });
}

#[cfg(test)]
pub(super) fn subscribe(state: &AgentdState, session: SessionId) -> mpsc::Receiver<AgentWireEvent> {
    lock_unpoisoned(&state.agent_subscribers)
        .entry(session)
        .or_default()
        .install(4096)
        .1
}

#[cfg(test)]
mod tests;
