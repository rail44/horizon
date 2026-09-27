//! The per-session agent model entity, the agent twin of
//! `terminal::session::TerminalSession`: owns the attachment's
//! [`RuntimeLink`] and the fold ([`AgentModel`]) of the session's ACP
//! traffic, independent of any pane view. Owned by the shell's
//! agent-session store, so close-vs-terminate holds for agent panes exactly
//! as for terminals. Everything here that is not specific to *agent*
//! sessions -- the link, the event-stream bridge, the notify coalescer --
//! lives in `crate::runtime`.

use std::time::Instant;

use gpui::*;
use horizon_workspace::SessionId;

use super::model::{
    actionable_pending_approval_identities_in, AgentFrameItem, AgentModel, MessageRole,
    SessionState, ToolCallIdentity,
};
use crate::runtime::{
    AgentCommand, AgentSessionHandle, AgentUpdate, AttachmentState, NotifyCoalescer,
    NotifyDecision, RuntimeLink,
};
use crate::title::derive_session_title;

pub(crate) struct AgentSession {
    /// The fold of this attachment's traffic: the transcript frame, the
    /// `model` config option's selection, and the running background-task
    /// rows.
    pub(crate) model: AgentModel,
    pub(crate) attachment: AttachmentState,
    _wire: Option<AgentSessionHandle>,
    attachment_generation: u64,
    /// The command channel to `horizon-agentd` plus its reachability
    /// bookkeeping. Its notify pump forwards to the existing
    /// `cx.observe(&session, ...)` in the view (`view.rs`), which already
    /// re-renders on any notify from this entity.
    link: RuntimeLink<AgentCommand>,
    /// The workspace session id this agent belongs to -- the title side of
    /// the terminal's same-named field: used to report the derived title
    /// below to the shell.
    session_id: SessionId,
    /// The one content-derived tab title (the first real user message,
    /// via [`derive_title_from_items`]), or `None` until one exists. The
    /// `Some` guard is what makes derivation run exactly once.
    derived_title: Option<String>,
    /// Whether the model-based title refinement (see
    /// [`AgentSession::refine_title_with_model`]) has already been fired
    /// for this attach: at most one summarizer call per session per
    /// attach, whatever its outcome -- a failed call stays failed (the
    /// raw first-message title is already in place, so a retry buys
    /// nothing but more calls).
    title_refine_attempted: bool,
    /// Reports the derived title to the shell, which folds it into the
    /// workspace model (`Workspace::set_session_derived_title` via
    /// `wire_session_title_updates`) -- the agent side of the terminal's
    /// same-named channel.
    title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
    /// Gates the event pump's `cx.notify()` calls to the terminal-parity
    /// ~60Hz window. Plain `mut` state, no `Cell`: unlike the link's
    /// reachability, it is only touched under `Entity::update`.
    notify_coalescer: NotifyCoalescer,
}

impl AgentSession {
    /// Wraps a freshly started (or attached) session handle: pumps its
    /// event stream through the fold onto this entity. The pump task is
    /// owned by the entity — it ends when the entity drops.
    pub(crate) fn new(
        handle: AgentSessionHandle,
        session_id: SessionId,
        title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_attachment(handle, session_id, title_tx, 0, cx)
    }

