//! One agent session as a pane sees it: commands in, the session's model
//! folded from what the daemon sends, and someone to wake when it has
//! changed. A session outlives a pane of it: the model is kept for the
//! next pane to draw.

use crate::board::activity::BoardSessionActivity;
use horizon_shell_runtime::agent_model::SessionStatus;
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::Sender;
use horizon_shell_runtime::agent_model::{
    AgentModel, ToolCallIdentity, actionable_pending_approval_identities_in,
    state_indicates_turn_in_flight,
};
use horizon_shell_runtime::agent_view::adapter::{Adapted, SettledUnits, adapt, unix_epoch_ms};
use horizon_shell_runtime::agent_view::title::derive_title_from_items;
use horizon_shell_runtime::{AgentCommand, AgentSessionHandle, AgentUpdate, AttachmentState};
use horizon_workspace::title::derive_session_title;

/// The model as folded so far, and how many updates went into it.
#[derive(Default)]
struct Folded {
    model: AgentModel,
    attachment: AttachmentState,
    revision: u64,
}

type Wake = Arc<Mutex<Option<Waker>>>;

/// The session's title from what it is about: the agent's, else its first
/// message as it was sent.
#[derive(Default)]
struct Title {
    derived: Option<String>,
    /// Whether `derived` has changed since it was last taken.
    fresh: bool,
}

fn wake(waker: &Wake) {
    if let Some(waker) = waker.lock().unwrap().as_ref() {
        waker.wake_by_ref();
    }
}

/// Gives the session its title: the agent's, once it has given one (it is
/// untrusted text, shaped as any title is), else its first message.
fn title_from(folded: &Folded, title: &Arc<Mutex<Title>>) {
    let given = (folded.model.title.as_deref()).and_then(derive_session_title);
    let derived = given.or_else(|| derive_title_from_items(&folded.model.frame.items));
    let mut held = title.lock().unwrap();
    if derived.is_some() && held.derived != derived {
        held.derived = derived;
        held.fresh = true;
    }
}

pub struct AgentSession {
    commands: Sender<AgentCommand>,
    folded: Arc<Mutex<Folded>>,
    title: Arc<Mutex<Title>>,
    waker: Wake,
    /// The attachment to the agent runtime: letting go of it detaches the
    /// session, which goes on.
    attachment: AgentSessionHandle,
}

/// What the commands' availability is decided from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    pub pending_approval: bool,
    pub turn_in_flight: bool,
}

impl AgentSession {
    /// A session of `horizon-agent-acp`'s, through the runtime client.
    pub fn attached(mut handle: AgentSessionHandle) -> Self {
        let (folded, waker) = (Arc::<Mutex<Folded>>::default(), Wake::default());
        let (folding, woken) = (Arc::clone(&folded), Arc::clone(&waker));
        let title = Arc::<Mutex<Title>>::default();
        let titled = Arc::clone(&title);
        let mut updates = handle.take_events();
        std::thread::spawn(move || {
            let changed = |change: &mut dyn FnMut(&mut Folded)| {
                let mut folded = folding.lock().unwrap();
                change(&mut folded);
                folded.revision += 1;
                title_from(&folded, &titled);
                drop(folded);
                wake(&woken);
            };
            while let Some(update) = updates.blocking_recv() {
                let mut update = Some(update);
                changed(&mut |folded| match update.take() {
                    Some(AgentUpdate::State(state)) => {
                        // Restoring opens the attachment's bootstrap: the
                        // fold starts over.
                        if state == AttachmentState::Restoring {
                            folded.model.restart();
                        }
                        folded.attachment = state;
                    }
                    Some(AgentUpdate::Event(event)) => folded.model.apply(*event),
                    None => {}
                });
            }
            changed(&mut |folded| folded.attachment.stream_ended());
        });
        Self {
            commands: handle.sender(),
            folded,
            title,
            waker,
            attachment: handle,
        }
    }

    /// The session's id: the agent's name for it, once it has named it.
    pub fn session_id(&self) -> horizon_workspace::SessionId {
        self.attachment.session_id()
    }

    pub fn send(&self, command: AgentCommand) {
        let _ = self.commands.send(command);
    }

    /// What a pane draws of the session and the number of the update it is
    /// as of, if that is not `seen`. `settled` is the pane's own: the
    /// units of the turns that have ended, kept between calls.
    pub fn adapted_after(&self, seen: u64, settled: &mut SettledUnits) -> Option<(u64, Adapted)> {
        let folded = self.folded.lock().unwrap();
        if folded.revision == seen {
            return None;
        }
        let unreachable = folded.attachment.is_closed();
        let adapted = adapt(&folded.model, unreachable, unix_epoch_ms(), settled);
        Some((folded.revision, adapted))
    }

    /// The approvals the session waits on, oldest first.
    pub fn pending_approvals(&self) -> Vec<ToolCallIdentity> {
        let folded = self.folded.lock().unwrap();
        actionable_pending_approval_identities_in(&folded.model.frame.items)
    }

    /// What the session is doing, as a board row says it.
    pub fn activity(&self) -> BoardSessionActivity {
        let folded = self.folded.lock().unwrap();
        if folded.attachment.is_closed() {
            return BoardSessionActivity::Unavailable;
        }
        match folded.model.frame.status() {
            None => BoardSessionActivity::Loading,
            Some(SessionStatus::Starting) => BoardSessionActivity::Starting,
            Some(SessionStatus::Running) => BoardSessionActivity::Running,
            Some(SessionStatus::ToolRunning) => BoardSessionActivity::ToolRunning,
            Some(SessionStatus::WaitingForInput) => BoardSessionActivity::WaitingForInput,
            Some(SessionStatus::WaitingForApproval) => BoardSessionActivity::WaitingForApproval,
            Some(SessionStatus::Cancelled) => BoardSessionActivity::Cancelled,
            Some(SessionStatus::Completed) => BoardSessionActivity::Completed,
            Some(SessionStatus::Failed) => BoardSessionActivity::Failed,
            Some(SessionStatus::Paused) => BoardSessionActivity::Paused,
            Some(SessionStatus::Terminated) => BoardSessionActivity::Terminated,
        }
    }

    /// What the session does or asks, in a line (`AgentFrame::words`).
    pub fn words(&self) -> Option<String> {
        let folded = self.folded.lock().unwrap();
        match folded.attachment.is_closed() {
            true => None,
            false => folded.model.frame.words(),
        }
    }

    /// The session's reasoning effort, when its model takes one.
    pub fn effort(&self) -> Option<horizon_shell_runtime::agent_model::EffortSelection> {
        self.folded.lock().unwrap().model.effort.clone()
    }

    /// What the session can be switched to, as its `model` option offers it.
    pub fn models(&self) -> horizon_shell_runtime::agent_model::ModelChoices {
        self.folded.lock().unwrap().model.models.clone()
    }

    pub fn standing(&self) -> Standing {
        let folded = self.folded.lock().unwrap();
        let frame = &folded.model.frame;
        Standing {
            pending_approval: !actionable_pending_approval_identities_in(&frame.items).is_empty(),
            turn_in_flight: state_indicates_turn_in_flight(frame.state()),
        }
    }

    /// The session's title, if it has one it has not given yet.
    pub fn take_title(&self) -> Option<String> {
        let mut title = self.title.lock().unwrap();
        std::mem::take(&mut title.fresh)
            .then(|| title.derived.clone())
            .flatten()
    }

    /// Who to wake when the session has changed.
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }
}
