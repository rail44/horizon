//! Provider rounds, recovery, checkpoints and completion of an interaction.

use super::super::conversation::Prompt;
use crate::contract::ConversationInputKind;

use std::collections::HashMap;

use rig_core::completion::Message;
use tokio_util::sync::CancellationToken;

use crate::{
    config::RigAgentConfig,
    contract::{
        Command, Error, Event, Message as AgentMessage, MessageRole, SessionState, ToolCallId,
        ToolCallResult, TurnEndReason,
    },
};

use super::super::completion::{CompletionStop, Truncation};
use super::state::SessionLoopState;
use super::{ToolCallDescriptor, TurnCompletion, complete_rig_turn, deterministic_rig_response};

impl SessionLoopState {
    pub(super) fn retain_prompt(&mut self, prompt: Prompt) -> bool {
        if let Err(message) = self.rig_history.append_prompt(prompt, &self.events_tx) {
            let _ = self.events_tx.send(Event::Error(Error { message }).into());
            self.inbox.push_front(Command::Shutdown);
            false
        } else {
            true
        }
    }

    /// The full turn-execution pipeline as one call: run a cancellable turn,
    /// then (if it wasn't truncated) apply its outcome. Each command arm in
    /// [`super::state::SessionLoopState::run`] does its own arm-specific
    /// setup, then calls this with the prompt message and a fallback closure.
    ///
    /// Two steps run in order on each round's outcome before it is
    /// applied: truncation recovery ([`Self::handle_truncation_recovery`])
    /// and the turn-continuation check ([`Self::handle_continuation_check`]).
    pub(crate) async fn run_turn(&mut self, prompt: Prompt, fallback: impl FnOnce() -> Message) {
        crate::tools::sandbox::reprobe();
        self.collect_inputs();
        self.activate_environment().await;
        self.collect_inputs();
        if self.has_pending_stop() {
            self.pause_inputs(true);
            self.retain_prompt(prompt);
            self.apply_turn_outcome(TurnCompletion {
                stop: CompletionStop::Cancelled,
                ..Default::default()
            });
            return;
        }
        let (prompt, injected) = self.inject_task_notification(prompt);
        let mut outcome = self
            .run_cancellable_turn(prompt, || match injected {
                Some(text) => deterministic_rig_response(&text),
                None => fallback(),
            })
            .await;
        loop {
            let Some(recovered) = self.handle_truncation_recovery(outcome).await else {
                return;
            };
            let Some(continued) = self.handle_continuation_check(recovered).await else {
                return;
            };
            outcome = continued;
            if outcome.stop.truncation().is_some() {
                continue;
            }
            self.collect_inputs();
            if self.has_pending_stop() && !matches!(outcome.stop, CompletionStop::Cancelled) {
                outcome.stop = CompletionStop::Cancelled;
                self.pause_inputs(true);
            }
            if outcome.is_completing()
                && let Some(text) = self.inputs.take_additions()
            {
                let _ = self
                    .events_tx
                    .send(crate::tools::notification_event(text.clone()).into());
                outcome = self
                    .run_cancellable_turn(
                        Prompt::input(ConversationInputKind::Notification, text.clone()),
                        || deterministic_rig_response(&text),
                    )
                    .await;
                continue;
            }
            if !matches!(outcome.stop, CompletionStop::Finished { .. }) {
                self.settle_outcome(&mut outcome).await;
            }
            self.apply_turn_outcome(outcome);
            return;
        }
    }

