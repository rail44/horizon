use std::collections::HashMap;
use std::time::Instant;

use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};

use crate::agent::frame::AgentFrame;
use crate::session::SessionId;
use crate::terminal::{initial_terminal_text, TerminalFrame};

mod agent_frame_handle;
use agent_frame_handle::AgentFrameHandle;

#[derive(Clone, Debug, Default)]
pub(crate) struct Frames {
    terminal: HashMap<SessionId, TerminalFrame>,
    // Foundation-5 fine-grained agent-frame storage (`docs/reactive-store-
    // design.md`): a coarse membership signal over per-session
    // `AgentFrameHandle`s, each of whose own fields (`state`/`items`/
    // `state_entry`) are independent signals living in a child `Scope` of
    // `agent_scope`. Replaces the old flat `HashMap<SessionId, AgentFrame>`
    // this field used to be -- a reader that grabs one session's handle and
    // reads only its own field signals no longer subscribes to every other
    // session's updates, and a writer (`update_agent_frame`) updates only
    // the field signal(s) that actually changed instead of notifying one
    // signal shared by the whole app.
    //
    // `terminal` above stays a plain (non-signal) map for this slice --
    // `docs/reactive-store-design.md`'s migration ordering does terminal
    // frames next -- so it still relies entirely on whatever signal wraps
    // the whole `Frames` value (`RwSignal<Frames>`, still constructed the
    // same way at every call site) for its own reactivity, same as before
    // this migration.
    agent: RwSignal<im::HashMap<SessionId, AgentFrameHandle>>,
    // The stable parent every session's `AgentFrameHandle` scope is created
    // under -- deliberately not "whatever scope happens to be current" at
    // the first `update_agent_frame` call for a session, which can be a
    // short-lived detached effect scope (e.g. the CLI control-plane
    // bridge's own fold scope -- see `agent::agentd_runtime::
    // fold_agent_session_events`'s doc comment for why that fold already
    // has to defend against exactly this). A handle's signals must outlive
    // whichever caller happened to create them first, so they hang off
    // this dedicated root instead, disposed one session at a time via
    // `remove_session`.
    agent_scope: Scope,
}

