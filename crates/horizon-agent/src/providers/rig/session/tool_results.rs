//! Accept tool results and resume halted results before starting the next turn.

use crate::contract::ToolCallResult;

use super::turn::{fold_batched_tool_result, BatchStep};
use super::{
    deterministic_rig_response, deterministic_tool_result_response, rig_tool_result_message,
    tool_result_fingerprint, SessionLoopState,
};

impl SessionLoopState {
    pub(super) async fn handle_tool_result(&mut self, result: ToolCallResult) {
        let Some(descriptor) = self.pending_tool_calls.remove(&result.call_id) else {
            // Unsolicited (duplicate or stale) result: no pending
            // tool call under this id. Running a turn from it would
            // append an orphan tool-result message to rig_history —
            // the next OpenAI request rejects a tool result with no
            // matching assistant tool call — and stray results
            // must not advance the loop guards. Accepted and
            // silently dropped.
            return;
        };

        // Standing-agent memory (`docs/standing-agent-memory-
        // design.md`): a `memory.update` result applies the
        // parsed digest to the session's memory document (the
        // tool handler already validated it and returned a
        // confirmation — this is the state-mutation half) and
        // emits the `MemoryDigest` event for persistence and
        // transcript display. Both `Updated` and `Skipped`
        // (no_update) satisfy the turn-end checkpoint.
        if descriptor.tool_id == crate::tools::MEMORY_UPDATE_TOOL_ID {
            if let Some(memory) = self.memory.as_mut() {
                if let Ok(digest) = crate::tools::parse_update(&descriptor.args) {
                    memory.apply(&digest);
                    let _ = self
                        .events_tx
                        .send(crate::contract::Event::MemoryDigest(digest).into());
                    self.memory_satisfied = true;
                }
            }
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

        if fold_batched_tool_result(
            &mut self.rig_history,
            &self.pending_tool_calls,
            &result,
            &descriptor.tool_id,
        ) == BatchStep::Continue
        {
            return;
        }

        self.advance_from_tool_result(result, &descriptor.tool_id)
            .await;
    }

    pub(super) async fn continue_halted_turn(&mut self) {
        self.pause_inputs(false);
        let Some((result, tool_id)) = self.pending_halt_result.take() else {
            // Nothing halted to resume: a safe no-op. Covers a
            // stale Continue arriving after a fresh user message
            // already flushed the pending result, a Continue sent
            // to an idle/never-halted session, and — critically —
            // a resumed session right after bootstrap: replay
            // never populates `pending_halt_result` on its own, so
            // a persisted session that ended halted stays halted
            // (waiting-for-user) rather than auto-resuming.
            return;
        };
        self.guard.reset();
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
        let (prompt, injected) =
            self.inject_task_notification(rig_tool_result_message(&result, tool_id));
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
        let descriptor = || ToolCallDescriptor {
            tool_id: "fs.read".into(),
            args: serde_json::json!({"path": "file"}),
        };
        let mut state = SessionLoopState {
            pending_tool_calls: HashMap::from([(reused.clone(), descriptor())]),
            guard: super::super::TurnLoopGuard::new(20, 10),
            ..SessionLoopState::default()
        };
        assert!(state.cancel_outstanding_tool_calls());
        state.apply_turn_outcome(TurnCompletion {
            requested_tool_call_ids: vec![reused.clone(), sibling.clone()],
            requested_tool_calls: HashMap::from([
                (reused.clone(), descriptor()),
                (sibling.clone(), descriptor()),
            ]),
            ..Default::default()
        });
        let before = state.rig_history.len();
        state
            .handle_tool_result(ToolCallResult::new(
                reused.clone(),
                Some(crate::contract::OccurrenceId::new()),
                serde_json::json!({"content": "new result"}),
            ))
            .await;
        assert_eq!(
            state.rig_history.len(),
            before + 1,
            "new result must reach provider history"
        );
        assert!(!state.pending_tool_calls.contains_key(&reused));
        assert!(state.pending_tool_calls.contains_key(&sibling));
    }
}
