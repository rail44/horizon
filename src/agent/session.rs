//! The per-session agent model entity, the agent twin of
//! `terminal::session::TerminalSession`: owns the command sender and the
//! live fold (`horizon_agent::live::LiveState`) of the session's event
//! stream into an `AgentFrame`, independent of any pane view. Owned by
//! the shell's agent-session store, so close-vs-terminate holds for
//! agent panes exactly as for terminals.

use crossbeam_channel::Sender;
use futures::StreamExt;
use gpui::*;
use horizon_agent::contract::{Command, ToolCallId};
use horizon_agent::frame::AgentFrame;
use horizon_agent::live::LiveState;

use crate::sessiond::AgentSessionHandle;

pub struct AgentSession {
    commands: Sender<Command>,
    pub frame: AgentFrame,
    /// The session's resolved model id, if known -- set once a
    /// `horizon_agent::wire::Control::SessionModel` announcement (folded via
    /// `LiveState::session_model`) arrives, either right after a fresh
    /// session starts or alongside a resumed session's replay. `None` until
    /// then (e.g. a role-less session, or a provider with no resolvable
    /// model -- see `contract::Provider::resolved_model`'s doc comment).
    /// Read by the composer's model chip alongside `turns::latest_turn_model`
    /// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum for the precedence between the two.
    pub model: Option<String>,
    _wire: AgentSessionHandle,
}

impl AgentSession {
    /// Wraps a freshly started (or attached) session handle: pumps its
    /// event stream through the live fold onto this entity. The pump task
    /// is owned by the entity — it ends when the entity drops.
    pub(crate) fn new(handle: AgentSessionHandle, cx: &mut Context<Self>) -> Self {
        let commands = handle.sender();
        let events = handle.events();

        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                if async_tx.unbounded_send(event).is_err() {
                    return;
                }
            }
        });
        let live = LiveState::with_disabled_persistence();
        cx.spawn(async move |this, cx| {
            while let Some(event) = async_rx.next().await {
                let apply = this.update(cx, |session: &mut AgentSession, cx| {
                    session.frame = live.extend_provider_events(std::iter::once(event));
                    session.model = live.session_model();
                    cx.notify();
                });
                if apply.is_err() {
                    return;
                }
            }
        })
        .detach();

        Self {
            commands,
            frame: AgentFrame::empty(),
            model: None,
            _wire: handle,
        }
    }

    pub fn send_user_message(&self, text: String) {
        let _ = self.commands.send(Command::UserMessage { text });
    }

    pub fn approve(&self, call_id: ToolCallId) {
        let _ = self.commands.send(Command::ApproveToolCall { call_id });
    }

    pub fn deny(&self, call_id: ToolCallId) {
        let _ = self.commands.send(Command::DenyToolCall {
            call_id,
            reason: None,
        });
    }

    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel { request_id: None });
    }

    /// The explicit destructive half of close-vs-terminate.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}
