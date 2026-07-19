//! Axis B: tool-result-aware history eviction --
//! `docs/research/agent-context-memory-separation-2026-07-20.md`'s
//! "Decision (2026-07-20)", opencode-prune-shaped.
//!
//! [`ToolResultPruningMemory`] replaces the stock `rig_memory::
//! TokenWindowMemory` previously returned by `completion::
//! history_token_window_policy`. Stock `TokenWindowMemory` treats every
//! message the same regardless of role or content: a pure recency cutoff
//! that can evict the task instruction itself just as readily as a bulky
//! tool result (see the research doc's "Context" section for the incident
//! this caused). This policy instead prefers to shrink bulky tool-result
//! *content* before it ever drops a whole message:
//!
//! 1. If the whole history already fits the budget, return it unchanged.
//! 2. Otherwise, replace the content of old tool-result messages (oldest
//!    first) with a short reference placeholder -- keeping the message
//!    itself (and its `id`/`call_id`, so tool-call/tool-result pairing
//!    never breaks) -- until the budget is met or every eligible tool
//!    result has been elided. The most recent tool results (within
//!    `protected_recent_tokens` of the end) are never touched, so pruning
//!    always reaches for the oldest bulky output first. Plain user/
//!    assistant text (in particular the task instruction, a
//!    `UserContent::Text`) is never touched by this step, so it survives
//!    as a byproduct.
//! 3. If the history is *still* over budget even with every eligible tool
//!    result reduced to a placeholder, fall back to dropping whole
//!    "turns" (a non-tool-result message plus every tool-result message
//!    immediately following it -- the exact shape this codebase's own
//!    session loop always builds, see `session.rs`'s `Command::
//!    ToolCallResult` handling) from the oldest, one turn at a time. This
//!    is atomic by construction: a turn is never split, so this can never
//!    leave an orphaned tool call or tool result.
//!
//! The full, unwindowed history (`rig_history`) is never touched by this --
//! only the cloned view `windowed_history_for_request` sends to the
//! provider for one turn (see that function's doc comment).

use std::collections::HashMap;
use std::sync::Arc;

use rig_core::completion::message::{ToolResult, ToolResultContent, UserContent};
use rig_core::completion::{AssistantContent, Message};
use rig_core::OneOrMany;
use rig_memory::{MemoryError, MemoryPolicy, TokenCounter};

/// A [`MemoryPolicy`] that prunes bulky old tool-result *content* to a short
/// placeholder before ever dropping a whole message -- see the module doc.
pub(super) struct ToolResultPruningMemory {
    max_tokens: usize,
    protected_recent_tokens: usize,
    counter: Arc<dyn TokenCounter>,
}

impl ToolResultPruningMemory {
    /// `max_tokens` is the overall budget (`RigAgentConfig::
    /// history_token_budget`); `protected_recent_tokens` is how much of the
    /// most-recent history (by the same counter) is never eligible for
    /// step 2's placeholder elision (`RigAgentConfig::
    /// protected_recent_tool_result_tokens`).
    pub(super) fn new<C>(max_tokens: usize, protected_recent_tokens: usize, counter: C) -> Self
    where
        C: TokenCounter + 'static,
    {
        Self {
            max_tokens,
            protected_recent_tokens,
            counter: Arc::new(counter),
        }
    }
}

impl MemoryPolicy for ToolResultPruningMemory {
    fn apply(&self, messages: Vec<Message>) -> Result<Vec<Message>, MemoryError> {
        Ok(self.apply_with_demoted(messages)?.0)
    }

