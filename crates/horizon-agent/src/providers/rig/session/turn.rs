//! The turn-execution pipeline: `run_cancellable_turn` →
//! `handle_truncation_recovery` → `apply_turn_outcome`, plus the guard-halt
//! response (`halt_turn_loop`) and the small helpers they share. All are
//! methods on [`super::state::SessionLoopState`], taking `&mut self`
//! instead of the 10+ individual arguments the free-function form
//! accumulated. The pure helpers that touch no loop state
//! (`fold_batched_tool_result`, `append_cancelled_tool_results_to_history`,
//! `BatchStep`) stay as free `pub(super)` functions so the tests can keep
//! calling them without a `SessionLoopState`.

use std::collections::HashMap;

use rig_core::completion::Message;
use tokio_util::sync::CancellationToken;

use crate::{
    contract::{
        Command, Error, Event, Message as AgentMessage, MessageRole, SessionState, ToolCallId,
        ToolCallResult, TurnEndReason,
    },
    tools::cancelled_tool_call_result,
};

use super::state::SessionLoopState;
use super::{
    complete_rig_turn, deterministic_rig_response, rig_tool_result_message, GuardHalt,
    ToolCallDescriptor, TurnCompletion,
};

impl SessionLoopState {
    /// The full turn-execution pipeline as one call: run a cancellable turn,
    /// then (if it wasn't truncated) apply its outcome. Each command arm in
    /// [`super::state::SessionLoopState::run`] does its own arm-specific
    /// setup, then calls this with the prompt message and a fallback closure.
    pub(crate) async fn run_turn(&mut self, prompt: Message, fallback: impl FnOnce() -> Message) {
        let outcome = self.run_cancellable_turn(prompt, fallback).await;
        if let Some(outcome) = self.handle_truncation_recovery(outcome).await {
            self.apply_turn_outcome(outcome);
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
    pub(crate) fn inject_task_notification(
        &mut self,
        prompt: Message,
    ) -> (Message, Option<String>) {
        let Some(text) = crate::tools::take_notification(self.session_id) else {
            return (prompt, None);
        };
        self.rig_history.push(prompt);
        let _ = self
            .events_tx
            .send(crate::tools::notification_event(text.clone()).into());
        (Message::user(text.clone()), Some(text))
    }

    /// Runs a single rig turn to completion while concurrently listening for
    /// `Command::Cancel`, so cancellation is readable mid-turn instead of
    /// sitting behind the turn's blocking network call. Any other command
    /// observed while the turn is in flight is queued in `inbox` and replayed by
    /// the outer loop right after (in arrival order), so e.g. a `Shutdown` sent
    /// mid-turn is never silently swallowed.
    async fn run_cancellable_turn(
        &mut self,
        prompt: Message,
        fallback: impl FnOnce() -> Message,
    ) -> TurnCompletion {
        let token = CancellationToken::new();
        let turn = complete_rig_turn(
            &self.config,
            &self.environment,
            &self.extra_sections,
            &mut self.rig_history,
            prompt,
            &self.events_tx,
            &mut self.clearing,
            fallback,
            &token,
        );
        tokio::pin!(turn);

        loop {
            tokio::select! {
                outcome = &mut turn => return outcome,
                maybe_command = self.commands.recv() => {
                    match maybe_command {
                        Some(Command::Cancel { .. }) => token.cancel(),
                        Some(other) => self.inbox.push_back(other),
                        None => return turn.await,
                    }
                }
            }
        }
    }

    /// Centralizes `Event::TurnEnded` emission for every turn-completion path
    /// that runs a rig turn (`run_cancellable_turn`/`complete_rig_turn`):
    /// completed, cancelled, and failed all funnel through here (the two
    /// guard-halted stop reasons come from the turn-loop guard's own
    /// [`Self::halt_turn_loop`], which never calls this — a halt stops the
    /// loop *instead of* running another turn, so there's no `TurnCompletion`
    /// for it to inspect). `outcome.failed` is checked before the
    /// empty-tool-calls branch since a failed provider request also requests
    /// no tool calls — without the explicit flag the two would be
    /// indistinguishable.
    pub(crate) fn apply_turn_outcome(&mut self, outcome: TurnCompletion) {
        if outcome.cancelled {
            self.cancelled_call_ids
                .extend(outcome.requested_tool_call_ids.iter().cloned());
            append_cancelled_tool_results_to_history(
                &mut self.rig_history,
                &outcome.requested_tool_call_ids,
            );
            for call_id in outcome.requested_tool_call_ids {
                let _ = self
                    .events_tx
                    .send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
            }
            let _ = self
                .events_tx
                .send(Event::TurnEnded(TurnEndReason::Cancelled).into());
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::Cancelled).into());
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::WaitingForUser).into());
            return;
        }

        if outcome.failed {
            let _ = self
                .events_tx
                .send(Event::TurnEnded(TurnEndReason::Failed).into());
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::WaitingForUser).into());
            return;
        }

        if outcome.requested_tool_call_ids.is_empty() {
            let _ = self
                .events_tx
                .send(Event::TurnEnded(TurnEndReason::Completed).into());
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::WaitingForUser).into());
        } else {
            self.pending_tool_calls.extend(outcome.requested_tool_calls);
        }
    }

    /// Handles a turn outcome that may be truncated: if the provider started
    /// streaming tool calls but never finalized them (`outcome.truncated`), or
    /// if the turn's output hit the configured token ceiling
    /// (`outcome.cap_truncated`), the turn is closed as `Failed` and the
    /// harness automatically continues with a synthetic prompt, up to
    /// [`MAX_CONSECUTIVE_TRUNCATION_CONTINUES`] times before falling back to
    /// `WaitingForUser`. Both truncation modes share the same
    /// consecutive-continue cap (one counter, not two) per the issue's
    /// instruction to reuse the existing guard rather than mint a new one.
    ///
    /// Returns `None` when the truncation was fully handled (the caller should
    /// do nothing further), or `Some(outcome)` when the turn was not truncated
    /// (the caller should pass it to `apply_turn_outcome`).
    pub(crate) async fn handle_truncation_recovery(
        &mut self,
        mut outcome: TurnCompletion,
    ) -> Option<TurnCompletion> {
        if !outcome.truncated && !outcome.cap_truncated {
            self.guard.reset_truncation_counter();
            return Some(outcome);
        }

        loop {
            // Cancel any finalized tool calls from the truncated turn — the
            // turn is ending as Failed, so they must not hang as pending.
            if !outcome.requested_tool_call_ids.is_empty() {
                self.cancelled_call_ids
                    .extend(outcome.requested_tool_call_ids.iter().cloned());
                append_cancelled_tool_results_to_history(
                    &mut self.rig_history,
                    &outcome.requested_tool_call_ids,
                );
                for call_id in &outcome.requested_tool_call_ids {
                    let _ = self.events_tx.send(
                        Event::ToolCallFinished(cancelled_tool_call_result(call_id.clone())).into(),
                    );
                }
            }

            // The event log must tell the truth: this was not a normal
            // completion. The turn is Failed, not Completed. The message
            // distinguishes the two truncation modes (tool calls cut mid-stream
            // vs. output budget exhausted).
            let _ = self.events_tx.send(
                Event::Error(Error {
                    message: truncation_error_message(&outcome, self.config.max_output_tokens),
                })
                .into(),
            );
            let _ = self
                .events_tx
                .send(Event::TurnEnded(TurnEndReason::Failed).into());

            if !self.guard.record_truncation_continue() {
                // Consecutive truncation cap exhausted: stop auto-continuing
                // and let the user take over.
                let _ = self
                    .events_tx
                    .send(Event::StateChanged(SessionState::WaitingForUser).into());
                return None;
            }

            // Auto-continue: inject the synthetic continuation prompt and run
            // the next turn, mirroring the TaskWake auto-start seam.
            let text = truncation_continuation_prompt_for(&outcome, self.config.max_output_tokens);
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
                .run_cancellable_turn(Message::user(text), || {
                    deterministic_rig_response("truncation recovery")
                })
                .await;

            if !outcome.truncated && !outcome.cap_truncated {
                self.guard.reset_truncation_counter();
                self.apply_turn_outcome(outcome);
                return None;
            }
        }
    }

    /// Retires every tool call still owned by the unfinished turn.
    ///
    /// This operates on normalized provider call ids, not tool implementations,
    /// so the same path covers synchronous filesystem/config tools, asynchronous
    /// bash/web tools, and approval-gated calls. Real results that arrive after
    /// retirement are recognized through `cancelled_call_ids` and dropped by the
    /// session loop instead of entering a later turn's batch.
    pub(crate) fn cancel_outstanding_tool_calls(&mut self) -> bool {
        let call_ids: Vec<ToolCallId> = self.pending_tool_calls.drain().map(|(id, _)| id).collect();
        if call_ids.is_empty() {
            return false;
        }

        self.cancelled_call_ids.extend(call_ids.iter().cloned());
        append_cancelled_tool_results_to_history(&mut self.rig_history, &call_ids);
        for call_id in call_ids {
            let _ = self
                .events_tx
                .send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
        }
        true
    }

    pub(crate) fn emit_cancelled_turn(&self) {
        let _ = self
            .events_tx
            .send(Event::TurnEnded(TurnEndReason::Cancelled).into());
        let _ = self
            .events_tx
            .send(Event::StateChanged(SessionState::Cancelled).into());
        let _ = self
            .events_tx
            .send(Event::StateChanged(SessionState::WaitingForUser).into());
    }

    /// Halts the turn loop in response to a tripped guard.
    ///
    /// `docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution
    /// (decision 2): a guard halt now reads as a pause, not an error, so this
    /// no longer emits `Event::Error` at all — only `Event::TurnEnded` with
    /// the specific guard-kind reason (folded by `frame::apply_agent_event_to_frame`
    /// into the turn's receipt, rendered calmly by `src/agent/turns/receipt.rs`
    /// rather than as a danger-styled error block).
    ///
    /// The result that tripped the guard (`arrived_result`) is *real*: its
    /// tool already executed (an `fs.write` is already on disk) and the app
    /// already surfaced its genuine `ToolCallFinished`. Any *other*
    /// still-pending calls in the batch (only possible on the doom-loop path —
    /// see the module doc) are cancelled immediately with the same helpers
    /// `Command::Cancel` uses, since those never get a second chance to
    /// land.
    ///
    /// For an iteration-cap halt on a role that opts in
    /// (`RoleDefinition::summarize_on_cap`, e.g. `EXPLORE_ROLE` -- see
    /// `docs/agent-explore-design.md`'s 2026-07-27 addendum and
    /// `docs/research/agent-context-reduction-prior-art-2026-07-26.md` §4's
    /// OpenCode/Hermes precedent), [`Self::run_cap_summary_turn`] first folds
    /// `arrived_result` into `rig_history` and runs one forced, tools-disabled
    /// completion asking the model to summarize instead of stopping cold. If
    /// that succeeds, the turn ends right there with the summary already
    /// committed as the session's final message for this turn. Every other
    /// case (doom loop, a role that doesn't opt in, or the wrap-up completion
    /// itself failing) falls back to the original behavior: `arrived_result`
    /// is deliberately *not* folded into `rig_history` here —
    /// `pending_halt_result` stashes it instead, the same way an ordinary
    /// tool-driven turn treats a batch's last-landed result: as the *next*
    /// turn's prompt (see [`fold_batched_tool_result`]'s doc comment), not a
    /// pre-pushed history entry. `Command::ContinueTurn` consumes it to
    /// resume; `Command::UserMessage` flushes it into history first if the
    /// user types past the halt instead.
    ///
    /// Resets the guard and returns the session to `WaitingForUser` either way
    /// (Continue re-enters the loop with a fresh guard, exactly like a new
    /// `Command::UserMessage` would).
    ///
    /// The caller must have already removed `arrived_result`'s call id from
    /// `pending_tool_calls` (the session loop does this when it looks up the
    /// call's descriptor).
    pub(crate) async fn halt_turn_loop(
        &mut self,
        halt: GuardHalt,
        arrived_result: &ToolCallResult,
    ) {
        self.cancel_outstanding_tool_calls();

        let summarized = halt == GuardHalt::IterationCapExceeded
            && self.role.is_some_and(|role| role.summarize_on_cap)
            && self.run_cap_summary_turn(arrived_result).await;

        if !summarized {
            self.pending_halt_result = Some(arrived_result.clone());
        }

        self.guard.reset();
        let _ = self
            .events_tx
            .send(Event::TurnEnded(halt.turn_end_reason()).into());
        let _ = self
            .events_tx
            .send(Event::StateChanged(SessionState::WaitingForUser).into());
    }

    /// Runs one forced, tools-disabled completion for an iteration-cap halt on
    /// a role that opts into it (`RoleDefinition::summarize_on_cap`) — see
    /// [`Self::halt_turn_loop`]'s doc comment for the surrounding decision.
    ///
    /// Folds `arrived_result` -- the real, already-executed result that tripped
    /// the guard -- into `rig_history` first (exactly where an ordinary
    /// tool-driven turn would put it), then runs one more completion with
    /// every tool definition withheld (`RigAgentConfig::allowed_tool_ids`
    /// overridden to an empty list, which `rig_tool_definitions` turns into
    /// "advertise nothing"), so the model cannot keep exploring even if it
    /// tries.
    ///
    /// Returns whether the wrap-up produced a completion at all. `false`
    /// (provider failure, or the turn getting cancelled mid-wrap-up) truncates
    /// `rig_history` back to exactly what the caller passed in -- no half step
    /// -- so [`Self::halt_turn_loop`]'s ordinary fallback (stash
    /// `arrived_result` for `Continue`/a new `UserMessage`) stays correct
    /// rather than double-folding it. Truncating to a recorded length, rather
    /// than popping a fixed count, is deliberate: `complete_rig_turn` pushes a
    /// different number of messages depending on how far the wrap-up got
    /// (zero on a provider-request error, two -- the prompt and a partial
    /// assistant message -- on cancellation).
    async fn run_cap_summary_turn(&mut self, arrived_result: &ToolCallResult) -> bool {
        let baseline_len = self.rig_history.len();
        self.rig_history
            .push(rig_tool_result_message(arrived_result));

        let mut wrap_up_config = self.config.clone();
        wrap_up_config.allowed_tool_ids = Some(Vec::new());

        let _ = self
            .events_tx
            .send(Event::StateChanged(SessionState::Running).into());
        let outcome = self
            .run_cancellable_turn(Message::user(CAP_SUMMARY_INSTRUCTION), || {
                deterministic_rig_response(CAP_SUMMARY_INSTRUCTION)
            })
            .await;

        if outcome.failed || outcome.cancelled {
            self.rig_history.truncate(baseline_len);
            return false;
        }
        true
    }
}