    /// Drains this session's finished background `task` children and, if any
    /// were waiting, turns the whole batch into the message this provider round
    /// actually carries (`docs/agent-async-task-design.md` decision 2: "before
    /// each provider round of the requester's turn loop, drain the queue").
    ///
    /// The mechanics matter for history validity. `prompt` is whatever the
    /// round would otherwise have sent -- a user message, or the tool result
    /// that completed a batch. When a notification is waiting, that original
    /// prompt is pushed into `rig_history` here and the notification becomes
    /// the new prompt, so the request reads `assistant(tool_calls) →
    /// tool_result(s) → user(notification) → assistant(reply)`: the tool
    /// results still sit directly behind the calls they answer, and the
    /// notification is an ordinary user-role text turn on top. Reversing the
    /// two would separate a tool call from its result and be rejected.
    ///
    /// Returns the prompt to send plus the notification text, which the caller
    /// needs only to drive the deterministic fallback provider (no network
    /// mode) off the message actually sent.
    pub(crate) fn inject_task_notification(&mut self, prompt: Prompt) -> (Prompt, Option<String>) {
        self.collect_inputs();
        let mut additions = Vec::new();
        let mut failures = Vec::new();
        if let Some(text) = self.inputs.take_additions() {
            additions.push(text);
        }
        if let Some(notification) = crate::tools::take_notification(self.session_id) {
            additions.push(notification.text);
            failures = notification.failures;
        }
        let Some(text) = (!additions.is_empty()).then(|| additions.join("\n\n")) else {
            return (prompt, None);
        };
        if !self.retain_prompt(prompt) {
            return (Prompt::Current, None);
        }
        self.retain_prompt(Prompt::input(
            ConversationInputKind::Notification,
            text.clone(),
        ));
        let _ = self
            .events_tx
            .send(crate::tools::notification_event(text.clone()).into());
        self.report_task_failures(failures);
        (Prompt::Current, Some(text))
    }

    /// Records each child that did not finish as an error item in the
    /// requester's pane. A `task` child is never attached to a pane, so the
    /// notification the model reads is otherwise the only trace of it.
    pub(crate) fn report_task_failures(&self, failures: Vec<String>) {
        for message in failures {
            let _ = self.events_tx.send(Event::Error(Error { message }).into());
        }
    }

    /// Runs a single rig turn to completion while concurrently listening for
    /// `Command::Cancel`, so cancellation is readable mid-turn instead of
    /// sitting behind the turn's blocking network call. Any other command
    /// observed while the turn is in flight is queued in `inbox` and replayed by
    /// the outer loop right after (in arrival order), so e.g. a `Shutdown` sent
    /// mid-turn is never silently swallowed.
    pub(super) async fn run_cancellable_turn(
        &mut self,
        prompt: Prompt,
        fallback: impl FnOnce() -> Message,
    ) -> TurnCompletion {
        let config = self.config.clone();
        self.run_cancellable_turn_with_config(&config, prompt, fallback)
            .await
    }

    async fn run_cancellable_turn_with_config(
        &mut self,
        config: &RigAgentConfig,
        prompt: Prompt,
        fallback: impl FnOnce() -> Message,
    ) -> TurnCompletion {
        let token = CancellationToken::new();
        let turn = complete_rig_turn(
            config,
            &self.environment,
            &self.resolved,
            &mut self.rig_history,
            prompt,
            &self.events_tx,
            &mut self.clearing,
            &self.modules,
            fallback,
            &token,
        );
        tokio::pin!(turn);

        let outcome = loop {
            tokio::select! {
                outcome = &mut turn => break outcome,
                maybe_command = self.commands.recv() => {
                    match maybe_command {
                        Some(command @ (Command::Cancel { .. } | Command::Shutdown)) => {
                            if let Some(event) = self.inputs.set_paused(true) {
                                let _ = self.events_tx.send(event.into());
                            }
                            if matches!(command, Command::Shutdown) {
                                self.inbox.push_front(command);
                            }
                            token.cancel();
                        }
                        Some(other) => self.inbox.push_back(other),
                        None => break turn.await,
                    }
                }
            }
        };
        self.continuation.tool_calls += outcome.requested_tool_call_ids.len();
        if let Some(input) = outcome.input_tokens {
            self.guard
                .record_round(input.saturating_sub(outcome.cached_input_tokens.unwrap_or(0)));
        }
        outcome
    }

