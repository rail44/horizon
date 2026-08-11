//! Tier 1 compaction: reversible, mechanical clearing of old tool-result
//! bodies (`docs/agent-compaction-design.md`).
//!
//! This module reinstates the provider-view seam that was deleted on
//! 2026-07-25 (`docs/research/agent-context-memory-separation-2026-07-20.md`,
//! the reversal note). That removal's stated precondition -- fix the amount
//! of context ordinary work produces before introducing a lossy history
//! transformation -- has since been discharged by the 2026-07-26..27
//! consumption campaign, and the removal note itself recorded that bringing
//! the seam back is "one function call at one call site". This is that
//! function ([`history_for_provider_request`]) and that call site
//! (`completion::complete_rig_turn`).
//!
//! **Projection, not rewrite.** `rig_history`, the event log, and the DuckDB
//! projection are never touched. Only the `Vec<Message>` handed to one
//! provider request is rewritten, and only the *content* of tool-result
//! messages: the assistant's tool call and its `call_id` stay exactly where
//! they are, because splitting a call from its result is a known source of
//! provider 400s. That the originals really do survive is what makes the
//! placeholder's "re-fetch it" pointer honest rather than a polite fiction --
//! `recall.search`/`recall.read` read the same event log.
//!
//! **The cleared set is frozen, and that is load-bearing.** A pass decides
//! its set exactly once, from the history as it stands at that moment, and
//! records it both in session state and as `Event::HistoryCleared`. Every
//! later request applies that identical set, so the projected prefix is
//! byte-stable and the provider's prompt cache is invalidated once per pass
//! rather than on every round. **Recomputing the set per request is a
//! rejected design**: a moving window would re-clear a different suffix each
//! round, churning the cache continuously and making a resumed session
//! disagree with the one it resumed -- the exact opposite of what a
//! compaction layer is for.
//!
//! **A frozen `call_id` only ever blanks the results that existed when it
//! froze.** Providers reuse `call_id`s across turns (measured on Kimi, whose
//! ids are `functions.<name>:<index>`), and Horizon reuses one deliberately
//! on every denial retry -- so a set keyed on `call_id` alone would blank a
//! *fresh* result the moment it arrived, in the very turn the model needs
//! it, with a placeholder pointing at an entirely different call's output.
//! [`ClearedResults`] therefore records, per `call_id`, *how many* of that
//! id's tool-result occurrences the passes cleared, counted from the oldest;
//! the projection replaces only that many. A result appended after the
//! freeze is a later occurrence and is always kept verbatim. The count is
//! carried on the wire for free -- `HistoryCleared::cleared_call_ids` is a
//! `Vec`, and a pass already pushes one entry per cleared *occurrence* -- so
//! resume replay rebuilds identical counts by tallying the flattened list,
//! with no new field and no `SESSION_PROTOCOL_VERSION` bump.

use std::collections::{HashMap, HashSet};

use rig_core::completion::{
    message::{AssistantContent, ToolResult, ToolResultContent, UserContent},
    Message,
};
use rig_core::OneOrMany;

use crate::config::{
    CLEARING_CHARS_PER_TOKEN, CLEARING_RECOVERY_FLOOR_TOKENS, CLEARING_TAIL_BUDGET_TOKENS,
};
use crate::contract::{Event, HistoryCleared, ToolCallId};

/// How much of a tool call's key argument the placeholder reproduces before
/// truncating. Long enough to identify the call (a repository-relative path,
/// a short command), short enough that a cleared result never grows back
/// into a context cost.
const PLACEHOLDER_ARGUMENT_CHARS: usize = 120;

/// Argument keys the placeholder looks for, in order -- "the key argument
/// (path/command/pattern)" from the design doc, generalized just far enough
/// to cover the current tool catalog. The first key a call's arguments
/// actually carry wins; a call with none of them is described by tool id
/// alone.
const PLACEHOLDER_ARGUMENT_KEYS: [&str; 6] =
    ["path", "command", "pattern", "query", "url", "session_id"];

/// Per-session Tier 1 state: the discovered window, the observed input size,
/// and the frozen cleared set.
#[derive(Clone, Debug)]
pub(super) struct ClearingState {
    /// `context_length − max_output_tokens`, or `None` when the provider
    /// declared no limits. `None` means clearing **never fires** -- crush's
    /// `cw == 0` protection; there is no fallback window (see
    /// `super::model_limits`' module doc).
    effective_window_tokens: Option<u64>,
    threshold_pct: u32,
    /// The provider's own most recently reported input token count
    /// (`Event::ProviderRequestUsage`). Measured, never estimated: the
    /// decision to compact is too consequential to make from a byte
    /// heuristic when the provider states the real number.
    latest_input_tokens: u64,
    cleared: ClearedResults,
}