    fn apply_with_demoted(
        &self,
        messages: Vec<Message>,
    ) -> Result<(Vec<Message>, Vec<Message>), MemoryError> {
        let total: usize = messages.iter().map(|m| self.counter.count(m)).sum();
        if total <= self.max_tokens {
            return Ok((messages, Vec::new()));
        }

        // Step 2: elide old tool-result content to a placeholder, oldest
        // first, stopping as soon as the budget is met or every eligible
        // (unprotected) tool result has been elided.
        let lookup = build_tool_call_lookup(&messages);
        let boundary = protected_boundary(
            &messages,
            self.counter.as_ref(),
            self.protected_recent_tokens,
        );

        let mut messages = messages;
        let mut current_total = total;
        for message in messages.iter_mut().take(boundary) {
            if current_total <= self.max_tokens {
                break;
            }
            if !is_tool_result_message(message) {
                continue;
            }
            let old_cost = self.counter.count(message);
            let placeholder = elide_tool_result_message(message, &lookup);
            let new_cost = self.counter.count(&placeholder);
            *message = placeholder;
            current_total = current_total
                .saturating_sub(old_cost)
                .saturating_add(new_cost);
        }

        if current_total <= self.max_tokens {
            return Ok((messages, Vec::new()));
        }

        // Step 3: still over budget with every tool result reduced to a
        // placeholder -- drop whole turns, oldest first, atomically.
        Ok(drop_oldest_turns_to_fit(
            messages,
            self.max_tokens,
            self.counter.as_ref(),
        ))
    }
}

/// True for a `Message::User` whose (first) content is a tool result --
/// mirrors `rig_memory::{SlidingWindowMemory, TokenWindowMemory}`'s own
/// orphan-check convention of only inspecting `content.first_ref()`, which
/// is always the *only* item for every tool-result message this codebase
/// constructs (`mapping::rig_tool_result_message` always builds a
/// single-item `OneOrMany`).
fn is_tool_result_message(message: &Message) -> bool {
    matches!(
        message,
        Message::User { content } if matches!(content.first_ref(), UserContent::ToolResult(_))
    )
}

/// What a tool-result placeholder cites back to: the requesting call's tool
/// id and a short summary of its arguments. `ToolResult` itself carries
/// only an `id`/`call_id` (the pairing key) and the *result* content -- the
/// tool name and arguments live on the paired `AssistantContent::ToolCall`
/// instead, so a placeholder needs this looked up separately.
struct ToolCallSummary {
    tool_id: String,
    args_summary: String,
}

/// Indexes every tool call across `messages` by its pairing key (`call_id`
/// if the provider supplied one, else `id` -- exactly
/// `mapping::rig_tool_call_request`'s own resolution, which is also the key
/// `ToolResult::id` is set to when Horizon builds a result message), so a
/// tool-result placeholder can name the call it's standing in for.
fn build_tool_call_lookup(messages: &[Message]) -> HashMap<String, ToolCallSummary> {
    let mut lookup = HashMap::new();
    for message in messages {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        for item in content.iter() {
            if let AssistantContent::ToolCall(call) = item {
                let key = call.call_id.clone().unwrap_or_else(|| call.id.clone());
                lookup.insert(
                    key,
                    ToolCallSummary {
                        tool_id: call.function.name.clone(),
                        args_summary: summarize_args(&call.function.arguments),
                    },
                );
            }
        }
    }
    lookup
}

/// Caps an args summary at this many `char`s (not bytes -- so truncation
/// never lands mid multi-byte character) before appending an ellipsis.
const PLACEHOLDER_ARGS_SUMMARY_MAX_CHARS: usize = 80;

fn summarize_args(args: &serde_json::Value) -> String {
    let compact = args.to_string();
    if compact.chars().count() <= PLACEHOLDER_ARGS_SUMMARY_MAX_CHARS {
        return compact;
    }
    let truncated: String = compact
        .chars()
        .take(PLACEHOLDER_ARGS_SUMMARY_MAX_CHARS)
        .collect();
    format!("{truncated}\u{2026}")
}

fn placeholder_text(result_id: &str, lookup: &HashMap<String, ToolCallSummary>) -> String {
    match lookup.get(result_id) {
        Some(summary) => format!(
            "[tool result cleared to fit context -- call {} with args {}; re-run if needed]",
            summary.tool_id, summary.args_summary
        ),
        None => "[tool result cleared to fit context; re-run if needed]".to_string(),
    }
}