    /// Centralizes `Event::TurnEnded` emission for every turn-completion path
    /// that runs a rig turn (`run_cancellable_turn`/`complete_rig_turn`):
    /// completed, cancelled, and failed all funnel through here (the two
    /// guard-halted stop reasons come from the turn-loop guard's own
    /// [`Self::halt_turn_loop`], which never calls this — a halt stops the
    /// loop *instead of* running another turn, so there's no `TurnCompletion`
    /// for it to inspect). Failure is checked before the
    /// empty-tool-calls branch: both a failed request and a text answer can
    /// have no tool calls, but only the latter completes the input.
    pub(crate) fn apply_turn_outcome(&mut self, outcome: TurnCompletion) {
        match outcome.stop {
            CompletionStop::Cancelled => {
                self.emit_cancelled_turn();
            }
            CompletionStop::Failed
            | CompletionStop::Refused
            | CompletionStop::Unknown { .. }
            | CompletionStop::Truncated(_) => {
                self.end_interaction(
                    TurnEndReason::Failed,
                    crate::contract::InputResult::Failure {
                        message: "Provider request failed.".into(),
                    },
                );
            }
            CompletionStop::Finished { text } if outcome.requested_tool_call_ids.is_empty() => {
                self.end_interaction(
                    TurnEndReason::Completed,
                    crate::contract::InputResult::Success { text },
                );
            }

            CompletionStop::Finished { .. } => {
                self.execution.wait_for(outcome.requested_tool_calls);
            }
        }
    }

    /// Handles a turn outcome that may be truncated; either way the turn is
    /// closed as `Failed`. Tool calls the provider started streaming but
    /// never finalized are not run: the harness continues with a prompt
    /// asking the model to re-issue them, up to
    /// [`MAX_CONSECUTIVE_TRUNCATION_CONTINUES`] times in a row before falling
    /// back to `WaitingForUser`. A response stopped at the output-token
    /// limit with no unfinished call is not continued: the session waits
    /// for the user.
    ///
    /// Returns `None` when the truncation was fully handled (the caller should
    /// do nothing further), or `Some(outcome)` when the turn was not truncated
    /// (the caller should pass it to `apply_turn_outcome`).
    pub(crate) async fn handle_truncation_recovery(
        &mut self,
        mut outcome: TurnCompletion,
    ) -> Option<TurnCompletion> {
        while let Some(reason) = outcome.stop.truncation() {
            self.settle_outcome(&mut outcome).await;
            if self
                .inbox
                .iter()
                .any(|command| matches!(command, Command::Shutdown))
            {
                return None;
            }

            if self.has_pending_stop() {
                outcome.stop = CompletionStop::Cancelled;
                return Some(outcome);
            }
            // The event log must tell the truth: this was not a normal
            // completion. The turn is Failed, not Completed. The message
            // distinguishes the two truncation modes (tool calls cut mid-stream
            // vs. output budget exhausted).
            let _ = self.events_tx.send(
                Event::Error(Error {
                    message: truncation_error_message(
                        reason,
                        outcome.output_tokens,
                        self.config.output_cap(),
                    ),
                })
                .into(),
            );
            let _ = self
                .events_tx
                .send(Event::TurnEnded(TurnEndReason::Failed).into());

            let Truncation::Tools { unfinished, .. } = reason else {
                self.finish_input(crate::contract::InputResult::Failure {
                    message: "The provider stopped the response at the output-token limit.".into(),
                });
                let _ = self
                    .events_tx
                    .send(Event::StateChanged(SessionState::WaitingForUser).into());
                return None;
            };
            if !self.guard.record_truncation_continue() {
                self.finish_input(crate::contract::InputResult::Failure {
                    message: "Provider truncation recovery exhausted.".into(),
                });
                // Consecutive truncation cap exhausted: stop auto-continuing
                // and let the user take over.
                let _ = self
                    .events_tx
                    .send(Event::StateChanged(SessionState::WaitingForUser).into());
                return None;
            }

            // Auto-continue: inject the synthetic continuation prompt and run
            // the next turn, mirroring the TaskWake auto-start seam.
            let text = truncation_continuation_prompt(unfinished.get());
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::Running).into());
            let _ = self.events_tx.send(
                Event::MessageCommitted(AgentMessage {
                    role: MessageRole::AutoContinue,
                    text: text.clone(),
                })
                .into(),
            );
            outcome = self
                .run_cancellable_turn(
                    Prompt::input(ConversationInputKind::Continuation, text),
                    || deterministic_rig_response("truncation recovery"),
                )
                .await;
        }
        self.guard.reset_truncation_counter();
        Some(outcome)
    }

    /// Retires every tool call still owned by the unfinished turn.
    ///
    /// This operates on normalized provider call ids, not tool implementations,
    /// so the same path covers synchronous filesystem/config tools, asynchronous
    /// bash/web tools, and approval-gated calls. No pending descriptor remains
    /// for a retired call, so its result cannot advance the loop. The host
    /// also checks async dispatch identity before forwarding a result when a
    /// later provider batch reuses the same call ID.
    pub(crate) async fn cancel_outstanding_tool_calls(&mut self) -> bool {
        let drained: HashMap<ToolCallId, ToolCallDescriptor> = self.execution.cancel_tools();
        if drained.is_empty() {
            return false;
        }
        let call_ids: Vec<ToolCallId> = drained.keys().cloned().collect();

        self.settle_calls(call_ids, drained).await;
        true
    }

    pub(crate) fn emit_cancelled_turn(&mut self) {
        self.end_interaction(
            TurnEndReason::Cancelled,
            crate::contract::InputResult::Interrupted,
        );
    }

    /// Every terminal path settles its input receipt before publishing the
    /// turn boundary.
    fn end_interaction(&mut self, reason: TurnEndReason, result: crate::contract::InputResult) {
        self.finish_input(result);
        let cancelled = reason == TurnEndReason::Cancelled;
        let _ = self.rig_history.apply_event(&Event::TurnEnded(reason));
        let _ = self.events_tx.send(Event::TurnEnded(reason).into());
        if cancelled {
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::Cancelled).into());
        }
        let _ = self
            .events_tx
            .send(Event::StateChanged(SessionState::WaitingForUser).into());
    }

    /// Pauses the turn loop once the rounds have spent the token budget
    /// (`super::super::guards`): a `TurnEnded` with
    /// `HaltedByTokenBudget`, and a return to `WaitingForUser` where a new
    /// user message goes on with a fresh budget.
    ///
    /// `arrived_result` (the batch's last result) is real -- its tool
    /// already ran -- and is retained in `Execution::halt` for Continue;
    /// history already owns it, so adding it again on Continue is
    /// idempotent. The caller must have removed its call id from the
    /// pending batch.
    pub(crate) async fn halt_turn_loop(&mut self, arrived_result: &ToolCallResult, tool_id: &str) {
        self.cancel_outstanding_tool_calls().await;

        self.execution
            .halt(arrived_result.clone(), tool_id.to_string());

        self.guard.reset();
        self.end_interaction(
            crate::contract::TurnEndReason::HaltedByTokenBudget,
            crate::contract::InputResult::Interrupted,
        );
    }
}

