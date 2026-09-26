//! Reversible provider projection. Durable result identities choose the cleared
//! bodies; explicit conversation turns choose the standing-memory boundary.
use super::conversation::ConversationHistory;
use crate::{
    config::{
        CLEARING_CHARS_PER_TOKEN, CLEARING_RECOVERY_FLOOR_TOKENS, CLEARING_TAIL_BUDGET_TOKENS,
    },
    contract::{Event, HistoryCleared, OccurrenceId},
    tools::MemoryDocument,
};
use rig_core::completion::{
    message::{ToolResultContent, UserContent},
    Message,
};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub(super) struct ClearingState {
    effective_window_tokens: Option<u64>,
    threshold_pct: u32,
    latest_input_tokens: u64,
    cleared: ClearedResults,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClearedResults {
    occurrences: HashSet<OccurrenceId>,
}
impl ClearedResults {
    #[cfg(test)]
    pub(super) fn from_occurrences(ids: impl IntoIterator<Item = OccurrenceId>) -> Self {
        Self {
            occurrences: ids.into_iter().collect(),
        }
    }
    pub(super) fn contains(&self, id: &OccurrenceId) -> bool {
        self.occurrences.contains(id)
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
    pub(super) fn disabled() -> Self {
        Self::new(None, crate::config::DEFAULT_CLEARING_TRIGGER_PCT)
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
    pub(super) fn run_pass(&mut self, history: &ConversationHistory) -> Option<HistoryCleared> {
        let window = self.effective_window_tokens?;
        if self.latest_input_tokens.saturating_mul(100)
            < window.saturating_mul(u64::from(self.threshold_pct))
        {
            return None;
        }
        let plan = plan_clearing_pass(history, &self.cleared);
        if plan.recovered_chars / CLEARING_CHARS_PER_TOKEN < CLEARING_RECOVERY_FLOOR_TOKENS {
            return None;
        }
        self.seed_cleared(plan.cleared_occurrence_ids.iter().cloned());
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
    memory: Option<&MemoryDocument>,
    moa: Option<&(usize, Message)>,
) -> Vec<Message> {
    let mut messages = history.messages();
    for site in history.result_sites() {
        if !cleared.contains(&site.result.occurrence_id) {
            continue;
        }
        let argument = key_argument(&site.tool.function.arguments);
        let described = argument
            .map(|arg| format!("{} {arg}", site.tool.function.name))
            .unwrap_or_else(|| site.tool.function.name.clone());
        let placeholder = format!("[cleared old tool result: {described} ({} chars). The full result is retained in the session event log — use recall.search / recall.read, or re-run the tool.]", result_chars(site.result));
        let Message::User { content } = &mut messages[site.message_index] else {
            unreachable!()
        };
        let UserContent::ToolResult(result) = &mut content[0] else {
            unreachable!()
        };
        result.content = vec![ToolResultContent::text(placeholder)];
    }
    let mut tail_start = history.turn_start();
    if let Some((index, message)) = moa {
        let index = (*index).min(messages.len());
        messages.insert(index, message.clone());
        // A proposal inserted at the turn boundary belongs to that turn.
        if index < tail_start {
            tail_start += 1;
        }
    }
    if let Some(document) = memory.filter(|doc| !doc.is_empty()) {
        messages = std::iter::once(Message::user(document.render()))
            .chain(messages[tail_start..].iter().cloned())
            .collect();
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
