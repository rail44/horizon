//! The live agent pane: one session's display model in the view, and the
//! view's requests back into the session or the shell's commands.

use std::collections::HashMap;
use std::time::Duration;

use gpui::*;
use horizon_workspace::commands::CommandId;

use super::super::model::ToolCallIdentity;
use super::super::session::AgentSession;
use super::adapter::{transcript_from, Adapted, SettledUnits};
use super::{AgentView, AgentViewEvent, UiState};
use crate::workspace::RunCommand;

pub(crate) struct AgentPane {
    session: Entity<AgentSession>,
    view: Entity<AgentView>,
    approvals: HashMap<String, ToolCallIdentity>,
    /// The ended turns' units from the last adaptation, so a session
    /// notify re-words only the turn in progress.
    settled: SettledUnits,
    _subscriptions: [Subscription; 2],
}

impl AgentPane {
    pub(crate) fn new(
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut settled = SettledUnits::default();
        let Adapted {
            transcript,
            approvals,
        } = transcript_from(session.read(cx), &mut settled);
        let view = cx.new(|cx| AgentView::new(transcript, UiState::default(), window, cx));
        let observe = cx.observe_in(&session, window, |pane: &mut Self, _, window, cx| {
            pane.refresh(window, cx);
        });
        let events = cx.subscribe_in(
            &view,
            window,
            |pane: &mut Self, _, event: &AgentViewEvent, window, cx| {
                pane.on_event(event, window, cx);
            },
        );
        // A background task's age advances without any session update.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update_in(cx, |pane, window, cx| {
                if !pane.session.read(cx).model.tasks.is_empty() {
                    pane.refresh(window, cx);
                }
            });
            if alive.is_err() {
                return;
            }
        })
        .detach();
        Self {
            session,
            view,
            approvals,
            settled,
            _subscriptions: [observe, events],
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Adapted {
            transcript,
            approvals,
        } = transcript_from(self.session.read(cx), &mut self.settled);
        self.approvals = approvals;
        self.view
            .update(cx, |view, cx| view.set_transcript(transcript, window, cx));
    }

    fn on_event(&mut self, event: &AgentViewEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AgentViewEvent::Send(text) => self.session.read(cx).send_user_message(text.clone()),
            AgentViewEvent::Approve(id) => {
                if let Some(identity) = self.approvals.get(id) {
                    self.session.read(cx).approve(identity.clone());
                }
            }
            AgentViewEvent::Deny(id) => {
                if let Some(identity) = self.approvals.get(id) {
                    self.session.read(cx).deny(identity.clone(), None);
                }
            }
            AgentViewEvent::Stop => run(CommandId::CancelAgentTurn, window, cx),
            AgentViewEvent::SwitchModel => run(CommandId::SwitchModel, window, cx),
        }
    }
}

fn run(id: CommandId, window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(RunCommand { id }), cx);
}

impl Focusable for AgentPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.read(cx).focus_handle(cx)
    }
}

impl Render for AgentPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}