/// The frozen cleared set, plus the guard that keeps a reused `call_id`
/// honest (see the module doc): for each cleared `call_id`, how many of that
/// id's tool-result occurrences -- counted from the oldest in history order
/// -- the passes so far cleared.
///
/// One pass pushes one entry per occurrence it clears, so tallying
/// `HistoryCleared::cleared_call_ids` across a session's events reproduces
/// this exactly; that is what [`ClearingState::seed_cleared`] does on
/// resume.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClearedResults {
    occurrences: HashMap<ToolCallId, usize>,
}

impl ClearedResults {
    /// Tallies one entry per cleared occurrence -- the shape both a live
    /// pass's plan and a replayed `HistoryCleared` come in. Production
    /// builds these through [`ClearingState`] (`run_pass`/`seed_cleared`);
    /// this is the direct constructor the unit tests use.
    #[cfg(test)]
    pub(super) fn from_occurrences(call_ids: impl IntoIterator<Item = ToolCallId>) -> Self {
        let mut cleared = Self::default();
        cleared.record_occurrences(call_ids);
        cleared
    }

    fn record_occurrences(&mut self, call_ids: impl IntoIterator<Item = ToolCallId>) {
        for call_id in call_ids {
            *self.occurrences.entry(call_id).or_insert(0) += 1;
        }
    }

    /// Whether any occurrence of `call_id` is already frozen -- the
    /// planning walk's "don't re-report what a previous pass cleared"
    /// check.
    pub(super) fn contains(&self, call_id: &ToolCallId) -> bool {
        self.occurrences.contains_key(call_id)
    }

    /// How many of `call_id`'s occurrences, from the oldest, are cleared.
    fn cleared_occurrences(&self, call_id: &ToolCallId) -> usize {
        self.occurrences.get(call_id).copied().unwrap_or(0)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.occurrences.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.occurrences.len()
    }
}

impl ClearingState {
    pub(super) fn new(effective_window_tokens: Option<u64>, threshold_pct: u32) -> Self {
        Self {
            effective_window_tokens,
            threshold_pct,
            latest_input_tokens: 0,
            cleared: ClearedResults::default(),
        }
    }

    /// A state that can never fire -- the shape every session gets when
    /// `/models` declared no limits, and the shape the deterministic
    /// (no-API-key) fallback provider runs with.
    pub(super) fn disabled() -> Self {
        Self::new(None, crate::config::DEFAULT_CLEARING_TRIGGER_PCT)
    }

    /// Replays a persisted pass into this session's cleared set (resume
    /// path). Additive and idempotent, so replaying several passes in log
    /// order reproduces exactly the set the live session ended with.
    pub(super) fn seed_cleared(&mut self, call_ids: impl IntoIterator<Item = ToolCallId>) {
        self.cleared.record_occurrences(call_ids);
    }

    pub(super) fn record_input_tokens(&mut self, input_tokens: u64) {
        self.latest_input_tokens = input_tokens;
    }

    pub(super) fn cleared(&self) -> &ClearedResults {
        &self.cleared
    }

    /// Whether the measured input has reached the configured share of the
    /// effective window. Always `false` without a window.
    fn over_threshold(&self) -> bool {
        let Some(window) = self.effective_window_tokens else {
            return false;
        };
        self.latest_input_tokens.saturating_mul(100)
            >= window.saturating_mul(u64::from(self.threshold_pct))
    }

    /// Runs at most one clearing pass against `history`, freezing whatever
    /// it decides into this session's cleared set.
    ///
    /// `Some` means a pass happened and its `HistoryCleared` must be
    /// emitted; `None` means nothing changed -- the window is unknown, the
    /// measured input is still under the threshold, or the pass would not
    /// have recovered enough to be worth the prompt-cache loss.
    pub(super) fn run_pass(&mut self, history: &[Message]) -> Option<HistoryCleared> {
        if !self.over_threshold() {
            return None;
        }
        let plan = plan_clearing_pass(history, &self.cleared);
        if plan.recovered_chars / CLEARING_CHARS_PER_TOKEN < CLEARING_RECOVERY_FLOOR_TOKENS {
            return None;
        }
        self.cleared
            .record_occurrences(plan.cleared_call_ids.iter().cloned());
        Some(HistoryCleared {
            cleared_call_ids: plan.cleared_call_ids,
            recovered_chars: plan.recovered_chars,
        })
    }
}

/// What one pass decided, before it is frozen into [`ClearingState`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClearingPlan {
    /// History order, oldest first -- the order the pass walked, so a
    /// replayed set is identical to the live one.
    pub(super) cleared_call_ids: Vec<ToolCallId>,
    pub(super) recovered_chars: u64,
}