    fn new_attachment(
        mut handle: AgentSessionHandle,
        session_id: SessionId,
        title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
        attachment_generation: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut events = handle.take_events();
        cx.spawn(async move |this, cx| {
            while let Some(update) = events.recv().await {
                let apply = this.update(cx, |session: &mut AgentSession, cx| {
                    if session.attachment_generation != attachment_generation {
                        return;
                    }
                    let event = match update {
                        AgentUpdate::State(state) => {
                            // Restoring opens the attachment's bootstrap:
                            // the fold starts over.
                            if state == AttachmentState::Restoring {
                                session.model = AgentModel::default();
                            }
                            session.attachment = state;
                            if session.attachment.is_closed() {
                                session.link.mark_unreachable();
                            }
                            if session.attachment.is_ready() {
                                session.link.mark_reachable();
                            }
                            cx.notify();
                            return;
                        }
                        AgentUpdate::Event(event) => *event,
                    };
                    session.model.apply(event);
                    // Title derivation runs only until it produces one: the
                    // first user message fixes "what this session is about",
                    // and a resumed session's replayed transcript surfaces
                    // that same message, so the re-derived title matches the
                    // one persistence kept. The raw title ships immediately;
                    // the model-refined replacement is fired right after
                    // (`refine_title_with_model`), once per attach.
                    session.derive_title_from_first_user_message();
                    session.refine_title_with_model(cx);
                    // The fold above is already applied -- only the notify
                    // is coalesced, so a burst's re-renders cap at the
                    // window rate while state never lags.
                    session.notify_coalesced(cx);
                });
                if apply.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |session: &mut AgentSession, cx| {
                if session.attachment_generation == attachment_generation {
                    session.attachment.stream_ended();
                    session.link.mark_unreachable();
                    cx.notify();
                }
            });
        })
        .detach();

        Self {
            model: AgentModel::default(),
            attachment: AttachmentState::Connecting,
            session_id,
            derived_title: None,
            title_refine_attempted: false,
            title_tx,
            link: RuntimeLink::new(handle.sender(), cx),
            notify_coalescer: NotifyCoalescer::default(),
            _wire: Some(handle),
            attachment_generation,
        }
    }

    /// Replaces a dead board-linked attachment in the same view entity.
    /// Drop the old UUID route before registering the new route.
    pub(crate) fn reattach(
        &mut self,
        attach: impl FnOnce() -> AgentSessionHandle,
        cx: &mut Context<Self>,
    ) {
        self._wire.take();
        let generation = self.attachment_generation + 1;
        *self = Self::new_attachment(
            attach(),
            self.session_id,
            self.title_tx.clone(),
            generation,
            cx,
        );
        cx.notify();
    }

    /// Fixes this session's tab title from the transcript's first real
    /// user message ("what was asked"), exactly once: the `derived_title`
    /// guard keeps later turns from retitling the tab, and a session with
    /// no usable user message yet keeps its default title until one
    /// arrives.
    fn derive_title_from_first_user_message(&mut self) {
        if self.derived_title.is_some() {
            return;
        }
        let Some(derived) = derive_title_from_items(&self.model.frame.items) else {
            return;
        };
        self.derived_title = Some(derived.clone());
        let _ = self
            .title_tx
            .unbounded_send((self.session_id, Some(derived)));
    }

    /// Replaces the raw first-message title with a model-summarized one,
    /// at most once per attach. The raw title (above) always ships first
    /// so the tab is never untitled while the call is in flight; this
    /// fires the background summarizer and pushes its result through the
    /// same `title_tx` when it lands. Fire-and-forget with a total
    /// fallback: any failure -- no `OPENAI_API_KEY`, timeout, transport
    /// error, unusable reply -- is a silent no-op that leaves the raw
    /// title showing. A resumed session's replay re-derives the raw
    /// title (overwriting the persisted refined one) and then re-runs
    /// this, so the refined title is re-derived per attach rather than
    /// distinguished in persistence -- one cheap small-model call, and no
    /// new title-provenance state.
    fn refine_title_with_model(&mut self, cx: &mut Context<Self>) {
        if self.title_refine_attempted || self.derived_title.is_none() {
            return;
        }
        let Some(first_message) = first_user_message_text(&self.model.frame.items) else {
            return;
        };
        self.title_refine_attempted = true;
        let Some(connection) = super::auxiliary::title_client(cx) else {
            return;
        };
        let session_id = self.session_id;
        let title_tx = self.title_tx.clone();
        cx.background_executor()
            .spawn(async move {
                // The model's reply is untrusted text: it goes through the
                // same sanitizer (control-char collapse, whitespace
                // collapse, 40-char clamp) every other title source runs
                // through, so a chatty or over-long reply can never reach
                // the tab strip unclamped.
                let summary =
                    horizon_agent::summarize::summarize_session_title(&connection, &first_message);
                if let Some(title) = summary.and_then(|text| derive_session_title(&text)) {
                    let _ = title_tx.unbounded_send((session_id, Some(title)));
                }
            })
            .detach();
    }

