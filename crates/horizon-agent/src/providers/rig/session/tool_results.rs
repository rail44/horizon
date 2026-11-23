//! Accept tool results and resume halted results before starting the next turn.

use super::super::conversation::Prompt;
use crate::contract::ToolCallResult;

use super::{
    SessionLoopState, deterministic_rig_response, deterministic_tool_result_response,
    tool_result_fingerprint,
};

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

        // Doom-loop fingerprinting is per *result* (every call's
        // outcome must be checked, not just the batch's last), so
        // it runs unconditionally here — before deciding whether
        // this is the last outstanding result of the current
        // batch.
        let fingerprint =
            tool_result_fingerprint(&descriptor.tool_id, &descriptor.args, &result.output);
        if let Some(halt) = self.guard.record_fingerprint(fingerprint) {
            // Stop instead of running another turn. The arrived
            // result is real — its tool already executed — so it
            // is recorded as-is; only *other* still-pending calls
            // get the cancelled treatment.
            self.halt_turn_loop(halt, &result, &descriptor.tool_id)
                .await;
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
        // The whole batch has landed: this is the one tool-driven
        // turn the batch counts as, so the iteration-cap guard is
        // recorded exactly once here — never per result, or an
        // N-call batch would burn the cap N times faster. A resumed halt also
        // counts once after its guard reset, so Continue can reach the cap again.
        if let Some(halt) = self.guard.record_tool_turn() {
            self.halt_turn_loop(halt, &result, tool_id).await;
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
            args: serde_json::json!({"path": "file"}),
        };
        let mut state = SessionLoopState {
            execution: HashMap::from([(reused.clone(), descriptor(&reused))]).into(),
            guard: super::super::TurnLoopGuard::new(Some(20), 10),
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
                input: descriptor.args.into(),
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
}
