//! The continue stage of the turn loop: after a round the model ended with
//! a plain reply, the session's modules are asked whether the turn goes on
//! (`modules::SessionModule::after_round`; the decision-model check is
//! `modules::DecisionContinuation`), and a continuation runs here.

use super::super::completion::CompletionStop;
use super::super::conversation::Prompt;
use super::state::SessionLoopState;
use super::{TurnCompletion, deterministic_rig_response};
use crate::contract::{
    Command, ContinuationVerdict, ConversationInputKind, Event, Message as AgentMessage,
    MessageRole, SessionState,
};
use crate::modules::RoundContext;

/// Per-turn inputs to the continue stage, reset when an interaction starts.
#[derive(Clone, Default)]
pub(crate) struct ContinuationState {
    /// The text of the input that started the current turn.
    pub(crate) request: String,
    /// Tool calls the provider rounds of the current turn requested.
    pub(crate) tool_calls: usize,
    /// Harness continuations the current turn has run.
    pub(crate) count: u32,
}

impl ContinuationState {
    pub(crate) fn begin_turn(&mut self, request: &str) {
        self.request = request.to_string();
        self.tool_calls = 0;
        self.count = 0;
    }
}

impl SessionLoopState {
    /// Asks the session's modules about `outcome` and, while one answers
    /// with a prompt, continues the turn with it (no `TurnEnded` in
    /// between). Returns `None`
    /// only when the check is installed and a shutdown is queued, as
    /// truncation recovery does; otherwise the outcome to carry on
    /// with, which may be a re-run's (completing, with tool calls, or
    /// truncated).
    pub(crate) async fn handle_continuation_check(
        &mut self,
        mut outcome: TurnCompletion,
    ) -> Option<TurnCompletion> {
        loop {
            if !outcome.is_completing() || !self.config.model_in_play() {
                return Some(outcome);
            }
            self.collect_inputs();
            if self.shutdown_queued() {
                return None;
            }
            if self.has_pending_stop() || self.inputs.has_additions() {
                return Some(outcome);
            }
            let reply = match &outcome.stop {
                CompletionStop::Finished { text } if !text.trim().is_empty() => text.clone(),
                _ => return Some(outcome),
            };
            let cx = RoundContext {
                request: &self.continuation.request,
                reply: &reply,
                tool_calls: self.continuation.tool_calls,
                continuations_so_far: self.continuation.count,
            };
            let mut decision = None;
            let modules = self.modules.clone();
            for module in modules.iter() {
                if let Some(answer) = module.after_round(&cx).await {
                    decision = Some(answer);
                    break;
                }
            }
            let Some(mut decision) = decision else {
                return Some(outcome);
            };
            // Input that arrived during the decision takes precedence over a
            // continuation.
            if decision.prompt.is_some() {
                self.collect_inputs();
                if self.shutdown_queued() {
                    decision.verdict.continued = false;
                    self.emit_verdict(decision.verdict);
                    return None;
                }
                if self.has_pending_stop() || self.inputs.has_additions() {
                    decision.prompt = None;
                    decision.verdict.continued = false;
                }
            }
            self.emit_verdict(decision.verdict);
            let Some(prompt) = decision.prompt else {
                return Some(outcome);
            };

            self.continuation.count += 1;
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::Running).into());
            let _ = self.events_tx.send(
                Event::MessageCommitted(AgentMessage {
                    role: MessageRole::AutoContinue,
                    text: prompt.to_string(),
                })
                .into(),
            );
            outcome = self
                .run_cancellable_turn(
                    Prompt::input(ConversationInputKind::Continuation, prompt),
                    || deterministic_rig_response("next-speaker continuation"),
                )
                .await;
            if outcome.stop.truncation().is_some() {
                return Some(outcome);
            }
        }
    }

    fn shutdown_queued(&self) -> bool {
        self.inbox
            .iter()
            .any(|command| matches!(command, Command::Shutdown))
    }

    fn emit_verdict(&self, verdict: ContinuationVerdict) {
        let _ = self
            .events_tx
            .send(Event::ContinuationVerdict(verdict).into());
    }
}

#[cfg(test)]
mod tests;