    /// The event pump's coalesced `cx.notify()`: leading edge fires
    /// immediately, and inside the window a one-shot trailing flush is
    /// armed instead -- the same `cx.spawn` +
    /// `cx.background_executor().timer(...)` shape as the view's
    /// running-card ticker, entity-owned via the weak handle (a flush
    /// against a dropped entity is a no-op and ends the task).
    fn notify_coalesced(&mut self, cx: &mut Context<Self>) {
        match self.notify_coalescer.on_event(Instant::now()) {
            NotifyDecision::Notify => cx.notify(),
            NotifyDecision::Arm(delay) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |session, cx| {
                        session.notify_coalescer.on_flush(Instant::now());
                        cx.notify();
                    });
                })
                .detach();
            }
            NotifyDecision::Pending => {}
        }
    }

    /// Whether the agentd command channel is known dead (backlog #35).
    /// The view's status line consults this to surface the state instead
    /// of leaving a failed send as a silent no-op.
    pub(crate) fn runtime_unreachable(&self) -> bool {
        self.link.is_unreachable()
    }

    /// The permission requests still awaiting an approve/deny decision in
    /// the open turn, oldest first.
    pub(crate) fn pending_approval_identities(&self) -> Vec<ToolCallIdentity> {
        actionable_pending_approval_identities_in(&self.model.frame.items)
    }

    /// Whether the session's current turn is actively running (as opposed
    /// to idle or waiting on an approval decision).
    pub(crate) fn turn_in_flight(&self) -> bool {
        matches!(
            self.model.frame.state(),
            Some(SessionState::Running | SessionState::ToolRunning)
        )
    }

    /// Whether the session is idle on a guard-halted turn, i.e.
    /// `CommandId::ContinueAgentTurn` has something to resume.
    pub(crate) fn turn_halted(&self) -> bool {
        self.model.frame.halted_awaiting_continue()
    }

    pub(crate) fn send_user_message(&self, text: String) {
        self.link.dispatch(AgentCommand::Prompt { text });
    }

    pub(crate) fn approve(&self, identity: ToolCallIdentity) {
        self.link.dispatch(AgentCommand::Approve { identity });
    }

    pub(crate) fn deny(&self, identity: ToolCallIdentity, reason: Option<String>) {
        self.link.dispatch(AgentCommand::Deny { identity, reason });
    }

    /// Cancels the running turn; the attachment answers every held
    /// permission request `Cancelled`.
    pub(crate) fn cancel(&self) {
        self.link.dispatch(AgentCommand::Cancel);
    }

    /// Resumes a turn the turn-loop guard halted, without composing a new
    /// user message -- `CommandId::ContinueAgentTurn`'s session-level
    /// action. A safe no-op daemon-side when nothing is halted.
    pub(crate) fn continue_turn(&self) {
        self.link.dispatch(AgentCommand::ContinueTurn);
    }

    /// The explicit destructive half of close-vs-terminate.
    pub(crate) fn shutdown(&self) {
        self.link.dispatch(AgentCommand::Close);
    }

    /// The daemon-side session id, for connection-level requests that
    /// address the session by id (`session/set_config_option` -- the model
    /// picker's confirm path resolves it at confirm time). `None` only in
    /// the mid-reload gap where the attachment handle is being replaced.
    pub(crate) fn daemon_session_id(&self) -> Option<horizon_acp::SessionId> {
        self._wire.as_ref().map(|handle| handle.session_id())
    }
}

