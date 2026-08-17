use std::time::Instant;

use crate::contract::*;

/// Tracks how long an [`AgentFrame`]'s `state` has held its current value,
/// for pane headers that show elapsed time in the current state (see
/// `docs/ux-principles.md`'s Persistent UI Requirement to show pane state).
///
/// `AgentFrame` itself doesn't carry this: its two-field shape is relied on
/// by callers that construct it as a plain struct literal, so timestamping
/// would live in this sidecar instead — a caller that needs it per session
/// would keep one alongside the frame and call [`Self::advance`] every time
/// it observes a new frame.
///
/// `cfg(test)`: no in-crate caller currently constructs one outside this
/// crate's own tests (confirmed by grep at the time of the 2026-07-18
/// interface audit) -- previously exempt from the dead-code lint only
/// because the type was `pub`.
#[derive(Clone, Copy, Debug)]
#[cfg(test)]
pub(crate) struct StateEntry {
    pub state: Option<SessionState>,
    entered_at: Instant,
}

#[cfg(test)]
impl StateEntry {
    pub(crate) fn initial(state: Option<SessionState>) -> Self {
        Self {
            state,
            entered_at: Instant::now(),
        }
    }

    /// Returns the entry that should be current after observing `state`:
    /// unchanged (same `entered_at`) if `state` matches, otherwise a fresh
    /// entry timestamped now.
    pub(crate) fn advance(self, state: Option<SessionState>) -> Self {
        if self.state == state {
            self
        } else {
            Self::initial(state)
        }
    }

    pub(crate) fn entered_at(&self) -> Instant {
        self.entered_at
    }
}

#[cfg(test)]
pub(crate) fn render_agent_transcript(events: &[Event]) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];

    for event in events {
        match event {
            Event::StateChanged(state) => lines.push(format!("state: {state:?}")),
            Event::ReasoningDelta(delta) => {
                lines.push(format!("{}: {}", delta.role.log_label(), delta.text));
            }
            Event::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", delta.role.log_label(), delta.text));
            }
            Event::MessageCommitted(message) => {
                lines.push(format!("{}: {}", message.role.log_label(), message.text));
            }
            Event::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            Event::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            Event::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output.0
                ));
            }
            Event::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            Event::ProviderRequestSent(sent) => {
                lines.push(format!("provider request sent: {}", sent.model));
            }
            Event::ProviderRequestFirstToken => {
                lines.push("provider request first token".to_string());
            }
            Event::ProviderRequestFinished => {
                lines.push("provider request finished".to_string());
            }
            Event::ProviderRequestUsage(usage) => {
                lines.push(format!(
                    "provider request usage: {} input, {} output, {} total, {} cached input",
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.total_tokens,
                    usage.cached_input_tokens,
                ));
            }
            Event::HistoryCleared(cleared) => {
                lines.push(format!(
                    "history cleared: {} tool result(s), {} chars",
                    cleared.cleared_call_ids.len(),
                    cleared.recovered_chars,
                ));
            }
            Event::ApprovalResolved(resolved) => {
                lines.push(format!(
                    "approval resolved: {} -> {:?}",
                    resolved.call_id.0, resolved.decision,
                ));
            }
            Event::ContinueTurnRequested(requested) => {
                lines.push(format!(
                    "continue turn requested: resumed_from {:?}",
                    requested.resumed_from,
                ));
            }
            Event::ProviderRateLimited(rate_limited) => lines.push(format!(
                "rate limited: status={:?} attempt={} backoff_ms={}",
                rate_limited.status, rate_limited.attempt, rate_limited.backoff_ms,
            )),
            Event::MemoryDigest(digest) => {
                if let Some(reason) = &digest.no_update_reason {
                    lines.push(format!("memory: no update ({reason})"));
                } else {
                    let fields: Vec<&str> = digest
                        .updates
                        .iter()
                        .map(|u| match u.field {
                            MemoryField::Goal => "goal",
                            MemoryField::Decisions => "decisions",
                            MemoryField::Completed => "completed",
                            MemoryField::InProgress => "in_progress",
                            MemoryField::Stuck => "stuck",
                            MemoryField::NextStep => "next_step",
                            MemoryField::Related => "related",
                        })
                        .collect();
                    lines.push(format!("memory: updated {}", fields.join(", ")));
                }
            }
            Event::MemoryCheckpointMissed => lines.push("memory: checkpoint missed".to_string()),
            Event::MemorySeeded => lines.push("memory: seeded".to_string()),
            Event::Error(error) => lines.push(format!("error: {}", error.message)),
            Event::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
            Event::TurnEnded(reason) => lines.push(format!("turn ended: {reason:?}")),
        }
    }

    lines.join("\n")
}