/// What the `Command::ToolCallResult` arm should do next for a landed batch
/// member, once [`fold_batched_tool_result`] has decided whether the rest of
/// the batch is still outstanding.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum BatchStep {
    /// More of the batch is still outstanding. The result has already been
    /// folded into `rig_history`, in arrival order — the caller just keeps
    /// consuming commands, without emitting `Running` or running a turn.
    Continue,
    /// The whole batch has landed (this was its last outstanding call), so
    /// a follow-up completion should run. The result is deliberately *not*
    /// yet in `rig_history` — the caller runs the turn with it as the
    /// prompt message, which appends it right before the resulting
    /// assistant message (`run_cancellable_turn`/`complete_rig_turn`),
    /// keeping a single unbroken "tool_calls, then all N results, then the
    /// assistant's reply" run in history.
    RunTurn,
}

/// Decides what a landed `Command::ToolCallResult` should do, per the
/// "batching" fix in `run_session_loop`'s `Command::ToolCallResult` arm: a
/// single completion can request several parallel tool calls (e.g. MiniMax
/// routinely requesting 4 parallel `fs.read`s), each of which arrives as its
/// own `Command::ToolCallResult`. Running a follow-up completion per result
/// would send the model a protocol-malformed history (an assistant
/// `tool_calls` message missing most of its results) for every
/// still-outstanding call, and burn the iteration-cap guard once per result
/// instead of once per batch.
///
/// The caller must have already removed `result`'s call id from
/// `pending_tool_calls` (to look up its descriptor for the doom-loop
/// fingerprint) before calling this — so an empty `pending_tool_calls` here
/// means `result` was the batch's last outstanding call.
pub(crate) fn fold_batched_tool_result(
    rig_history: &mut Vec<Message>,
    pending_tool_calls: &HashMap<ToolCallId, ToolCallDescriptor>,
    result: &ToolCallResult,
) -> BatchStep {
    if pending_tool_calls.is_empty() {
        BatchStep::RunTurn
    } else {
        rig_history.push(rig_tool_result_message(result));
        BatchStep::Continue
    }
}

