//! One agent session as a pane sees it: commands in, the session's model
//! folded from what the daemon sends, and someone to wake when it has
//! changed. A session outlives a pane of it: the model is kept for the
//! next pane to draw.

use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::Sender;
use horizon_shell_runtime::agent_model::{
    AgentModel, ToolCallIdentity, actionable_pending_approval_identities_in,
    state_indicates_turn_in_flight,
};
use horizon_shell_runtime::agent_view::adapter::{Adapted, SettledUnits, adapt, unix_epoch_ms};
use horizon_shell_runtime::{AgentCommand, AgentSessionHandle, AgentUpdate, AttachmentState};

/// The model as folded so far, and how many updates went into it.
#[derive(Default)]
struct Folded {
    model: AgentModel,
    attachment: AttachmentState,
    revision: u64,
}

type Wake = Arc<Mutex<Option<Waker>>>;

pub struct AgentSession {
    commands: Sender<AgentCommand>,
    folded: Arc<Mutex<Folded>>,
    waker: Wake,
    /// The attachment to the agent runtime: letting go of it detaches the
    /// session, which goes on.
    _attachment: AgentSessionHandle,
}

/// What the commands' availability is decided from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    pub pending_approval: bool,
    pub turn_in_flight: bool,
    pub paused_turn: bool,
}

impl AgentSession {
    /// A session of `horizon-agentd`'s, through the runtime client.
    pub fn attached(mut handle: AgentSessionHandle) -> Self {
        let (folded, waker) = (Arc::<Mutex<Folded>>::default(), Wake::default());
        let (folding, woken) = (Arc::clone(&folded), Arc::clone(&waker));
        let mut updates = handle.take_events();
        std::thread::spawn(move || {
            let changed = |change: &mut dyn FnMut(&mut Folded)| {
                let mut folded = folding.lock().unwrap();
                change(&mut folded);
                folded.revision += 1;
                drop(folded);
                if let Some(waker) = woken.lock().unwrap().as_ref() {
                    waker.wake_by_ref();
                }
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
            waker,
            _attachment: handle,
        }
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

    pub fn standing(&self) -> Standing {
        let folded = self.folded.lock().unwrap();
        let frame = &folded.model.frame;
        Standing {
            pending_approval: !actionable_pending_approval_identities_in(&frame.items).is_empty(),
            turn_in_flight: state_indicates_turn_in_flight(frame.state()),
            paused_turn: frame.halted_awaiting_continue(),
        }
    }

    /// Who to wake when the session has changed.
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }
}