impl Frames {
    pub(crate) fn terminal_frame(&self, session_id: SessionId) -> TerminalFrame {
        self.terminal
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| TerminalFrame::from_text(initial_terminal_text()))
    }

    pub(crate) fn update_terminal_output(&mut self, session_id: SessionId, output: String) {
        self.update_terminal_frame(session_id, TerminalFrame::from_text(output));
    }

    pub(crate) fn update_terminal_frame(&mut self, session_id: SessionId, frame: TerminalFrame) {
        self.terminal.insert(session_id, frame);
    }

    /// `session_id`'s agent-frame handle, tracked on membership -- for
    /// callers that need to react to sessions being registered/removed
    /// (e.g. a `dyn_stack` keyed by session id), not to any individual
    /// session's own updates. Everyone else should prefer [`Self::
    /// agent_frame`]/[`Self::agent_frame_untracked`] below, which read one
    /// handle's own field signals instead of walking the whole map.
    pub(crate) fn agent_handle(&self, session_id: SessionId) -> Option<AgentFrameHandle> {
        self.agent.with(|map| map.get(&session_id).cloned())
    }

    fn agent_handle_untracked(&self, session_id: SessionId) -> Option<AgentFrameHandle> {
        self.agent
            .with_untracked(|map| map.get(&session_id).cloned())
    }

    /// Tracked compat read: reconstructs a whole `AgentFrame` value by
    /// reading `session_id`'s own `state`/`items` field signals (plus the
    /// map's membership signal, for a session that doesn't have a handle
    /// yet). Existing call sites (`workspace::view::pane`, `agent::view`)
    /// keep working unchanged against this -- the isolation win is that
    /// this no longer subscribes to any *other* session's frame updates,
    /// which the old single `RwSignal<Frames>` design did. New call sites
    /// that only need one field should prefer reading `agent_handle(id)`'s
    /// own `state()`/`items()` directly instead (`docs/reactive-store-
    /// design.md`'s accessor-boundary note).
    pub(crate) fn agent_frame(&self, session_id: SessionId) -> AgentFrame {
        match self.agent_handle(session_id) {
            Some(handle) => AgentFrame {
                state: handle.state().get(),
                items: handle.items().get(),
            },
            None => AgentFrame::empty(),
        }
    }

    /// Untracked counterpart of [`Self::agent_frame`] -- for one-shot
    /// imperative reads (command handlers deciding what to do next, not
    /// rendering) that must not leave behind a live subscription if they
    /// happen to run from inside an active reactive scope.
    pub(crate) fn agent_frame_untracked(&self, session_id: SessionId) -> AgentFrame {
        match self.agent_handle_untracked(session_id) {
            Some(handle) => AgentFrame {
                state: handle.state().get_untracked(),
                items: handle.items().get_untracked(),
            },
            None => AgentFrame::empty(),
        }
    }

    /// Folds `frame` into `session_id`'s handle, creating one (under
    /// `agent_scope`) on the session's first frame. Only that first-ever
    /// insert notifies the coarse membership signal; every later call
    /// writes straight into the handle's own field signals via
    /// [`AgentFrameHandle::apply_frame`], which updates only the field(s)
    /// that actually changed. Takes `&self` (not `&mut self`, unlike
    /// [`Self::remove_session`]) since every mutation goes through a
    /// signal's own interior mutability rather than restructuring `Frames`
    /// itself -- callable through a plain `.with_untracked(...)` on
    /// `RwSignal<Frames>` without ever notifying that outer signal, which
    /// is what actually keeps an agent-frame write from waking readers of
    /// unrelated state (e.g. `terminal`) bundled into the same `Frames`.
    pub(crate) fn update_agent_frame(&self, session_id: SessionId, frame: AgentFrame) {
        let handle = self.agent_handle_untracked(session_id).unwrap_or_else(|| {
            let handle = AgentFrameHandle::new(self.agent_scope);
            self.agent.update(|map| {
                map.insert(session_id, handle.clone());
            });
            handle
        });
        handle.apply_frame(frame);
    }

    /// When the visible agent session's `AgentFrame.state` last changed --
    /// `None` if the session has never had a frame recorded.
    pub(crate) fn agent_state_entered_at(&self, session_id: SessionId) -> Option<Instant> {
        self.agent_handle(session_id)
            .map(|handle| handle.state_entry().get().entered_at())
    }

    pub(crate) fn remove_session(&mut self, session_id: SessionId) {
        self.terminal.remove(&session_id);
        if let Some(handle) = self
            .agent
            .try_update(|map| map.remove(&session_id))
            .flatten()
        {
            handle.dispose();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Message, MessageRole};

    #[test]
    fn terminal_frame_defaults_to_initial_terminal_text() {
        let frames = Frames::default();
        let frame = frames.terminal_frame(SessionId::new());

        assert!(frame.text.contains("Terminal plugin"));
    }

    #[test]
    fn terminal_output_updates_frame_by_session() {
        let session_id = SessionId::new();
        let mut frames = Frames::default();

        frames.update_terminal_output(session_id, "Terminal exited".to_string());

        assert_eq!(frames.terminal_frame(session_id).text, "Terminal exited");
    }

    #[test]
    fn agent_frame_defaults_empty_and_updates_by_session() {
        let session_id = SessionId::new();
        let frames = Frames::default();
        assert_eq!(frames.agent_frame(session_id), AgentFrame::empty());

        let frame = AgentFrame {
            state: None,
            items: vec![crate::agent::frame::AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                text: "hello".to_string(),
            })],
        };
        frames.update_agent_frame(session_id, frame.clone());

        assert_eq!(frames.agent_frame(session_id), frame);
        assert_eq!(frames.agent_frame_untracked(session_id), frame);
    }

    #[test]
    fn agent_state_entered_at_resets_only_on_state_change() {
        use crate::agent::contract::SessionState;

        let session_id = SessionId::new();
        let frames = Frames::default();
        assert_eq!(frames.agent_state_entered_at(session_id), None);

        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::Running),
                items: Vec::new(),
            },
        );
        let first_entered_at = frames
            .agent_state_entered_at(session_id)
            .expect("entry recorded for a session with a state");

        // Re-observing the same state must not reset the timestamp.
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::Running),
                items: vec![crate::agent::frame::AgentFrameItem::Message(Message {
                    role: MessageRole::Assistant,
                    text: "still running".to_string(),
                })],
            },
        );
        assert_eq!(
            frames.agent_state_entered_at(session_id),
            Some(first_entered_at)
        );

        // A genuine state transition must produce a fresh timestamp.
        std::thread::sleep(std::time::Duration::from_millis(5));
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::WaitingForUser),
                items: Vec::new(),
            },
        );
        let second_entered_at = frames
            .agent_state_entered_at(session_id)
            .expect("entry still recorded after a state change");
        assert!(second_entered_at > first_entered_at);
    }

    /// `remove_session` must dispose the departing session's handle scope
    /// (so its signals are actually freed, not just unreachable through
    /// the map) and leave a fresh handle creatable afterward -- proven via
    /// the same `try_get_untracked` probe `agent_frame_handle`'s own
    /// `dispose_drops_the_handles_signals` test uses.
    #[test]
    fn remove_session_disposes_the_agent_handles_signals() {
        let session_id = SessionId::new();
        let mut frames = Frames::default();
        frames.update_agent_frame(session_id, AgentFrame::empty());
        let handle = frames
            .agent_handle(session_id)
            .expect("handle exists after the first frame");

        frames.remove_session(session_id);

        assert_eq!(handle.state().try_get_untracked(), None);
        assert!(frames.agent_handle(session_id).is_none());
    }
}
