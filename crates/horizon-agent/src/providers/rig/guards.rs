//! The turn loop's guard: a budget on the input tokens a run of provider
//! rounds may spend without a person in between, and the bound on
//! consecutive truncation continues. Pure state, so the counting is tested
//! directly; the halt itself (cancelling outstanding calls, ending the turn)
//! is `session::SessionLoopState::halt_turn_loop`.
//!
//! The budget counts input tokens the provider did not serve from its
//! prompt cache, summed over every round since the last user input: what
//! the rounds cost, rather than how many there were. A halted turn ends
//! with `TurnEndReason::HaltedByTokenBudget` and waits; `Command::
//! ContinueTurn` (or a new user message) starts a fresh budget. A session
//! nobody attends cannot be continued, so the halt is where it stops.

const MAX_CONSECUTIVE_TRUNCATION_CONTINUES: u32 = 3;

#[derive(Debug)]
pub(super) struct TurnLoopGuard {
    token_budget: Option<u64>,
    spent: u64,
    consecutive_truncation_continues: u32,
}

impl TurnLoopGuard {
    /// `None` is no budget.
    pub(super) fn new(token_budget: Option<u64>) -> Self {
        Self {
            token_budget,
            spent: 0,
            consecutive_truncation_continues: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.spent = 0;
        self.consecutive_truncation_continues = 0;
    }

    /// Records one provider round's uncached input tokens.
    pub(super) fn record_round(&mut self, uncached_input_tokens: u64) {
        self.spent = self.spent.saturating_add(uncached_input_tokens);
    }

    /// Whether the rounds since the last reset have spent the budget.
    pub(super) fn exhausted(&self) -> bool {
        self.token_budget.is_some_and(|budget| self.spent >= budget)
    }

    pub(super) fn record_truncation_continue(&mut self) -> bool {
        self.consecutive_truncation_continues += 1;
        self.consecutive_truncation_continues <= MAX_CONSECUTIVE_TRUNCATION_CONTINUES
    }

    /// Resets the truncation counter — called when a turn completes
    /// without truncation, breaking the consecutive-truncation streak.
    pub(super) fn reset_truncation_counter(&mut self) {
        self.consecutive_truncation_continues = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_spent_by_the_sum_of_rounds_and_a_reset_restores_it() {
        let mut guard = TurnLoopGuard::new(Some(100));
        guard.record_round(60);
        assert!(!guard.exhausted());
        guard.record_round(40);
        assert!(guard.exhausted());
        guard.reset();
        assert!(!guard.exhausted());
    }

    #[test]
    fn no_budget_never_exhausts() {
        let mut guard = TurnLoopGuard::new(None);
        guard.record_round(u64::MAX);
        assert!(!guard.exhausted());
    }

    #[test]
    fn truncation_continues_are_bounded_until_reset() {
        let mut guard = TurnLoopGuard::new(None);
        for _ in 0..MAX_CONSECUTIVE_TRUNCATION_CONTINUES {
            assert!(guard.record_truncation_continue());
        }
        assert!(!guard.record_truncation_continue());
        guard.reset_truncation_counter();
        assert!(guard.record_truncation_continue());
    }
}
