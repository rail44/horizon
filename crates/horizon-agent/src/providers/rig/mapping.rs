use std::collections::HashSet;

use rig_core::completion::{
    message::{ToolCall, ToolFunction, UserContent},
    AssistantContent, Message,
};
use rig_core::OneOrMany;

use crate::contract::{
    Event, Message as AgentMessage, MessageDelta, MessageRole, OccurrenceId, ProviderEvent,
    ProviderSide, ToolCallId, ToolCallRequest, ToolCallResult,
};
use crate::tools::cancelled_tool_call_result;

#[cfg(test)]
use rig_core::completion::ToolDefinition;

#[cfg(test)]
use crate::{contract::ToolPermission, tools::Definition};

pub(super) const RIG_PROVIDER_PAYLOAD_SCHEMA: &str = "horizon.rig.provider_payload";
pub(super) const RIG_PROVIDER_PAYLOAD_VERSION: u32 = 1;

#[cfg(test)]
pub(super) fn horizon_events_from_rig_message(message: Message) -> Vec<Event> {
    horizon_provider_events_from_rig_message(message)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

pub(super) fn horizon_provider_events_from_rig_message(message: Message) -> Vec<ProviderEvent> {
    match message {
        Message::System { content } => vec![Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: format!("system: {content}"),
        })
        .into()],
        Message::User { content } => content
            .into_iter()
            .filter_map(|content| match content {
                rig_core::completion::message::UserContent::Text(text) => Some(
                    Event::MessageCommitted(AgentMessage {
                        role: MessageRole::User,
                        text: text.text,
                    })
                    .into(),
                ),
                _ => None,
            })
            .collect(),
        Message::Assistant { content, .. } => {
            let mut reasoning_events = Vec::new();
            let mut output_events = Vec::new();

            for content in content {
                match content {
                    AssistantContent::Text(text) => output_events.push(
                        Event::MessageCommitted(AgentMessage {
                            role: MessageRole::Assistant,
                            text: text.text,
                        })
                        .into(),
                    ),
                    AssistantContent::ToolCall(call) => {
                        output_events.push(ProviderEvent::with_provider_payload(
                            Event::ToolCallRequested(rig_tool_call_request(call.clone())),
                            rig_tool_call_provider_payload(&call),
                        ));
                    }
                    AssistantContent::Reasoning(reasoning) => {
                        let text = reasoning.display_text();
                        if !text.is_empty() {
                            reasoning_events.push(
                                Event::ReasoningDelta(MessageDelta {
                                    role: MessageRole::Assistant,
                                    text,
                                })
                                .into(),
                            );
                        }
                    }
                    AssistantContent::Image(_) => {}
                }
            }

            reasoning_events.extend(output_events);
            reasoning_events
        }
    }
}

#[cfg(test)]
pub(super) fn horizon_tool_definition_from_rig(
    definition: ToolDefinition,
    permission: ToolPermission,
) -> Definition {
    Definition {
        id: definition.name,
        title: definition.description.clone(),
        description: definition.description,
        input_schema: definition.parameters,
        permission,
    }
}

