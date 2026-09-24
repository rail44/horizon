//! Work retained between provider rounds. An in-flight provider future is owned
//! by the session loop; it cannot run concurrently with these transitions.
use std::collections::HashMap;

use super::ToolCallDescriptor;
use crate::contract::{ToolCallId, ToolCallIdentity, ToolCallResult};

#[derive(Default)]
pub(crate) struct Execution {
    phase: Phase,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Tools(HashMap<ToolCallId, ToolCallDescriptor>),
    Halted {
        result: ToolCallResult,
        tool_id: String,
    },
}

impl From<HashMap<ToolCallId, ToolCallDescriptor>> for Execution {
    fn from(calls: HashMap<ToolCallId, ToolCallDescriptor>) -> Self {
        Self {
            phase: if calls.is_empty() {
                Phase::Idle
            } else {
                Phase::Tools(calls)
            },
        }
    }
}

impl Execution {
    pub(crate) fn has_pending_tools(&self) -> bool {
        matches!(self.phase, Phase::Tools(_))
    }

    pub(crate) fn wait_for(&mut self, calls: HashMap<ToolCallId, ToolCallDescriptor>) {
        debug_assert!(matches!(self.phase, Phase::Idle));
        *self = calls.into();
    }

    pub(crate) fn reissue(&mut self, identity: ToolCallIdentity) {
        if let Phase::Tools(calls) = &mut self.phase {
            if let Some(call) = calls.get_mut(&identity.call_id) {
                call.identity = identity;
            }
        }
    }

    /// The daemon checks attempt identity. A declined retry answers the
    /// provider with the prior attempt, so this boundary is keyed by call ID.
    pub(crate) fn accept(&mut self, result: &ToolCallResult) -> Option<ToolCallDescriptor> {
        if result.is_superseded() {
            return None;
        }
        let Phase::Tools(calls) = &mut self.phase else {
            return None;
        };
        let call = calls.remove(&result.call_id)?;
        if calls.is_empty() {
            self.phase = Phase::Idle;
        }
        Some(call)
    }

    pub(crate) fn cancel_tools(&mut self) -> HashMap<ToolCallId, ToolCallDescriptor> {
        if !self.has_pending_tools() {
            return HashMap::new();
        }
        let Phase::Tools(calls) = std::mem::take(&mut self.phase) else {
            unreachable!()
        };
        calls
    }

    pub(crate) fn halt(&mut self, result: ToolCallResult, tool_id: String) {
        debug_assert!(!self.has_pending_tools());
        self.phase = Phase::Halted { result, tool_id };
    }

    /// Both Continue and fresh input consume the retained result exactly once.
    /// Replay starts Idle: provider history already contains persisted results.
    pub(crate) fn take_halted(&mut self) -> Option<(ToolCallResult, String)> {
        if !matches!(self.phase, Phase::Halted { .. }) {
            return None;
        }
        let Phase::Halted { result, tool_id } = std::mem::take(&mut self.phase) else {
            unreachable!()
        };
        Some((result, tool_id))
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, call_id: &ToolCallId) -> bool {
        matches!(&self.phase, Phase::Tools(calls) if calls.contains_key(call_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn result_and_cancellation_orders_settle_each_call_once() {
        // Each cut cancels a batch after a different number of completions.
        // Repeated and late results cannot re-open the next provider round.
        for cancel_after in 0..=3 {
            let calls: HashMap<_, _> = (0..3)
                .map(|index| {
                    let call_id = ToolCallId(format!("call-{index}"));
                    (
                        call_id.clone(),
                        ToolCallDescriptor {
                            identity: crate::test_support::tool_identity(&call_id),
                            tool_id: "fs.read".into(),
                            args: json!({}),
                        },
                    )
                })
                .collect();
            let mut results: Vec<_> = calls
                .values()
                .map(|call| call.identity.result(json!({"content": "value"})))
                .collect();
            results.sort_by(|left, right| left.call_id.0.cmp(&right.call_id.0));
            let mut state = Execution::from(calls);
            for result in &results[..cancel_after] {
                assert!(state.accept(result).is_some());
                assert!(state.accept(result).is_none());
            }
            assert_eq!(state.cancel_tools().len(), 3 - cancel_after);
            for result in &results {
                assert!(state.accept(result).is_none());
            }
            assert!(!state.has_pending_tools());
        }
    }

    #[test]
    fn halted_results_are_consumed_once_and_cannot_coexist_with_a_pending_batch() {
        let identity = crate::test_support::tool_identity(&ToolCallId("halted".into()));
        let result = identity.result(json!({"content": "already executed"}));
        let mut state = Execution::default();
        state.halt(result.clone(), "fs.read".into());
        assert!(state.cancel_tools().is_empty());
        assert!(state.accept(&result).is_none());
        assert_eq!(state.take_halted(), Some((result, "fs.read".into())));
        assert!(state.take_halted().is_none());
    }
}
