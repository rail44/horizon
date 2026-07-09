use std::collections::HashMap;

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

    /// Reactive-sufficiency check for `docs/reactive-store-design.md`'s
    /// slice-2 migration: `workspace::view::pane`'s `pending_approval`
    /// closure was moved off the tracked-outer `Frames::agent_frame` onto
    /// this exact shape (`frames.with_untracked` to grab the handle via the
    /// still-tracked `agent_handle`, then a field-scoped `items().with(...)`
    /// read) specifically to *drop* the outer `RwSignal<Frames>`
    /// subscription -- this proves that drop didn't also break the
    /// approval-focus wiring's ability to notice a freshly-arrived
    /// `ApprovalRequested` item (the y/n buttons' actual signal source).
    ///
    /// Models an already-running session (a prior frame establishes the
    /// handle first, matching every real approval prompt -- a session is
    /// never brand new the moment it asks for one), not a session's
    /// very-first-ever frame: `update_agent_frame`'s handle-creation branch
    /// (`Frames::update_agent_frame`) does an un-batched membership insert
    /// *then* `apply_frame`, which are two separate signal writes and so,
    /// for a subscriber tracking both membership and `items`, two separate
    /// re-runs -- expected and harmless (it only affects a session's first
    /// frame), but a distraction from what this test is actually checking.
    #[test]
    fn field_scoped_pending_approval_read_wakes_on_a_new_approval_request() {
        use crate::agent::contract::{ApprovalRequest, SessionState, ToolCallId};
        use crate::agent::frame::{pending_approval_call_ids_in, AgentFrameItem};
        use floem::reactive::create_effect;
        use std::cell::RefCell;
        use std::rc::Rc;

        let session_id = SessionId::new();
        // Wrapped in an outer `RwSignal`, matching how every real call site
        // holds `Frames` -- the property under test is specifically about
        // dropping *this* signal's subscription, so a bare (unwrapped)
        // `Frames` wouldn't exercise it.
        let frames = RwSignal::new(Frames::default());
        // Establishes the handle before the effect below ever subscribes,
        // so the effect's own first run already sees a real (not just-
        // created) handle -- an already-running session, not one whose
        // very first frame happens to be the approval request.
        frames.with_untracked(|frames| {
            frames.update_agent_frame(
                session_id,
                AgentFrame {
                    state: Some(SessionState::Running),
                    items: Vec::new(),
                },
            );
        });

        let runs = Rc::new(RefCell::new(0));
        let runs_probe = runs.clone();
        let last_pending: Rc<RefCell<Option<ToolCallId>>> = Rc::new(RefCell::new(None));
        let last_pending_probe = last_pending.clone();
        create_effect(move |_| {
            let pending = frames
                .with_untracked(|frames| frames.agent_handle(session_id))
                .and_then(|handle| {
                    handle
                        .items()
                        .with(|items| pending_approval_call_ids_in(items).into_iter().next())
                });
            *runs_probe.borrow_mut() += 1;
            *last_pending_probe.borrow_mut() = pending;
        });
        assert_eq!(*runs.borrow(), 1, "the initial run");
        assert_eq!(*last_pending.borrow(), None);

        let call_id = ToolCallId("call-1".to_string());
        frames.with_untracked(|frames| {
            frames.update_agent_frame(
                session_id,
                AgentFrame {
                    state: Some(SessionState::WaitingForApproval),
                    items: vec![AgentFrameItem::ApprovalRequested(ApprovalRequest {
                        call_id: call_id.clone(),
                        reason: "writes a file".to_string(),
                    })],
                },
            );
        });

        assert_eq!(
            *runs.borrow(),
            2,
            "a freshly-arrived ApprovalRequested item must wake a subscriber \
             that only reads items() -- the outer-untracked, handle-scoped \
             shape pending_approval now uses"
        );
        assert_eq!(*last_pending.borrow(), Some(call_id));
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
