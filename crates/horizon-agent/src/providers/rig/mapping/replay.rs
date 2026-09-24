//! Repair the provider-facing conversation reconstructed from persisted events.

use std::collections::{HashMap, HashSet};

use rig_core::completion::{
    message::{ToolCall, UserContent},
    AssistantContent, Message,
};

use crate::{contract::ToolCallId, tools::cancelled_tool_call_result};

use super::rig_tool_result_message;

/// Makes a rebuilt history satisfy the tool-call pairing invariant strict
/// chat templates enforce, so a resumed session can't be killed by a shape
/// only the *rebuild* can produce. Same family as
/// `completion::replay_safe_tool_arguments`: history-rebuild template
/// safety.
///
/// Three repairs, all order-preserving and idempotent:
///
/// 1. **Adjacent assistant messages are folded into one.** Live, a provider
///    response is a single assistant message carrying its text *and* its
///    tool calls. The event log splits that into one `ToolCallRequested` per
///    call plus a `MessageCommitted` for the text — and the text event is
///    only emitted once the response stream ends
///    (`completion::rig_openai_turn_streaming`), while tool calls are
///    emitted as their chunks arrive and Horizon starts executing them
///    immediately. A tool that finishes after the stream does lands its
///    `ToolCallFinished` *after* the text event, so a naive replay puts a
///    text-only assistant message between a tool call and the result that
///    answers it. That is what killed session `b182c25b` on 2026-07-28:
///    MiniMax-M3's template rejected the history permanently with "Message
///    has tool role, but there was no previous assistant message with a tool
///    call!", and because the shape is *in* the rebuilt history, every later
///    request 400'd too. Folding the run restores the live shape.
/// 2. **A tool result no assistant message announced is dropped.** Nothing
///    is synthesized in its place: the provider never saw a call, so
///    inventing one would put words in its mouth. Dropping a result no
///    completed request ever consumed is the honest repair.
/// 3. **An announced call nothing ever answers gets a cancelled result**,
///    the same synthesis `session::append_cancelled_tool_results_to_history`
///    and `horizon-agentd`'s startup fixup already use, inserted where the
///    real result would have gone. The rebuild needs its own copy because
///    the two run against different stores: the startup fixup appends to the
///    event log, while the rebuild reads the DuckDB projection, which the
///    writer thread updates asynchronously — so a resumed session can load
///    its history before the fixup's cancelled results have landed there.
///
/// Not repaired: with a *parallel* tool batch whose results arrive out of
/// order, a result can still sit behind an assistant message that announced
/// a different call of the same batch. Templates that match the call id
/// against the nearest assistant message (rather than just requiring one
/// with tool calls, as the observed failure does) would still object; no
/// such rejection has been seen.
pub(in crate::providers::rig) fn repair_replayed_message_pairing(
    messages: Vec<Message>,
) -> Vec<Message> {
    let mut repair = PairingRepair::new(&messages);
    for (index, message) in messages.into_iter().enumerate() {
        repair.push(index, message);
    }
    repair.finish()
}

/// Look ahead by announcement occurrence, not provider id: an id can be reused
/// after a missing answer. Each real result consumes one preceding announcement.
struct ReplayIndex {
    answered: HashSet<usize>,
    accepted_results: HashSet<usize>,
}

impl ReplayIndex {
    fn new(messages: &[Message]) -> Self {
        let mut pending = HashMap::new();
        let mut answered = HashSet::new();
        let mut accepted_results = HashSet::new();
        let mut next_call = 0;
        for (index, message) in messages.iter().enumerate() {
            if let Some(call_id) = answered_tool_call_id(message) {
                if let Some(announcement) = pending.remove(call_id) {
                    answered.insert(announcement);
                    accepted_results.insert(index);
                }
            } else if let Message::Assistant { content, .. } = message {
                for call in announced_tool_calls(content) {
                    pending.insert(call.id.as_str(), next_call);
                    next_call += 1;
                }
            }
        }
        Self {
            answered,
            accepted_results,
        }
    }
}

/// Forward-only repair state. Unanswered calls retain announcement order;
/// results without a preceding announcement never enter the rebuilt history.
struct PairingRepair {
    index: ReplayIndex,
    next_call: usize,
    unanswered: Vec<(String, String)>,
    dropped: Vec<String>,
    synthesized: Vec<String>,
    repaired: Vec<Message>,
}

impl PairingRepair {
    fn new(messages: &[Message]) -> Self {
        Self {
            index: ReplayIndex::new(messages),
            next_call: 0,
            unanswered: Vec::new(),
            dropped: Vec::new(),
            synthesized: Vec::new(),
            repaired: Vec::with_capacity(messages.len()),
        }
    }

    fn push(&mut self, index: usize, message: Message) {
        if let Some(call_id) = answered_tool_call_id(&message) {
            if self.index.accepted_results.contains(&index) {
                self.repaired.push(message);
            } else {
                self.dropped.push(call_id.to_string());
            }
            return;
        }
        match message {
            Message::Assistant { id, content } => self.push_assistant(id, content),
            other => {
                self.close_unanswered_calls();
                self.repaired.push(other);
            }
        }
    }