/// The pure pass: which tool results, given this history and the set
/// already cleared, become placeholders from now on.
///
/// Protections, all structural (`docs/agent-compaction-design.md` Tier 1's
/// 保護 list):
///
/// 1. **Only tool results are ever candidates.** User messages, the task
///    brief, assistant text, and `TaskNotification` messages are not
///    tool-result content, so they cannot be selected -- instruction
///    eviction is made impossible by construction, not by a rule that could
///    be mis-tuned.
/// 2. **The recent tail is kept verbatim.** Walking newest-to-oldest, tool
///    results are protected until their combined size reaches
///    [`CLEARING_TAIL_BUDGET_TOKENS`]; the walk that actually clears then
///    runs oldest-first and stops there.
/// 3. **The current tool-call round is never crossed.** Every result
///    belonging to the most recent assistant message that requested tools is
///    excluded, so a batch still resolving (its last result arrives as the
///    next request's prompt) can never be half-cleared.
/// 4. **Already-cleared results are not re-counted**, so a second pass
///    reports only what it newly recovered.
///
/// No per-tool exemptions in v1 -- deliberately, per the design doc.
pub(super) fn plan_clearing_pass(
    history: &[Message],
    already_cleared: &ClearedResults,
) -> ClearingPlan {
    let sites = tool_result_sites(history);
    let tail_start = protected_tail_start(&sites);
    let current_round = current_round_call_ids(history);

    let mut plan = ClearingPlan::default();
    for site in &sites[..tail_start] {
        if site.chars == 0
            || already_cleared.contains(&site.call_id)
            || current_round.contains(&site.call_id)
        {
            continue;
        }
        plan.recovered_chars = plan.recovered_chars.saturating_add(site.chars);
        plan.cleared_call_ids.push(site.call_id.clone());
    }
    plan
}

/// The provider view of the session's canonical history: `history` with the
/// content of every tool result in `cleared` replaced by a one-line
/// reference placeholder.
///
/// This is the single seam between canonical history and what a provider
/// request carries. It is a pure function of `(history, cleared)`, which is
/// what makes two consecutive request builds byte-identical while nothing
/// else changed -- see the module doc on why the set is frozen and why
/// recomputing it per request is rejected.
///
/// The walk is ordered and counts occurrences per `call_id`, replacing only
/// the first `cleared_occurrences(call_id)` of them: a result appended after
/// the pass froze is a later occurrence of a reused id, never one the pass
/// could have decided about, so it is left verbatim (module doc).
pub(super) fn history_for_provider_request(
    history: &[Message],
    cleared: &ClearedResults,
) -> Vec<Message> {
    if cleared.is_empty() {
        return history.to_vec();
    }

    let calls = tool_call_summaries(history);
    let mut seen: HashMap<ToolCallId, usize> = HashMap::new();
    let mut projected = history.to_vec();
    for message in &mut projected {
        let Message::User { content } = message else {
            continue;
        };
        for item in content.iter_mut() {
            let UserContent::ToolResult(result) = item else {
                continue;
            };
            let call_id = ToolCallId(result.id.clone());
            let occurrence = seen.entry(call_id.clone()).or_insert(0);
            let index = *occurrence;
            *occurrence += 1;
            if index >= cleared.cleared_occurrences(&call_id) {
                continue;
            }
            let original_chars = tool_result_chars(result);
            if original_chars == 0 {
                continue;
            }
            let placeholder = clearing_placeholder(calls.get(&call_id), original_chars);
            result.content = OneOrMany::one(ToolResultContent::text(placeholder));
        }
    }
    projected
}

/// Replays every persisted pass's frozen set out of a session's events --
/// the resume half of the freeze (see the module doc).
pub(super) fn cleared_call_ids_from_events(events: &[Event]) -> Vec<ToolCallId> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::HistoryCleared(cleared) => Some(cleared.cleared_call_ids.iter().cloned()),
            // Transcript/tool/approval content: none is a Tier 1 freeze
            // boundary, so none carries a cleared call-id set -- only
            // `HistoryCleared` does (see the module doc).
            Event::StateChanged(_)
            | Event::ReasoningDelta(_)
            | Event::AssistantTextDelta(_)
            | Event::MessageCommitted(_)
            | Event::ToolCallRequested(_)
            | Event::ToolCallStarted(_)
            | Event::ToolCallFinished(_)
            | Event::ApprovalRequested(_)
            // Provider request lifecycle markers are timing-only: no
            // clearing record.
            | Event::ProviderRequestSent(_)
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::ProviderRequestUsage(_)
            // Operator-intervention audit events: audit-only, no clearing
            // record (the resolved call is named by the `ToolCallStarted`
            // that follows, not by a freeze).
            | Event::ApprovalResolved(_)
            | Event::ContinueTurnRequested(_)
            // Turn/exit/error signals carry no clearing record.
            | Event::Error(_)
            | Event::ProviderRateLimited(_)
            | Event::Exited(_)
            | Event::TurnEnded(_) => None,
        })
        .flatten()
        .collect()
}

