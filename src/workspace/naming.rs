//! The agent names the sessions the shell starts: the shell holds a new
//! agent session under an id of its own until the agent answers with its
//! name, then keeps it under that.

use horizon_control::contract::EnvelopeBody;
use horizon_control::host::executor::error_body;
use horizon_workspace::SessionId;

use super::Shell;

/// An answer the control plane waits for until the agent has named the
/// session it asked for.
pub(super) type Awaiting = Box<dyn FnOnce(EnvelopeBody) + Send>;

impl Shell {
    /// The session started as `was` is `is` to the agent: kept under `is`
    /// from here, and told to whoever asked for it.
    pub(crate) fn rename_agent_session(&self, was: SessionId, is: SessionId) {
        let kept = self.kept();
        let session = kept.agents.borrow_mut().remove(&was);
        if let Some(session) = session {
            kept.agents.borrow_mut().insert(is, session);
        }
        let start = kept.starting.borrow_mut().remove(&was);
        if let Some(start) = start {
            kept.starting.borrow_mut().insert(is, start);
        }
        // (Taken before the workspace changes, which lets go of the waits
        // on sessions it no longer has.)
        let awaiting = kept.unnamed.borrow_mut().remove(&was);
        self.model_changed(|model| model.rename_session(was, is));
        for reply in awaiting.into_iter().flatten() {
            reply(EnvelopeBody::Ok {
                session_id: Some(is.as_uuid().to_string()),
            });
        }
    }

    /// The session started as `was` was never named: it did not start.
    pub(crate) fn agent_session_unnamed(&self, was: SessionId, error: &str) {
        let awaiting = self.kept().unnamed.borrow_mut().remove(&was);
        for reply in awaiting.into_iter().flatten() {
            reply(error_body(error));
        }
    }

    /// Answers `reply` once the agent session `session` is named, if it is
    /// not yet; hands `reply` back otherwise.
    pub(super) fn answer_when_named(
        &self,
        session: SessionId,
        reply: Awaiting,
    ) -> Result<(), Awaiting> {
        match self.kept().unnamed.borrow_mut().get_mut(&session) {
            Some(awaiting) => {
                awaiting.push(reply);
                Ok(())
            }
            None => Err(reply),
        }
    }

    /// Lets go of the waits on sessions the workspace no longer has.
    pub(super) fn forget_unnamed(&self, sessions: &std::collections::HashSet<SessionId>) {
        let gone: Vec<_> = (self.kept().unnamed.borrow().keys())
            .filter(|session| !sessions.contains(session))
            .copied()
            .collect();
        for session in gone {
            self.agent_session_unnamed(session, "the session closed before it started");
        }
    }
}