/// Replaces every `UserContent::ToolResult` item in `message` with a short
/// placeholder, preserving that item's `id`/`call_id` exactly (the pairing
/// key the provider checks) and any non-tool-result items unchanged. Given
/// `is_tool_result_message` gates every call site, `message` is always a
/// `Message::User` with at least one `ToolResult` item in practice; the
/// `other => other.clone()` arm is defensive, not reachable in this
/// codebase's own message shapes.
fn elide_tool_result_message(
    message: &Message,
    lookup: &HashMap<String, ToolCallSummary>,
) -> Message {
    let Message::User { content } = message else {
        return message.clone();
    };
    let replaced: Vec<UserContent> = content
        .iter()
        .map(|item| match item {
            UserContent::ToolResult(result) => {
                let text = placeholder_text(&result.id, lookup);
                UserContent::ToolResult(ToolResult {
                    id: result.id.clone(),
                    call_id: result.call_id.clone(),
                    content: OneOrMany::one(ToolResultContent::text(text)),
                })
            }
            other => other.clone(),
        })
        .collect();
    Message::User {
        content: OneOrMany::many(replaced)
            .unwrap_or_else(|_| OneOrMany::one(UserContent::text(String::new()))),
    }
}

/// Returns the earliest index `i` such that `messages[i..]` fits within
/// `protected_tokens` -- mirrors `rig_memory::TokenWindowMemory`'s own
/// newest-to-oldest walk, just against a separate (smaller) budget used
/// only to mark a "never elide" zone rather than to cut anything. Every
/// index `>= boundary` is protected; indices before it are eligible for
/// step 2's placeholder elision.
fn protected_boundary(
    messages: &[Message],
    counter: &dyn TokenCounter,
    protected_tokens: usize,
) -> usize {
    let mut budget = protected_tokens;
    let mut boundary = messages.len();
    for (idx, msg) in messages.iter().enumerate().rev() {
        let cost = counter.count(msg);
        if cost > budget {
            break;
        }
        budget -= cost;
        boundary = idx;
    }
    boundary
}

/// Groups `messages` into atomic "turn" units: each unit starts at a
/// non-tool-result message and swallows every tool-result message
/// immediately following it. This is exactly the shape `session.rs`'s
/// `Command::ToolCallResult` handling always builds (an assistant
/// `tool_calls` message is immediately followed, with nothing else
/// interposed, by every one of its results -- see `fold_batched_tool_result`/
/// `run_cancellable_turn`), so a unit either carries no tool calls needing
/// results, or carries its own tool calls *and* all of their results --
/// dropping a whole unit can never orphan a pairing.
fn group_into_turns(messages: &[Message]) -> Vec<std::ops::Range<usize>> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let start = i;
        i += 1;
        while i < messages.len() && is_tool_result_message(&messages[i]) {
            i += 1;
        }
        units.push(start..i);
    }
    units
}