    fn push_assistant(&mut self, id: Option<String>, content: Vec<AssistantContent>) {
        if !matches!(self.repaired.last(), Some(Message::Assistant { .. })) {
            self.close_unanswered_calls();
        }
        for call in announced_tool_calls(&content) {
            if !self.index.answered.contains(&self.next_call) {
                self.unanswered
                    .push((call.id.as_str().to_string(), call.function.name.clone()));
            }
            self.next_call += 1;
        }
        match self.repaired.last_mut() {
            Some(Message::Assistant {
                id: run_id,
                content: run_content,
            }) => {
                // Restore the single provider response split across events.
                if run_id.is_none() {
                    *run_id = id;
                }
                run_content.extend(content);
            }
            _ => {
                self.repaired.push(Message::Assistant { id, content });
            }
        }
    }

    /// Close calls that will never be answered where their results would sit.
    fn close_unanswered_calls(&mut self) {
        for (call_id, tool_name) in self.unanswered.drain(..) {
            self.repaired.push(rig_tool_result_message(
                &cancelled_tool_call_result(ToolCallId(call_id.clone())),
                &tool_name,
            ));
            self.synthesized.push(call_id);
        }
    }

    fn finish(mut self) -> Vec<Message> {
        self.close_unanswered_calls();
        // Match the daemon's resume fixups: every repair is visible on stderr.
        if !self.dropped.is_empty() {
            eprintln!(
                "horizon-agent: dropped {} orphaned tool result(s) while rebuilding provider \
                 history (no unanswered assistant call precedes them): {}",
                self.dropped.len(),
                self.dropped.join(", ")
            );
        }
        if !self.synthesized.is_empty() {
            eprintln!(
                "horizon-agent: closed {} unanswered tool call(s) with a cancelled result while \
                 rebuilding provider history: {}",
                self.synthesized.len(),
                self.synthesized.join(", ")
            );
        }
        self.repaired
    }
}

fn announced_tool_calls(content: &[AssistantContent]) -> impl Iterator<Item = &ToolCall> {
    content.iter().filter_map(|item| match item {
        AssistantContent::ToolCall(call) => Some(call),
        _ => None,
    })
}

/// The call id a tool-result message answers. Rig models a tool result as a
/// user message whose content is entirely `ToolResult` ([`Message::tool_result`],
/// and [`rig_tool_result_message`] with it); a plain user text message is
/// `None`.
fn answered_tool_call_id(message: &Message) -> Option<&str> {
    let Message::User { content } = message else {
        return None;
    };
    if !content
        .iter()
        .all(|item| matches!(item, UserContent::ToolResult(_)))
    {
        return None;
    }
    match content.first() {
        Some(UserContent::ToolResult(result)) => Some(result.call.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rig_core::completion::message::{ToolCall, ToolFunction};

    use super::*;

    fn call(id: &str, name: &str) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall::new(
            rig_core::message::ToolCallId::new_or_mint(id),
            ToolFunction::new(name.to_string(), serde_json::json!({})),
        ))
    }

    #[test]
    fn merged_response_retains_its_first_message_id_content_order_and_cancelled_tool_names() {
        let first = call("first", "fs.read");
        let second = call("second", "bash");
        let messages = vec![
            Message::Assistant {
                id: None,
                content: vec![first.clone()],
            },
            Message::Assistant {
                id: Some("response-1".to_string()),
                content: vec![AssistantContent::text("Checking."), second.clone()],
            },
            Message::Assistant {
                id: Some("response-2".to_string()),
                content: vec![AssistantContent::text("Still checking.")],
            },
            Message::user("Stop."),
        ];
        let repaired = repair_replayed_message_pairing(messages);
        assert_eq!(
            repaired,
            vec![
                Message::Assistant {
                    id: Some("response-1".to_string()),
                    content: vec![
                        first,
                        AssistantContent::text("Checking."),
                        second,
                        AssistantContent::text("Still checking.")
                    ],
                },
                rig_tool_result_message(
                    &cancelled_tool_call_result(ToolCallId("first".to_string())),
                    "fs.read"
                ),
                rig_tool_result_message(
                    &cancelled_tool_call_result(ToolCallId("second".to_string())),
                    "bash"
                ),
                Message::user("Stop."),
            ]
        );
        assert_eq!(repair_replayed_message_pairing(repaired.clone()), repaired);
    }

    #[test]
    fn a_result_before_its_announcement_does_not_prevent_cancellation() {
        let request = Message::Assistant {
            id: None,
            content: vec![call("call-1", "fs.read")],
        };
        let repaired = repair_replayed_message_pairing(vec![
            Message::tool_result("call-1", "fs.read", "orphan"),
            request.clone(),
        ]);
        assert_eq!(
            repaired,
            vec![
                request,
                rig_tool_result_message(
                    &cancelled_tool_call_result(ToolCallId("call-1".to_string())),
                    "fs.read"
                ),
            ]
        );
        assert_eq!(repair_replayed_message_pairing(repaired.clone()), repaired);
    }
}
