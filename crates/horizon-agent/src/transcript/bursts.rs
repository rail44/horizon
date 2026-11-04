//! Tool-activity bursts within a turn (round 5, monotone burst splitting).

/// One tool burst within a turn: a maximal run of tool activity. Indices
/// are relative to the turn's own item slice, `[start, end)`. The shell's
/// structural reading (`src/agent/model/transcript.rs`, `segment_bursts`)
/// produces them.
///
/// Round 5 ("monotone burst splitting" -- superseding round 2's whole-turn
/// provisional-receipt flip-back, see
/// `docs/agent-output-ui-amendment.md`'s post-review note): a turn can
/// fold into *more than one* receipt as it progresses -- tools run,
/// finish, the model answers, then decides to run more tools, answers
/// again, and so on. Each such run is its own burst, and a burst that
/// has closed never reopens into a card again, however much more the turn
/// goes on to do -- eliminating the round-2 mechanism's "flips back to a
/// card" bounce entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Burst {
    pub start: usize,
    pub end: usize,
    /// Whether this burst has permanently folded into a receipt.
    /// `false` only for the trailing burst of a still-running turn whose
    /// tool calls aren't all finished yet, or that has no closing
    /// assistant text after them yet -- the one burst still eligible to
    /// render as the running card. Every other burst -- including the
    /// turn's very last one, once `TurnEnded` folds -- is always `true`.
    pub closed: bool,
}