/// The transcript's first real user message text -- the same selection
/// rule [`derive_title_from_items`] applies (blank messages are skipped,
/// as are system-authored ones) -- but *unclamped*: this is the
/// summarizer's input, not a tab label. `None` while no usable message
/// exists yet.
fn first_user_message_text(items: &[AgentFrameItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        AgentFrameItem::Message(message) if message.role == MessageRole::User => {
            // `derive_session_title` returning `Some` is exactly the
            // "this text would survive as a title" predicate, reused
            // here so both paths skip the same messages.
            derive_session_title(&message.text).map(|_| message.text.clone())
        }
        _ => None,
    })
}

/// The title text for a transcript: the first user-authored message whose
/// text survives [`derive_session_title`] (blank messages are skipped, as
/// are system-authored ones -- [`MessageRole::TaskNotification`] and the
/// other injected roles deliberately never title a tab), or `None` while
/// no such message exists yet.
fn derive_title_from_items(items: &[AgentFrameItem]) -> Option<String> {
    first_user_message_text(items).and_then(|text| derive_session_title(&text))
}

#[cfg(test)]
mod tests {
    // `super::*` is avoided here for the same reason `src/terminal/tests.rs`
    // records: the parent module's `use gpui::*` glob-exports gpui's own
    // `test` attribute macro, which shadows the built-in `#[test]` and
    // sends plain tests through gpui's async harness instead (whose
    // expansion blows the crate's macro recursion limit).
    use super::derive_title_from_items;
    use super::first_user_message_text;
    use crate::agent::model::{AgentFrameItem, Message, MessageRole};

    // Explicitly-typed builders keep the literals shallow and the
    // assertions' intent readable.
    fn message(role: MessageRole, text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: text.to_string(),
            role,
            text: text.to_string(),
        })
    }

    fn user_message(text: &str) -> AgentFrameItem {
        message(MessageRole::User, text)
    }

    fn task_notification(text: &str) -> AgentFrameItem {
        message(MessageRole::TaskNotification, text)
    }

    #[test]
    fn the_first_usable_user_message_supplies_the_title() {
        let items = vec![
            task_notification("task done"),
            user_message("  fix the flaky test in\nsession.rs  "),
        ];
        assert_eq!(
            derive_title_from_items(&items),
            Some("fix the flaky test in session.rs".to_string())
        );
    }

    #[test]
    fn a_blank_user_message_is_skipped_for_a_later_usable_one() {
        let items = vec![user_message("   "), user_message("real ask")];
        assert_eq!(
            derive_title_from_items(&items),
            Some("real ask".to_string())
        );
    }

    #[test]
    fn first_user_message_text_is_the_unclamped_source_message() {
        // The summarizer's input must be the raw message -- a 40-char
        // clamp here would summarize the already-truncated label instead
        // of what the user actually asked.
        let long = "please investigate ".repeat(20);
        let items = vec![task_notification("task done"), user_message(&long)];
        assert_eq!(first_user_message_text(&items), Some(long.clone()));
        assert!(derive_title_from_items(&items).unwrap().chars().count() < long.chars().count());
    }

    #[test]
    fn first_user_message_text_follows_the_same_skip_rules_as_the_title() {
        let items = vec![
            user_message("   "),
            task_notification("task done"),
            user_message("real ask"),
        ];
        assert_eq!(
            first_user_message_text(&items),
            Some("real ask".to_string())
        );
        assert_eq!(
            first_user_message_text(&[task_notification("task done")]),
            None
        );
    }

    #[test]
    fn a_transcript_without_a_user_message_yields_no_title() {
        assert_eq!(
            derive_title_from_items(&[AgentFrameItem::MemoryCheckpointMissed]),
            None
        );
        assert_eq!(derive_title_from_items(&[]), None);
    }
}