pub(super) fn rig_messages_from_horizon_events(events: &[Event]) -> Vec<Message> {
    let messages = events
        .iter()
        .filter_map(|event| match event {
            Event::MessageCommitted(message) => Some(match message.role.provider_side() {
                ProviderSide::User => Message::user(message.text.clone()),
                ProviderSide::Assistant => Message::assistant(message.text.clone()),
            }),
            Event::ToolCallRequested(request) => {
                Some(Message::from(rig_tool_call_from_request(request)))
            }
            Event::ToolCallFinished(result) => Some(rig_tool_result_message(result)),
            Event::Error(error) => Some(Message::assistant(format!("error: {}", error.message))),
            Event::StateChanged(_)
            | Event::ReasoningDelta(_)
            | Event::AssistantTextDelta(_)
            | Event::ToolCallStarted(_)
            | Event::ApprovalRequested(_)
            // Operator-intervention audit events (`SESSION_PROTOCOL_VERSION`
            // v16): deliberately do not contribute to the provider's view
            // of history -- the resolved approval is already represented
            // by the `ToolCallStarted`/`ToolCallFinished` that follow, and
            // the continue-turn by the *next* turn's events. Including the
            // audit row itself would invent an operator message the model
            // never received live.
            | Event::ApprovalResolved(_)
            | Event::ContinueTurnRequested(_)
            | Event::ProviderRequestSent(_)
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::ProviderRequestUsage(_)
            // Tier 1 clearing is a projection, never a rewrite of canonical
            // history: a resumed session reloads the full tool results here
            // exactly as an uninterrupted one holds them in memory, and the
            // cleared set is replayed separately
            // (`clearing::cleared_call_ids_from_events`) so the *provider
            // view* comes out identical either way.
            | Event::HistoryCleared(_)
            | Event::Exited(_)
            | Event::TurnEnded(_)
            | Event::Unknown => None,
        })
        .collect();
    repair_replayed_message_pairing(messages)
}

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
///    and `horizon-sessiond`'s startup fixup already use, inserted where the
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
pub(super) fn repair_replayed_message_pairing(messages: Vec<Message>) -> Vec<Message> {
    // Which announced calls ever get an answer -- needed up front, because
    // repair 3 has to distinguish "no result yet" from "no result at all"
    // while walking the sequence forwards.
    let mut seen: HashSet<String> = HashSet::new();
    let mut answered: HashSet<String> = HashSet::new();
    for message in &messages {
        match answered_tool_call_id(message) {
            Some(call_id) if seen.contains(call_id) => {
                answered.insert(call_id.to_string());
            }
            Some(_) => {}
            None => seen.extend(announced_tool_call_ids(message)),
        }
    }

    let mut announced: HashSet<String> = HashSet::new();
    let mut unanswered: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut synthesized: Vec<String> = Vec::new();
    let mut repaired: Vec<Message> = Vec::with_capacity(messages.len());

    for message in messages {
        if let Some(call_id) = answered_tool_call_id(&message) {
            if announced.contains(call_id) {
                repaired.push(message);
            } else {
                dropped.push(call_id.to_string());
            }
            continue;
        }

        match message {
            Message::Assistant { id, content } => {
                let call_ids = announced_tool_call_ids_in(&content);
                let mut pending = Some((id, content));
                if let Some(Message::Assistant {
                    id: run_id,
                    content: run_content,
                }) = repaired.last_mut()
                {
                    // Still the same provider response, split across events.
                    if let Some((id, content)) = pending.take() {
                        if run_id.is_none() {
                            *run_id = id;
                        }
                        for item in content {
                            run_content.push(item);
                        }
                    }
                }
                if let Some((id, content)) = pending {
                    close_unanswered_calls(&mut repaired, &mut unanswered, &mut synthesized);
                    repaired.push(Message::Assistant { id, content });
                }
                for call_id in call_ids {
                    if !answered.contains(&call_id) {
                        unanswered.push(call_id.clone());
                    }
                    announced.insert(call_id);
                }
            }
            other => {
                close_unanswered_calls(&mut repaired, &mut unanswered, &mut synthesized);
                repaired.push(other);
            }
        }
    }
    close_unanswered_calls(&mut repaired, &mut unanswered, &mut synthesized);

    // Diagnosable on sessiond's stderr, the channel `horizon-sessiond`'s own
    // resume fixups already report through: a repair that fired silently
    // would leave the next incident with nothing to read.
    if !dropped.is_empty() {
        eprintln!(
            "horizon-agent: dropped {} orphaned tool result(s) while rebuilding provider \
             history (no assistant message announced them): {}",
            dropped.len(),
            dropped.join(", ")
        );
    }
    if !synthesized.is_empty() {
        eprintln!(
            "horizon-agent: closed {} unanswered tool call(s) with a cancelled result while \
             rebuilding provider history: {}",
            synthesized.len(),
            synthesized.join(", ")
        );
    }

    repaired
}

/// Appends one cancelled tool result per call that will never be answered,
/// at the point the real results would have sat.
fn close_unanswered_calls(
    repaired: &mut Vec<Message>,
    unanswered: &mut Vec<String>,
    synthesized: &mut Vec<String>,
) {
    for call_id in unanswered.drain(..) {
        repaired.push(rig_tool_result_message(&cancelled_tool_call_result(
            ToolCallId(call_id.clone()),
        )));
        synthesized.push(call_id);
    }
}