/// Drops whole turns (see [`group_into_turns`]) from the oldest, one at a
/// time, until the remainder fits `max_tokens` -- the last-resort fallback
/// once every eligible tool result is already a placeholder. Never splits a
/// turn, so tool-call/tool-result pairing always survives.
fn drop_oldest_turns_to_fit(
    messages: Vec<Message>,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> (Vec<Message>, Vec<Message>) {
    let units = group_into_turns(&messages);
    let mut running = 0usize;
    let mut keep_from_unit = units.len();
    for (idx, range) in units.iter().enumerate().rev() {
        let cost: usize = messages[range.clone()]
            .iter()
            .map(|m| counter.count(m))
            .sum();
        if running + cost > max_tokens {
            break;
        }
        running += cost;
        keep_from_unit = idx;
    }
    let keep_from_msg = units
        .get(keep_from_unit)
        .map(|range| range.start)
        .unwrap_or(messages.len());

    let mut iter = messages.into_iter();
    let demoted: Vec<Message> = (&mut iter).take(keep_from_msg).collect();
    let window: Vec<Message> = iter.collect();
    (window, demoted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{ToolCall, ToolFunction};
    use rig_memory::HeuristicTokenCounter;

    /// An exact-byte counter (`bytes_per_token = 1.0`, no per-message or
    /// attachment overhead) rather than the production `HeuristicTokenCounter::
    /// openai()` preset -- lets every test below reason about *exact* token
    /// costs (message content length, in bytes) instead of an approximated
    /// preset, so budgets can be chosen to land precisely on one side or the
    /// other of an elision/drop decision.
    fn exact_byte_counter() -> HeuristicTokenCounter {
        HeuristicTokenCounter::new(1.0, 0, 0)
    }

    fn user_text(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant_text(text: &str) -> Message {
        Message::assistant(text)
    }

    fn tool_call(id: &str, tool_id: &str, args: serde_json::Value) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                id.to_string(),
                ToolFunction::new(tool_id.to_string(), args),
            ))),
        }
    }

    fn tool_calls(calls: &[(&str, &str, serde_json::Value)]) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::many(
                calls
                    .iter()
                    .map(|(id, tool_id, args)| {
                        AssistantContent::ToolCall(ToolCall::new(
                            id.to_string(),
                            ToolFunction::new(tool_id.to_string(), args.clone()),
                        ))
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("at least one call"),
        }
    }

    fn tool_result(id: &str, text: &str) -> Message {
        Message::tool_result(id.to_string(), text.to_string())
    }

    fn is_placeholder(message: &Message) -> bool {
        matches!(message, Message::User { content }
            if matches!(content.first_ref(), UserContent::ToolResult(result)
                if matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.starts_with("[tool result cleared"))))
    }

    fn tool_result_text(message: &Message) -> &str {
        let Message::User { content } = message else {
            panic!("expected a User message");
        };
        let UserContent::ToolResult(result) = content.first_ref() else {
            panic!("expected a ToolResult");
        };
        let ToolResultContent::Text(text) = result.content.first_ref() else {
            panic!("expected text content");
        };
        &text.text
    }

    #[test]
    fn within_budget_history_is_returned_unchanged() {
        let history = vec![
            user_text("do the thing"),
            tool_call("call-1", "fs.read", serde_json::json!({ "path": "/a" })),
            tool_result("call-1", "file contents"),
            assistant_text("done"),
        ];
        let policy = ToolResultPruningMemory::new(100_000, 100_000, exact_byte_counter());

        let windowed = policy.apply(history.clone()).expect("apply never fails");

        assert_eq!(windowed, history);
    }

    #[test]
    fn over_budget_elides_the_oldest_tool_result_first_but_keeps_the_task_instruction() {
        // Exact byte costs (bytes_per_token=1.0, no overhead): instruction=28,
        // each tool_call ("fs.read" + `{"path":"/a"}`)=7+13=20, each 1000-byte
        // tool result=1000, final assistant text=7. Total=28+20+1000+20+1000+7
        // =2075. Eliding just the oldest result to its placeholder (94 bytes
        // exactly for this template + a 13-byte args summary) brings the
        // total to 2075-1000+94=1169, comfortably under a 1200 budget --
        // so only the oldest tool result needs to be touched at all.
        let history = vec![
            user_text("please summarize these files"), // 0: instruction (29 bytes)
            tool_call("call-1", "fs.read", serde_json::json!({ "path": "/a" })),
            tool_result("call-1", &"A".repeat(1000)), // 2: oldest tool result
            tool_call("call-2", "fs.read", serde_json::json!({ "path": "/b" })),
            tool_result("call-2", &"B".repeat(1000)), // 4: newest tool result
            assistant_text("summary"),                // 5: newest message (7 bytes)
        ];
        // No protection window (0): every tool result is eligible, but the
        // oldest-first walk should still reach for index 2 before index 4.
        let policy = ToolResultPruningMemory::new(1200, 0, exact_byte_counter());

        let windowed = policy.apply(history.clone()).expect("apply never fails");

        assert_eq!(
            windowed.len(),
            history.len(),
            "elision replaces content, it never removes messages by itself"
        );
        assert_eq!(
            windowed[0], history[0],
            "the task instruction must survive elision untouched"
        );
        assert!(
            is_placeholder(&windowed[2]),
            "the oldest tool result must be elided first"
        );
        assert!(
            !is_placeholder(&windowed[4]),
            "eliding the oldest result alone already meets budget, so the newer \
             result must be left untouched"
        );
        // The oldest tool call's placeholder cites its tool id and args.
        assert!(tool_result_text(&windowed[2]).contains("fs.read"));
        assert!(tool_result_text(&windowed[2]).contains("/a"));
    }

    #[test]
    fn recent_tool_results_within_the_protected_window_are_never_elided() {
        // Exact byte costs: instruction=1, each tool_call ("fs.read" +
        // `{"path":"/old"}`/`{"path":"/new"}`, 15-byte args)=7+15=22, each
        // 1000-byte tool result=1000, final assistant text=1. A
        // protected-recent-tokens floor of 1050 walks back from the end
        // (1 + 1000 + 22 = 1023 <= 1050, plus the next 1000-byte result
        // would overshoot) landing the protected boundary at index 3 --
        // so the newest tool result (index 4) is protected, but the older
        // one (index 2) is not.
        let history = vec![
            user_text("I"),
            tool_call("call-1", "fs.read", serde_json::json!({ "path": "/old" })),
            tool_result("call-1", &"O".repeat(1000)), // 2: unprotected (old)
            tool_call("call-2", "fs.read", serde_json::json!({ "path": "/new" })),
            tool_result("call-2", &"N".repeat(1000)), // 4: protected (recent)
            assistant_text("F"),
        ];
        let policy = ToolResultPruningMemory::new(1200, 1050, exact_byte_counter());

        let windowed = policy.apply(history.clone()).expect("apply never fails");

        assert!(
            is_placeholder(&windowed[2]),
            "the older, unprotected tool result must be elided"
        );
        assert_eq!(
            windowed[4], history[4],
            "the protected, most-recent tool result must be untouched"
        );
    }

    #[test]
    fn batched_results_are_each_independently_eligible_while_the_assistant_survives() {
        // One assistant message requesting 3 parallel tool calls, followed
        // by their 3 results -- the shape `session.rs`'s batching path
        // always builds. Exact byte costs: instruction=11, the 3-call
        // assistant message=3*(7+13)=60, each 500-byte tool result=500,
        // final assistant text=4; total=11+60+500+500+500+4=1575. A 1000
        // budget requires eliding exactly 2 of the 3 results (1575-500+94=
        // 1169 still over budget; 1169-500+94=763 under it) before the
        // loop's own budget check stops it ahead of the third.
        let assistant = tool_calls(&[
            ("call-a", "fs.read", serde_json::json!({ "path": "/a" })),
            ("call-b", "fs.read", serde_json::json!({ "path": "/b" })),
            ("call-c", "fs.read", serde_json::json!({ "path": "/c" })),
        ]);
        let history = vec![
            user_text("instruction"),
            assistant.clone(),
            tool_result("call-a", &"A".repeat(500)),
            tool_result("call-b", &"B".repeat(500)),
            tool_result("call-c", &"C".repeat(500)),
            assistant_text("done"),
        ];
        let policy = ToolResultPruningMemory::new(1000, 0, exact_byte_counter());

        let windowed = policy.apply(history.clone()).expect("apply never fails");

        assert_eq!(
            windowed[1], assistant,
            "the assistant's tool_calls message is never elided or dropped here"
        );
        assert!(
            is_placeholder(&windowed[2]),
            "the batch's oldest result must be elided independently"
        );
        assert!(
            is_placeholder(&windowed[3]),
            "the batch's second result must also be elided independently"
        );
        assert!(
            !is_placeholder(&windowed[4]),
            "the batch's most recent (newest) result should survive once the \
             budget is already met"
        );
    }

    #[test]
    fn still_over_budget_after_full_elision_drops_the_oldest_whole_turn() {
        // Exact byte costs: instruction=11, each tool_call=20, each 300-byte
        // tool result=300, final assistant text=5; total=11+20+300+20+300+5
        // =656. Eliding both results (300 -> 94 each) only gets to
        // 656-600+188=244, still over a 200 budget -- step 3 must drop the
        // oldest whole turn (the lone instruction) to fit.
        let call_a = tool_call("call-a", "fs.read", serde_json::json!({ "path": "/a" }));
        let result_a = tool_result("call-a", &"X".repeat(300));
        let call_b = tool_call("call-b", "fs.read", serde_json::json!({ "path": "/b" }));
        let result_b = tool_result("call-b", &"Y".repeat(300));
        let history = vec![
            user_text("instruction"), // unit 0
            call_a.clone(),           // unit 1 start
            result_a.clone(),         // unit 1 (swallowed)
            call_b.clone(),           // unit 2 start
            result_b.clone(),         // unit 2 (swallowed)
            assistant_text("final"),  // unit 3
        ];
        let policy = ToolResultPruningMemory::new(200, 0, exact_byte_counter());

        let windowed = policy.apply(history.clone()).expect("apply never fails");

        assert!(
            windowed.len() < history.len(),
            "step 3 must drop whole turns once elision alone can't fit the budget"
        );
        // Pairing invariant: every tool-result message's id has a preceding
        // tool call in the surviving window, and every tool call's ids all
        // have a following result -- i.e. no orphans either direction.
        assert_pairing_intact(&windowed);
        // The oldest turn (the instruction, a singleton unit) is dropped
        // before the newer call_a/result_a turn.
        assert!(
            !windowed.contains(&user_text("instruction")),
            "the oldest turn must be dropped first"
        );
        assert_eq!(
            windowed.last(),
            history.last(),
            "the newest turn must survive"
        );
    }

    #[test]
    fn windowed_history_for_request_style_policy_error_path_is_unaffected() {
        // Sanity check that this policy participates in the existing
        // fallback-on-error contract the same way stock policies do (see
        // `completion::windowed_history_for_request`'s doc comment) --
        // `apply` here never actually errors, but the trait boundary must
        // still hold.
        let policy = ToolResultPruningMemory::new(100_000, 100_000, exact_byte_counter());
        let history = vec![user_text("hi")];

        let result: Result<Vec<Message>, MemoryError> = policy.apply(history.clone());

        assert_eq!(result.unwrap(), history);
    }

    /// Asserts every tool-result message's id has a preceding tool call
    /// somewhere earlier in `messages`, and every tool call's id has a
    /// following tool-result message somewhere later -- the invariant the
    /// provider enforces (`session.rs:219-220, 271-273`).
    fn assert_pairing_intact(messages: &[Message]) {
        let mut called_ids: Vec<&str> = Vec::new();
        let mut result_ids: Vec<&str> = Vec::new();
        for message in messages {
            match message {
                Message::Assistant { content, .. } => {
                    for item in content.iter() {
                        if let AssistantContent::ToolCall(call) = item {
                            called_ids.push(call.call_id.as_deref().unwrap_or(call.id.as_str()));
                        }
                    }
                }
                Message::User { content } => {
                    if let UserContent::ToolResult(result) = content.first_ref() {
                        result_ids.push(&result.id);
                    }
                }
                Message::System { .. } => {}
            }
        }
        for id in &result_ids {
            assert!(
                called_ids.contains(id),
                "orphan tool result with no matching call: {id}"
            );
        }
        for id in &called_ids {
            assert!(
                result_ids.contains(id),
                "orphan tool call with no matching result: {id}"
            );
        }
    }
}