/// Appends one cancelled tool-result message per cancelled call id, directly
/// after the assistant message that carried the tool calls. This keeps the
/// rig history self-consistent for the API: an assistant `tool_calls`
/// message not followed by a result message per call is rejected by OpenAI
/// on the next request. Mirrors the cancelled `ToolCallFinished` events
/// synthesized for the UI and persistence.
pub(crate) fn append_cancelled_tool_results_to_history(
    rig_history: &mut Vec<Message>,
    cancelled_call_ids: &[ToolCallId],
) {
    for call_id in cancelled_call_ids {
        rig_history.push(rig_tool_result_message(&cancelled_tool_call_result(
            call_id.clone(),
        )));
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
fn truncation_error_message(outcome: &TurnCompletion, cap: u64) -> String {
    if outcome.truncated {
        format!(
            "Provider truncated {count} tool call(s) mid-stream — \
             the call(s) started streaming but were never finalized.",
            count = outcome.truncated_tool_call_count,
        )
    } else {
        format!(
            "Provider truncated the response at the {cap}-token output limit — \
             {output_tokens} tokens were generated and the turn did not complete \
             (the output budget was exhausted before a tool call or text reply).",
            output_tokens = outcome.output_tokens.unwrap_or(0),
        )
    }
}

/// The synthetic continuation prompt for a truncated turn, tailored to the
/// truncation mode. Tool-call truncation asks the model to re-issue the cut
/// calls; output-cap truncation asks it to be more concise so the next turn
/// does not exhaust the budget the same way.
fn truncation_continuation_prompt_for(outcome: &TurnCompletion, cap: u64) -> String {
    if outcome.truncated {
        truncation_continuation_prompt(outcome.truncated_tool_call_count)
    } else {
        format!(
            "The provider cut your previous response short at the output-token \
             limit ({cap} tokens generated) — you used the entire output budget \
             without producing a tool call or text reply. Continue the work you \
             described in your reasoning, but be more concise: go straight to the \
             action instead of re-deriving it."
        )
    }
}

/// The instruction injected as a synthetic user message ahead of a forced
/// cap wrap-up completion — the Hermes/OpenCode shape
/// (`docs/research/agent-context-reduction-prior-art-2026-07-26.md` §4):
/// stop calling tools and report what was found instead of hard-erroring
/// with the work discarded.
const CAP_SUMMARY_INSTRUCTION: &str = "You have reached the turn limit for this task. Stop here \
     and summarize, without calling any more tools: the relevant files you found (with paths and \
     line numbers), your best partial answer to the question you were asked, and what remains \
     unknown.";