// --- Truncation-recovery prompts -------------------------------------------

/// The synthetic continuation prompt injected after the harness detects
/// the provider truncated tool calls mid-stream. The model is told its
/// previous response was cut short and asked to continue the work it
/// described in its reasoning.
fn truncation_continuation_prompt(truncated_count: usize) -> String {
    format!(
        "The provider cut short {truncated_count} tool call(s) in your previous \
         response — the call(s) started streaming but were never finalized. \
         Continue the work you described in your reasoning and re-issue the \
         tool call(s)."
    )
}

/// The error message for a truncated turn, distinguishing the two truncation
/// modes the harness recovers from. Tool-call truncation names how many calls
/// were cut mid-stream; output-cap truncation names the ceiling and the token
/// count that hit it.
fn truncation_error_message(reason: Truncation, output_tokens: Option<u64>, cap: u64) -> String {
    if let Truncation::Tools { unfinished, .. } = reason {
        format!(
            "Provider truncated {count} tool call(s) mid-stream — \
             the call(s) started streaming but were never finalized.",
            count = unfinished.get(),
        )
    } else {
        format!(
            "Provider truncated the response at the {cap}-token output limit — \
             {output_tokens} tokens were generated and the turn did not complete \
             (the output budget was exhausted before a tool call or text reply).",
            output_tokens = output_tokens.unwrap_or(0),
        )
    }
}
