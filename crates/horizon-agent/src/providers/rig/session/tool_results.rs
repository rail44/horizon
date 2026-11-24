//! Accept tool results and resume halted results before starting the next turn.

use super::super::conversation::Prompt;
use crate::contract::ToolCallResult;

use super::{SessionLoopState, deterministic_rig_response, deterministic_tool_result_response};

impl SessionLoopState {
    pub(super) async fn handle_tool_result(&mut self, result: ToolCallResult) {
        let Some(descriptor) = self.execution.accept(&result) else {
            return;
        };

        if let Err(message) = self.rig_history.append_result(&result, &descriptor.tool_id) {
            let _ = self
                .events_tx
                .send(crate::contract::Event::Error(crate::contract::Error { message }).into());
            self.inbox.push_front(crate::contract::Command::Shutdown);
            return;
        }

        if self.execution.has_pending_tools() {
            return;
        }

        self.advance_from_tool_result(result, &descriptor.tool_id)
            .await;
    }

    pub(super) async fn continue_halted_turn(&mut self) {
        let Some((result, tool_id)) = self.execution.take_halted() else {
            // Nothing halted to resume: a safe no-op. Covers a
            // stale Continue arriving after a fresh user message
            // already flushed the pending result, a Continue sent
            // to an idle/never-halted session, and — critically —
            // a resumed session right after bootstrap: replay
            // never reconstructs an in-memory halted continuation, so
            // a persisted session that ended halted stays halted
            // (waiting-for-user) rather than auto-resuming.
            return;
        };
        self.pause_inputs(false);
        self.guard.reset();
        // A resumed halt is a new turn on the same request.
        self.continuation.count = 0;
        self.advance_from_tool_result(result, &tool_id).await;
    }

    async fn advance_from_tool_result(&mut self, result: ToolCallResult, tool_id: &str) {
        // The whole batch has landed: before another round, stop if the
        // rounds so far have spent the token budget.
        if self.guard.exhausted() {
            self.halt_turn_loop(&result, tool_id).await;
            return;
        }

        let _ = self.events_tx.send(
            crate::contract::Event::StateChanged(crate::contract::SessionState::Running).into(),
        );
        let (prompt, injected) = self.inject_task_notification(Prompt::result(&result, tool_id));
        self.run_turn(prompt, move || match injected {
            Some(text) => deterministic_rig_response(&text),
            None => deterministic_tool_result_response(&result),
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolCallId;
    use crate::providers::rig::{ToolCallDescriptor, TurnCompletion};
    use std::collections::HashMap;
    #[tokio::test]
    async fn a_new_provider_batch_can_reuse_an_id_from_cancelled_work() {
        let reused = ToolCallId("reused-after-cancel".into());
        let sibling = ToolCallId("still-outstanding".into());
        let descriptor = |call_id: &ToolCallId| ToolCallDescriptor {
            identity: crate::test_support::tool_identity(call_id),
            tool_id: "fs_read".into(),
        };
        let mut state = SessionLoopState {
            execution: HashMap::from([(reused.clone(), descriptor(&reused))]).into(),
            guard: super::super::TurnLoopGuard::new(None),
            ..SessionLoopState::default()
        };
        assert!(state.cancel_outstanding_tool_calls().await);
        state.apply_turn_outcome(TurnCompletion {
            requested_tool_call_ids: vec![reused.clone(), sibling.clone()],
            requested_tool_calls: HashMap::from([
                (reused.clone(), descriptor(&reused)),
                (sibling.clone(), descriptor(&sibling)),
            ]),
            ..Default::default()
        });
        for call in [&reused, &sibling] {
            let descriptor = descriptor(call);
            let request = crate::contract::ToolCallRequest {
                call_id: call.clone(),
                occurrence_id: descriptor.identity.occurrence_id,
                tool_id: descriptor.tool_id,
                input: serde_json::json!({}).into(),
            };
            state
                .rig_history
                .apply_event(&crate::providers::rig::conversation::announcement(
                    &request,
                    "new-batch",
                ))
                .unwrap();
        }
        let before = state.rig_history.len();
        state
            .handle_tool_result(ToolCallResult::new(
                reused.clone(),
                crate::contract::OccurrenceId::new(),
                serde_json::json!({"content": "new result"}),
            ))
            .await;
        assert_eq!(
            state.rig_history.len(),
            before + 1,
            "new result must reach provider history"
        );
        assert!(!state.execution.contains(&reused));
        assert!(state.execution.contains(&sibling));
    }

    /// Once the rounds have spent the budget, the batch's last result
    /// pauses the turn instead of starting another round, and Continue
    /// starts over with a fresh budget.
    #[tokio::test]
    async fn a_spent_budget_pauses_the_turn_at_the_end_of_a_batch() {
        let call = ToolCallId("spent".into());
        let descriptor = ToolCallDescriptor {
            identity: crate::test_support::tool_identity(&call),
            tool_id: "fs_read".into(),
        };
        let (events_tx, events) = crossbeam_channel::unbounded();
        let mut guard = super::super::TurnLoopGuard::new(Some(10));
        guard.record_round(10);
        let mut state = SessionLoopState {
            events_tx,
            execution: HashMap::from([(call.clone(), descriptor.clone())]).into(),
            guard,
            ..SessionLoopState::default()
        };
        let request = crate::contract::ToolCallRequest {
            call_id: call.clone(),
            occurrence_id: descriptor.identity.occurrence_id.clone(),
            tool_id: descriptor.tool_id.clone(),
            input: serde_json::json!({}).into(),
        };
        state
            .rig_history
            .apply_event(&crate::providers::rig::conversation::announcement(
                &request, "batch",
            ))
            .unwrap();
        state
            .handle_tool_result(ToolCallResult::new(
                call.clone(),
                descriptor.identity.occurrence_id,
                serde_json::json!({}),
            ))
            .await;
        assert!(events.try_iter().any(|event| matches!(
            event.as_event(),
            Some(crate::contract::Event::TurnEnded(
                crate::contract::TurnEndReason::HaltedByTokenBudget
            ))
        )));
        assert!(!state.guard.exhausted(), "the halt starts a fresh budget");
    }
}