/// The one-line reference that replaces a cleared result's body.
///
/// Carries what the model needs to decide whether it wants the content back:
/// which tool ran, the call's key argument, how much text was elided, and
/// the two honest ways to get it -- `recall` over the event log (the
/// originals are still there), or simply running the tool again.
fn clearing_placeholder(call: Option<&ToolCallSummary>, original_chars: u64) -> String {
    let described = match call {
        Some(call) => match &call.key_argument {
            Some(argument) => format!("{} {}", call.tool_id, argument),
            None => call.tool_id.clone(),
        },
        None => "tool call".to_string(),
    };
    format!(
        "[cleared old tool result: {described} ({original_chars} chars). \
         The full result is retained in the session event log — use recall.search / \
         recall.read, or re-run the tool.]"
    )
}

/// One tool result's position in history, for the planning walk.
struct ToolResultSite {
    call_id: ToolCallId,
    chars: u64,
}

/// Every tool result in `history`, in history order.
fn tool_result_sites(history: &[Message]) -> Vec<ToolResultSite> {
    let mut sites = Vec::new();
    for message in history {
        let Message::User { content } = message else {
            continue;
        };
        for item in content.iter() {
            if let UserContent::ToolResult(result) = item {
                sites.push(ToolResultSite {
                    call_id: ToolCallId(result.id.clone()),
                    chars: tool_result_chars(result),
                });
            }
        }
    }
    sites
}

/// Index of the oldest tool result the tail budget protects: everything from
/// here on is kept verbatim, everything before it is a candidate.
///
/// Sizes are read from *canonical* history, so an already-cleared result
/// still counts at its original size. That is deliberate: it keeps the
/// boundary a pure function of history rather than of how many passes have
/// run, so successive passes agree with each other about where the tail
/// begins.
fn protected_tail_start(sites: &[ToolResultSite]) -> usize {
    let budget_chars = CLEARING_TAIL_BUDGET_TOKENS.saturating_mul(CLEARING_CHARS_PER_TOKEN);
    let mut accumulated = 0u64;
    let mut start = sites.len();
    for (index, site) in sites.iter().enumerate().rev() {
        if accumulated >= budget_chars {
            break;
        }
        accumulated = accumulated.saturating_add(site.chars);
        start = index;
    }
    start
}

/// Call ids requested by the most recent assistant message that requested
/// any -- "the current round". Its results are never cleared: at the moment
/// a pass runs, the round's last result is the prompt about to be sent and
/// is not in `history` yet, so treating the round as one indivisible unit is
/// the only reading that cannot half-clear it.
fn current_round_call_ids(history: &[Message]) -> HashSet<ToolCallId> {
    for message in history.iter().rev() {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        let ids: HashSet<ToolCallId> = content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(call) => Some(tool_call_id(
                    call.call_id.as_deref().unwrap_or(call.id.as_str()),
                )),
                _ => None,
            })
            .collect();
        if !ids.is_empty() {
            return ids;
        }
    }
    HashSet::new()
}

/// What the placeholder says about a cleared result's originating call.
struct ToolCallSummary {
    tool_id: String,
    key_argument: Option<String>,
}

/// Indexes every tool call in `history` by the id its result carries -- the
/// same `call_id.unwrap_or(id)` resolution `mapping::rig_tool_call_request`
/// applies, so the two agree on what a call's id is.
fn tool_call_summaries(history: &[Message]) -> HashMap<ToolCallId, ToolCallSummary> {
    let mut summaries = HashMap::new();
    for message in history {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        for item in content.iter() {
            let AssistantContent::ToolCall(call) = item else {
                continue;
            };
            let id = tool_call_id(call.call_id.as_deref().unwrap_or(call.id.as_str()));
            summaries.insert(
                id,
                ToolCallSummary {
                    tool_id: call.function.name.clone(),
                    key_argument: key_argument(&call.function.arguments),
                },
            );
        }
    }
    summaries
}

fn tool_call_id(id: &str) -> ToolCallId {
    ToolCallId(id.to_string())
}

/// `key="value"`, truncated, for the first of [`PLACEHOLDER_ARGUMENT_KEYS`]
/// the call actually passed.
fn key_argument(arguments: &serde_json::Value) -> Option<String> {
    PLACEHOLDER_ARGUMENT_KEYS.iter().find_map(|key| {
        let value = arguments.get(key)?.as_str()?;
        let (truncated, was_cut) =
            crate::transcript::truncate_chars(value, PLACEHOLDER_ARGUMENT_CHARS);
        let value = if was_cut {
            format!("{truncated}…")
        } else {
            truncated
        };
        Some(format!("{key}=\"{value}\""))
    })
}

fn tool_result_chars(result: &ToolResult) -> u64 {
    result
        .content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text(text) => text.text.chars().count() as u64,
            ToolResultContent::Image(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests;
