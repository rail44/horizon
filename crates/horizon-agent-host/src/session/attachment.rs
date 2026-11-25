//! Atomic history/live handoff, owned by the session loop. Replay is private
//! to its attachment; it never passes through the live observer fan-out.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use horizon_agent::contract::{
    ApprovalRequest, Command, Event, SessionId, TaskProgress, TaskProgressState,
};
use horizon_agent::hosting::{AgentWireEvent, AttachmentEnd, WorkspaceRootResolved};
use horizon_agent::live::LiveState;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use super::state::{AgentdState, lock_unpoisoned};

pub(super) type AttachRequest = oneshot::Sender<Bootstrap>;

#[derive(Default)]
pub(super) struct SessionStream {
    subscriber: Option<Subscriber>,
    /// Running children, in launch order.
    tasks: Vec<TaskProgress>,
    /// Children's approvals shown through this session and not settled, by
    /// occurrence id: asked again of a new attachment, and the record an
    /// answer is checked against before it reaches the child
    /// ([`SessionStream::task_approval`]).
    task_approvals: BTreeMap<String, (SessionId, ApprovalRequest)>,
}

struct Subscriber {
    id: Uuid,
    events: mpsc::UnboundedSender<AgentWireEvent>,
    end: watch::Sender<Option<AttachmentEnd>>,
}

pub(super) type Streams = std::sync::Mutex<HashMap<SessionId, SessionStream>>;

impl SessionStream {
    pub(super) fn publish(&mut self, event: AgentWireEvent) {
        match &event {
            AgentWireEvent::TaskProgress(progress) => {
                let known = self
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_session_id == progress.task_session_id);
                match (known, progress.state) {
                    (Some(_), TaskProgressState::Finished) => self
                        .tasks
                        .retain(|task| task.task_session_id != progress.task_session_id),
                    (Some(task), _) => *task = progress.clone(),
                    (None, TaskProgressState::Finished) => {}
                    (None, _) => self.tasks.push(progress.clone()),
                }
            }
            AgentWireEvent::TaskApproval {
                task_session_id,
                request,
            } => {
                self.task_approvals.insert(
                    request.occurrence_id.0.clone(),
                    (*task_session_id, request.clone()),
                );
            }
            AgentWireEvent::TaskApprovalSettled { occurrence_id, .. } => {
                self.task_approvals.remove(&occurrence_id.0);
            }
            _ => {}
        }
        // The one reader is the client's connection: a slow one is waited
        // for, not dropped.
        if let Some(subscriber) = &self.subscriber
            && subscriber.events.send(event).is_err()
        {
            subscriber.end.send_replace(Some(AttachmentEnd::Detached));
            self.subscriber = None;
        }
    }

    /// The child that raised the still-pending approval `occurrence`, if
    /// this session showed one.
    pub(crate) fn task_approval(&self, occurrence: &str) -> Option<SessionId> {
        self.task_approvals.get(occurrence).map(|(child, _)| *child)
    }

    fn install(
        &mut self,
    ) -> (
        Uuid,
        mpsc::UnboundedReceiver<AgentWireEvent>,
        watch::Receiver<Option<AttachmentEnd>>,
    ) {
        let (events, receiver) = mpsc::unbounded_channel();
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
    pub(crate) events: mpsc::UnboundedReceiver<AgentWireEvent>,
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
        if let Some(stream) = streams.get_mut(&self.session)
            && stream
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
            metadata.extend(
                entry
                    .reasoning
                    .clone()
                    .map(AgentWireEvent::SessionReasoning),
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
                .tasks
                .iter()
                .cloned()
                .map(AgentWireEvent::TaskProgress),
        );
        metadata.extend(stream.task_approvals.values().cloned().map(
            |(task_session_id, request)| AgentWireEvent::TaskApproval {
                task_session_id,
                request,
            },
        ));
        let (id, events, ended) = stream.install();
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
pub(super) fn subscribe(
    state: &AgentdState,
    session: SessionId,
) -> mpsc::UnboundedReceiver<AgentWireEvent> {
    lock_unpoisoned(&state.agent_subscribers)
        .entry(session)
        .or_default()
        .install()
        .1
}

#[cfg(test)]
mod tests;