/// Every tool-call id an assistant message announces; empty for any other
/// message.
fn announced_tool_call_ids(message: &Message) -> Vec<String> {
    match message {
        Message::Assistant { content, .. } => announced_tool_call_ids_in(content),
        _ => Vec::new(),
    }
}

fn announced_tool_call_ids_in(content: &OneOrMany<AssistantContent>) -> Vec<String> {
    content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::ToolCall(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect()
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
    match content.first_ref() {
        UserContent::ToolResult(result) => Some(result.id.as_str()),
        _ => None,
    }
}

pub(super) fn rig_tool_call_request(call: ToolCall) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId(call.call_id.unwrap_or(call.id)),
        tool_id: call.function.name,
        input: call.function.arguments.into(),
        // Mint a fresh `OccurrenceId` here -- the upstream provider only
        // knows its own `call_id`, which it can reuse across genuinely
        // distinct calls (the `functions.fs.edit:66` incident in session
        // 05254b6a) and which Horizon also reuses on every sandbox-denial
        // retry. `OccurrenceId` is the per-occurrence second key the
        // transcript, approval, and analytics each follow; a UUID v4 is
        // globally unique without coordination across resumed sessions
        // and replayed logs, which a per-process counter would not be.
        occurrence_id: Some(OccurrenceId::new()),
    }
}

pub(super) fn rig_tool_call_provider_payload(call: &ToolCall) -> serde_json::Value {
    serde_json::json!({
        "schema": RIG_PROVIDER_PAYLOAD_SCHEMA,
        "version": RIG_PROVIDER_PAYLOAD_VERSION,
        "rig": {
            "tool_call": {
                "id": call.id.clone(),
                "call_id": call.call_id.clone(),
                "signature": call.signature.clone(),
                "additional_params": call.additional_params.clone(),
                "function": {
                    "name": call.function.name.clone(),
                    "arguments": call.function.arguments.clone(),
                }
            }
        }
    })
}

/// Rebuilds a rig tool call from a persisted `ToolCallRequested` event —
/// the history-reload path, so the arguments get the same normalization a
/// live turn applies (see `completion::replay_safe_tool_arguments`).
/// Without it, a session recorded before that repair existed would
/// re-poison itself the first time its history was reloaded from the
/// projection.
fn rig_tool_call_from_request(request: &ToolCallRequest) -> ToolCall {
    let mut arguments = request.input.0.clone();
    super::completion::replay_safe_tool_arguments(&mut arguments);
    ToolCall::new(
        request.call_id.0.clone(),
        ToolFunction::new(request.tool_id.clone(), arguments),
    )
}

pub(super) fn rig_tool_result_message(result: &ToolCallResult) -> Message {
    Message::tool_result(result.call_id.0.clone(), result.output.to_string())
}

pub(super) fn rig_workspace_snapshot_call() -> ToolCall {
    ToolCall::new(
        "rig-workspace-snapshot-1".to_string(),
        ToolFunction::new("workspace.snapshot".to_string(), serde_json::json!({})),
    )
}

/// `count` distinct `workspace.snapshot` calls (a fresh call id and an
/// index in the arguments per call, so each has its own doom-loop
/// fingerprint) — the deterministic fallback's hook for exercising a
/// parallel-tool-call batch (e.g. `deterministic_rig_response`'s "multi
/// tool" trigger) without a network provider. Mirrors the shape of a real
/// completion that requests several tool calls at once (the production
/// incident this covers: a MiniMax completion routinely requesting 4
/// parallel `fs.read`s).
pub(super) fn rig_multi_snapshot_calls(count: usize) -> Vec<ToolCall> {
    (1..=count)
        .map(|index| {
            ToolCall::new(
                format!("rig-multi-snapshot-{index}"),
                ToolFunction::new(
                    "workspace.snapshot".to_string(),
                    serde_json::json!({ "n": index }),
                ),
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) fn rig_workspace_snapshot_call_with_provider_metadata() -> ToolCall {
    ToolCall {
        call_id: Some("provider-call-1".to_string()),
        signature: Some("signature-1".to_string()),
        additional_params: Some(serde_json::json!({
            "provider": "rig",
            "reasoning_ref": "reasoning-1"
        })),
        ..rig_workspace_snapshot_call()
    }
}
