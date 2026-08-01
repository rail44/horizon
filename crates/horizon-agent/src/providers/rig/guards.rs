//! Pure turn-loop guard state machines: an iteration cap on consecutive
//! tool-driven turns and doom-loop detection on repeated identical tool
//! result fingerprints. Free of I/O so the counting and fingerprinting
//! logic can be tested directly, independent of the session's channels and
//! async plumbing. The guard's halting *response* — cancelling outstanding
//! calls, running a cap-summary turn, emitting events — lives in
//! [`super::session::halt_turn_loop`], which is coupled to the turn
//! execution machinery and stays there.

use std::collections::VecDeque;

use crate::contract::TurnEndReason;

// Two independent safety nets against a runaway tool-calling loop, per
// `docs/agent-tools-design.md`'s "Error Model and Loop Guards" section:
//
// - an iteration cap on consecutive tool-driven turns since the last user
//   message (a model that never stops calling tools), and
// - doom-loop detection on repeated identical (tool, args, result)
//   fingerprints (a model stuck re-issuing the same call to the same
//   effect).
//
// Both halt the same way (`halt_turn_loop`): the same cancellation
// machinery `Command::Cancel` uses for still-pending calls (so
// `rig_history` stays API-valid), a `TurnEnded` event carrying which guard
// fired (rendered as a calm "paused" receipt, not an error -- see
// `docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution),
// and a return to `WaitingForUser` so either a new user message or
// `Command::ContinueTurn` works normally. `TurnLoopGuard` itself is pure
// (no I/O), so its counting and fingerprinting logic is unit-tested
// directly in `tests.rs`.

/// Why the turn loop halted itself rather than running another turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuardHalt {
    IterationCapExceeded,
    DoomLoopDetected,
}

impl GuardHalt {
    /// The specific [`TurnEndReason`] this halt reports, so the UI can
    /// render the right calm reason text without needing to know the
    /// guard's internals — see [`TurnEndReason::HaltedByIterationCap`]/
    /// [`TurnEndReason::HaltedByDoomLoop`]'s own doc comments.
    pub(super) fn turn_end_reason(self) -> TurnEndReason {
        match self {
            GuardHalt::IterationCapExceeded => TurnEndReason::HaltedByIterationCap,
            GuardHalt::DoomLoopDetected => TurnEndReason::HaltedByDoomLoop,
        }
    }
}

/// Pure turn-loop guard state: counts consecutive tool-driven turns since
/// the last user message, and keeps a short window of tool-result
/// fingerprints to detect a doom loop. Free of I/O so it can be tested
/// directly as a small unit, independent of the session's channels and
/// async plumbing.
///
/// `iteration_cap`/`doom_loop_window` come from `agent::config::
/// RigAgentConfig` (formerly the hardcoded `TOOL_TURN_ITERATION_CAP`/
/// `DOOM_LOOP_WINDOW` constants) and are fixed for the guard's lifetime;
/// `reset` only clears the running counters below, never these.
/// Maximum consecutive auto-continues after the provider truncated tool
/// calls mid-stream before giving up and falling back to `WaitingForUser`
/// (the owner's design: three consecutive truncation auto-continues,
/// then stop).
const MAX_CONSECUTIVE_TRUNCATION_CONTINUES: u32 = 3;

#[derive(Debug)]
pub(super) struct TurnLoopGuard {
    iteration_cap: u32,
    doom_loop_window: usize,
    consecutive_tool_turns: u32,
    consecutive_truncation_continues: u32,
    recent_fingerprints: VecDeque<u64>,
}

impl TurnLoopGuard {
    pub(super) fn new(iteration_cap: u32, doom_loop_window: usize) -> Self {
        Self {
            iteration_cap,
            doom_loop_window,
            consecutive_tool_turns: 0,
            consecutive_truncation_continues: 0,
            recent_fingerprints: VecDeque::new(),
        }
    }

    /// Resets both the iteration count and the fingerprint window. Called
    /// when a `Command::UserMessage` starts a fresh interaction.
    pub(super) fn reset(&mut self) {
        self.consecutive_tool_turns = 0;
        self.consecutive_truncation_continues = 0;
        self.recent_fingerprints.clear();
    }

    /// Records that a tool-driven turn is about to run. Returns
    /// `Some(GuardHalt::IterationCapExceeded)` once the cap is exceeded
    /// (i.e. on the `iteration_cap + 1`-th consecutive call).
    pub(super) fn record_tool_turn(&mut self) -> Option<GuardHalt> {
        self.consecutive_tool_turns += 1;
        (self.consecutive_tool_turns > self.iteration_cap)
            .then_some(GuardHalt::IterationCapExceeded)
    }

    /// Records an incoming tool result's fingerprint. Returns
    /// `Some(GuardHalt::DoomLoopDetected)` once the last `doom_loop_window`
    /// fingerprints are all identical.
    pub(super) fn record_fingerprint(&mut self, fingerprint: u64) -> Option<GuardHalt> {
        self.recent_fingerprints.push_back(fingerprint);
        if self.recent_fingerprints.len() > self.doom_loop_window {
            self.recent_fingerprints.pop_front();
        }
        let is_doom_loop = self.recent_fingerprints.len() == self.doom_loop_window
            && self.recent_fingerprints.iter().all(|fp| *fp == fingerprint);
        is_doom_loop.then_some(GuardHalt::DoomLoopDetected)
    }

    /// Records that a truncation-triggered auto-continue is about to run.
    /// Returns `true` while under the cap (the continue may proceed),
    /// `false` once the cap is exceeded (the session must fall back to
    /// `WaitingForUser`).
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

/// Fingerprints a tool result as (tool, args, output) — the triple the
/// design doc specifies. Args are included so distinct, productive calls
/// that happen to return identical output (e.g. greps for different
/// patterns, each with zero matches) are not mistaken for a doom loop.
/// Call ids are deliberately excluded: each call gets a fresh id even when
/// the model repeats the same call verbatim.
pub(super) fn tool_result_fingerprint(
    tool_id: &str,
    args: &serde_json::Value,
    output: &serde_json::Value,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_id.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    output.to_string().hash(&mut hasher);
    hasher.finish()
}
