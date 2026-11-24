//! Reversible provider projection. Durable result identities choose the cleared
//! bodies.
use super::conversation::ConversationHistory;
use crate::{
    config::{
        CLEARING_CHARS_PER_TOKEN, CLEARING_RECOVERY_FLOOR_TOKENS, CLEARING_TAIL_BUDGET_TOKENS,
    },
    contract::{Event, HistoryCleared, OccurrenceId},
};
use rig_core::completion::{
    Message,
    message::{ToolResultContent, UserContent},
};
use std::collections::HashSet;

/// What the session has hidden from the requests it sends, and what the
/// context stage's modules decide more from (`modules::ContextContext`).
#[derive(Clone, Debug)]
pub(super) struct ClearingState {
    effective_window_tokens: Option<u64>,
    latest_input_tokens: u64,
    cleared: ClearedResults,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClearedResults {
    occurrences: HashSet<OccurrenceId>,
}
impl ClearedResults {
    #[cfg(test)]
    pub(super) fn from_occurrences(ids: impl IntoIterator<Item = OccurrenceId>) -> Self {
        Self {
            occurrences: ids.into_iter().collect(),
        }
    }
    pub(crate) fn contains(&self, id: &OccurrenceId) -> bool {
        self.occurrences.contains(id)
    }
}
impl ClearingState {
    pub(super) fn new(effective_window_tokens: Option<u64>) -> Self {
        Self {
            effective_window_tokens,
            latest_input_tokens: 0,
            cleared: ClearedResults::default(),
        }
    }
    pub(super) fn disabled() -> Self {
        Self::new(None)
    }
    pub(super) fn seed_cleared(&mut self, ids: impl IntoIterator<Item = OccurrenceId>) {
        self.cleared.occurrences.extend(ids);
    }
    pub(super) fn record_input_tokens(&mut self, tokens: u64) {
        self.latest_input_tokens = tokens;
    }
    pub(super) fn adopt_window(&mut self, window: Option<u64>) {
        self.effective_window_tokens = window;
    }
    #[cfg(test)]
    pub(super) fn effective_window_tokens(&self) -> Option<u64> {
        self.effective_window_tokens
    }
    #[cfg(test)]
    pub(super) fn latest_input_tokens(&self) -> u64 {
        self.latest_input_tokens
    }
    pub(super) fn cleared(&self) -> &ClearedResults {
        &self.cleared
    }
    /// One pass of Tier 1 clearing at `threshold_pct`, as the context
    /// stage runs it.
    #[cfg(test)]
    pub(super) fn run_pass_at(
        &mut self,
        history: &ConversationHistory,
        threshold_pct: u32,
    ) -> Option<HistoryCleared> {
        let tier_one: std::sync::Arc<dyn crate::modules::SessionModule> =
            std::sync::Arc::new(TierOneClearing { threshold_pct });
        self.run_stage(history, &[tier_one]).pop()
    }

    /// Asks the context stage's `modules` for more results to hide, hides
    /// them, and returns each module's pass for the record.
    pub(super) fn run_stage(
        &mut self,
        history: &ConversationHistory,
        modules: &[std::sync::Arc<dyn crate::modules::SessionModule>],
    ) -> Vec<HistoryCleared> {
        let mut passes = Vec::new();
        for module in modules {
            let cx = crate::modules::ContextContext {
                history,
                cleared: &self.cleared,
                window: self.effective_window_tokens,
                input_tokens: self.latest_input_tokens,
            };
            if let Some(pass) = module.context(&cx) {
                self.seed_cleared(pass.cleared_occurrence_ids.iter().cloned());
                passes.push(pass);
            }
        }
        passes
    }
}

/// Tier 1 clearing (`docs/agent-compaction-design.md`) as a module of the
/// context stage: once the last request filled `threshold_pct` of the
/// model's window, the oldest tool results outside a recent tail are
/// hidden, when that recovers enough to matter.
pub(crate) struct TierOneClearing {
    pub(crate) threshold_pct: u32,
}

impl crate::modules::SessionModule for TierOneClearing {
    fn context(&self, cx: &crate::modules::ContextContext<'_>) -> Option<HistoryCleared> {
        let window = cx.window?;
        if cx.input_tokens.saturating_mul(100)
            < window.saturating_mul(u64::from(self.threshold_pct))
        {
            return None;
        }
        let plan = plan_clearing_pass(cx.history, cx.cleared);
        if plan.recovered_chars / CLEARING_CHARS_PER_TOKEN < CLEARING_RECOVERY_FLOOR_TOKENS {
            return None;
        }
        Some(plan)
    }
}
pub(super) fn plan_clearing_pass(
    history: &ConversationHistory,
    cleared: &ClearedResults,
) -> HistoryCleared {
    let sites = history.result_sites();
    let mut tail_start = sites.len();
    let mut tail_chars = 0u64;
    for (index, site) in sites.iter().enumerate().rev() {
        if tail_chars >= CLEARING_TAIL_BUDGET_TOKENS.saturating_mul(CLEARING_CHARS_PER_TOKEN) {
            break;
        }
        tail_chars = tail_chars.saturating_add(result_chars(site.result));
        tail_start = index;
    }
    let mut plan = HistoryCleared {
        cleared_occurrence_ids: Vec::new(),
        recovered_chars: 0,
    };
    for site in &sites[..tail_start] {
        if site.current_response || cleared.contains(&site.result.occurrence_id) {
            continue;
        }
        let chars = result_chars(site.result);
        if chars == 0 {
            continue;
        }
        plan.cleared_occurrence_ids
            .push(site.result.occurrence_id.clone());
        plan.recovered_chars = plan.recovered_chars.saturating_add(chars);
    }
    plan
}

pub(super) fn history_for_provider_request(
    history: &ConversationHistory,
    cleared: &ClearedResults,
    earlier: super::conversation::EarlierReasoning,
) -> Vec<Message> {
    let mut messages = history.messages_with(earlier);
    for site in history.result_sites() {
        if !cleared.contains(&site.result.occurrence_id) {
            continue;
        }
        let argument = key_argument(&serde_json::Value::Object(
            site.tool.function.arguments.clone(),
        ));
        let described = argument
            .map(|arg| format!("{} {arg}", site.tool.function.name.as_str()))
            .unwrap_or_else(|| site.tool.function.name.as_str().to_string());
        let placeholder = format!(
            "[cleared old tool result: {described} ({} chars). The full result is retained in the session event log — use recall_search / recall_read, or re-run the tool.]",
            result_chars(site.result)
        );
        let Message::User { content } = &mut messages[site.message_index] else {
            unreachable!()
        };
        let UserContent::ToolResult(result) = &mut content[0] else {
            unreachable!()
        };
        result.content = vec![ToolResultContent::text(placeholder)];
    }
    messages
}
fn result_chars(result: &crate::contract::ToolCallResult) -> u64 {
    serde_json::json!({"outcome":result.outcome,"output":result.output})
        .to_string()
        .chars()
        .count() as u64
}
fn key_argument(arguments: &serde_json::Value) -> Option<String> {
    ["path", "command", "pattern", "query", "url", "session_id"]
        .iter()
        .find_map(|key| {
            let value = arguments.get(key)?.as_str()?;
            let (value, cut) = crate::transcript::truncate_chars(value, 120);
            Some(format!("{key}=\"{value}{}\"", if cut { "…" } else { "" }))
        })
}
pub(super) fn cleared_occurrence_ids_from_events(events: &[Event]) -> Vec<OccurrenceId> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::HistoryCleared(cleared) => Some(cleared.cleared_occurrence_ids.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}
#[cfg(test)]
mod tests;
