//! Fresh interactions started by owner input or a completed background task.

use rig_core::completion::Message;

use super::state::SessionLoopState;
use super::{deterministic_rig_response, rig_tool_result_message};

impl SessionLoopState {
    pub(super) async fn handle_user_message(&mut self, text: String) {
        self.pause_inputs(false);
        // A user message starts a new interaction rather than
        // joining the previous turn's tool batch. This command can
        // arrive while any kind of tool is still running or
        // awaiting approval, so retire the whole old batch before
        // asking the provider to handle the new message. Otherwise
        // those old call ids remain in `pending_tool_calls` and a
        // result from the new turn is mistaken for a non-final
        // member of the old batch, leaving the session waiting
        // forever.
        if self.cancel_outstanding_tool_calls() {
            self.emit_cancelled_turn();
        }
        self.begin_interaction();
        let _ = self.events_tx.send(
            crate::contract::Event::MessageCommitted(crate::contract::Message {
                role: crate::contract::MessageRole::User,
                text: text.clone(),
            })
            .into(),
        );
        // An owner message opens a Mixture-of-Agents pass; the
        // aggregator's turn runs once every proposer has
        // answered. The message joins the conversation only
        // after the pass, which carries it separately from the
        // conversation so far.
        let pass = self.run_moa_pass(&text).await;
        self.moa_conversation.record_owner(text.clone());
        if let super::moa::PassOutcome::Cancelled = pass {
            return;
        }
        let (prompt, injected) = self.inject_task_notification(Message::user(text.clone()));
        let fallback_text = injected.unwrap_or(text);
        self.run_turn(prompt, move || deterministic_rig_response(&fallback_text))
            .await;
    }

    pub(super) async fn handle_task_wake(&mut self) {
        if self.inputs_paused {
            return;
        }
        // A background `task` child finished. If a tool batch is
        // still outstanding -- which includes a call parked on an
        // approval -- a provider round is still coming, and the
        // drain that runs before it will carry the notification
        // instead; nothing to do here. Otherwise the turn has
        // already ended, so the notification becomes a new turn's
        // synthetic input. That turn is an ordinary one:
        // `Event::TurnEnded` remains the only turn boundary
        // external monitors need to trust.
        if !self.pending_tool_calls.is_empty() {
            return;
        }
        let Some(notification) = crate::tools::take_notification(self.session_id) else {
            return;
        };
        let text = notification.text;
        self.begin_interaction();
        let _ = self
            .events_tx
            .send(crate::tools::notification_event(text.clone()).into());
        self.report_task_failures(notification.failures);
        let fallback_text = text.clone();
        self.run_turn(Message::user(text), move || {
            deterministic_rig_response(&fallback_text)
        })
        .await;
    }

    /// Settle a guard-halted result before the next request, so every tool
    /// call in provider history has a result. Fresh interactions also reset
    /// the tool-loop guards and standing-memory checkpoint.
    fn begin_interaction(&mut self) {
        if let Some((result, tool_id)) = self.pending_halt_result.take() {
            self.rig_history
                .push(rig_tool_result_message(&result, &tool_id));
        }
        self.guard.reset();
        self.memory_satisfied = false;
        self.memory_reminded = false;
        let _ = self.events_tx.send(
            crate::contract::Event::StateChanged(crate::contract::SessionState::Running).into(),
        );
    }
}
