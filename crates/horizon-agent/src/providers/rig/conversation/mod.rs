//! One conversation model for live decisions and persisted replay.
//! Response batches own their calls and results; flattening is an adapter step.
mod projection;
#[cfg(test)]
mod test_support;
pub(crate) mod upgrade;
use std::collections::{HashMap, HashSet};
#[cfg(test)]
pub(crate) use test_support::{announcement, fixture};

use crossbeam_channel::Sender;
use rig_core::completion::{message::ToolCall, AssistantContent, Message};

use crate::contract::{
    ConversationInputKind, ConversationRecord, Event, OccurrenceId, ProviderEvent, ToolCallId,
    ToolCallIdentity, ToolCallResult, ToolOutcome,
};

pub(super) const MESSAGE_CODEC: u32 = 1;

#[derive(Clone, Debug)]
pub(crate) enum Prompt {
    Current,
    Input {
        kind: ConversationInputKind,
        text: String,
    },
    Result {
        result: ToolCallResult,
        tool_id: String,
    },
}
impl Prompt {
    pub(super) fn input(kind: ConversationInputKind, text: impl Into<String>) -> Self {
        Self::Input {
            kind,
            text: text.into(),
        }
    }
    pub(super) fn result(result: &ToolCallResult, tool_id: &str) -> Self {
        Self::Result {
            result: result.clone(),
            tool_id: tool_id.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConversationHistory {
    entries: Vec<Entry>,
    response_indices: HashMap<String, usize>,
    occurrences: HashMap<OccurrenceId, (usize, usize)>,
    turn: usize,
    retired: HashSet<OccurrenceId>,
}
#[derive(Clone, Debug)]
struct Entry {
    turn: usize,
    content: Content,
}
#[derive(Clone, Debug)]
enum Content {
    Input {
        kind: ConversationInputKind,
        text: String,
    },
    Response(Response),
}
#[derive(Clone, Debug)]
struct Response {
    id: String,
    message: Option<Message>,
    completed: bool,
    reasoning: Vec<rig_core::completion::message::Reasoning>,
    calls: Vec<Call>,
}
#[derive(Clone, Debug)]
struct Call {
    identity: ToolCallIdentity,
    tool: ToolCall,
    result: Option<ToolCallResult>,
    active: OccurrenceId,
}

pub(super) struct ResultSite<'a> {
    pub(super) result: &'a ToolCallResult,
    pub(super) tool: &'a ToolCall,
    pub(super) message_index: usize,
    pub(super) current_response: bool,
}

impl ConversationHistory {
    pub(super) fn from_events(events: &[Event]) -> Result<Self, String> {
        let mut history = Self::default();
        let mut recorded_inputs = 0;
        let mut displayed_inputs = 0;
        for event in events {
            if matches!(
                event,
                Event::ConversationRecorded(ConversationRecord::Input {
                    kind: ConversationInputKind::User,
                    ..
                })
            ) {
                recorded_inputs += 1;
            }
            if matches!(event, Event::MessageCommitted(message) if message.role == crate::contract::MessageRole::User)
            {
                displayed_inputs += 1;
            }
            if displayed_inputs > recorded_inputs {
                return Err(
                    "Missing canonical owner input; convert legacy history before resuming".into(),
                );
            }
            history.apply_event(event)?;
        }
        if history.is_empty() && events.iter().any(|event| matches!(event,
            Event::MessageCommitted(message) if message.role == crate::contract::MessageRole::User
        ) || matches!(event, Event::ToolCallRequested(_))) {
            return Err("Missing canonical conversation records; convert legacy history before resuming".into());
        }
        Ok(history)
    }

    pub(super) fn apply_event(&mut self, event: &Event) -> Result<(), String> {
        match event {
            Event::ConversationRecorded(record) => self.apply(record),
            Event::ToolCallRequested(request) => {
                // Denial retries are execution attempts of the same provider call.
                if !self.occurrences.contains_key(&request.occurrence_id) {
                    let location = self
                        .pending(&request.call_id)
                        .ok_or("Tool dispatch lacks its canonical announcement")?;
                    let (entry, call) = location;
                    if self.response(entry).calls[call].tool.function.name != request.tool_id {
                        return Err("Retry tool differs from the canonical call".into());
                    }
                    self.occurrences
                        .insert(request.occurrence_id.clone(), location);
                }
                Ok(())
            }
            Event::ToolCallFinished(result) => {
                if let Some(location) = self.occurrences.get(&result.occurrence_id).copied() {
                    self.accept_result(location, result)?;
                }
                Ok(())
            }
            Event::TurnEnded(crate::contract::TurnEndReason::Completed) => {
                if let Some(Entry {
                    content: Content::Response(response),
                    ..
                }) = self.entries.last_mut()
                {
                    response.completed = true;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(super) fn open_turn(&mut self, events: &Sender<ProviderEvent>) {
        self.record(ConversationRecord::TurnOpened, events)
            .expect("turn opening is valid");
    }

    pub(super) fn append_prompt(
        &mut self,
        prompt: Prompt,
        events: &Sender<ProviderEvent>,
    ) -> Result<(), String> {
        match prompt {
            Prompt::Current => Ok(()),
            Prompt::Input { kind, text } => {
                self.record(ConversationRecord::Input { kind, text }, events)
            }
            Prompt::Result { result, tool_id } => self.append_result(&result, &tool_id),
        }
    }

    pub(super) fn append_result(
        &mut self,
        result: &ToolCallResult,
        tool_id: &str,
    ) -> Result<(), String> {
        let location = self
            .occurrences
            .get(&result.occurrence_id)
            .copied()
            .or_else(|| self.pending(&result.call_id))
            .ok_or_else(|| format!("No pending conversation call for {}", result.call_id.0))?;
        let (entry, call) = location;
        if self.response(entry).calls[call].tool.function.name != tool_id {
            return Err("Tool result name disagrees with its conversation call".into());
        }
        self.occurrences
            .insert(result.occurrence_id.clone(), location);
        self.accept_result(location, result)
    }

    fn accept_result(
        &mut self,
        location: (usize, usize),
        result: &ToolCallResult,
    ) -> Result<(), String> {
        if self.retired.contains(&result.occurrence_id) {
            return Ok(());
        }
        let (entry, call) = location;
        if self.response(entry).calls[call].identity.call_id != result.call_id {
            return Err("Tool result identity disagrees with its conversation call".into());
        }
        if let ToolOutcome::Superseded {
            retry_occurrence_id,
        } = &result.outcome
        {
            self.retired.insert(result.occurrence_id.clone());
            self.response_mut(entry).calls[call].active = retry_occurrence_id.clone();
            self.occurrences
                .insert(retry_occurrence_id.clone(), location);
            return Ok(());
        }
        let slot = &mut self.response_mut(entry).calls[call].result;
        if let Some(previous) = slot {
            if previous != result {
                return Err("Conflicting conversation tool results".into());
            }
        } else {
            *slot = Some(result.clone());
        }
        Ok(())
    }

    pub(super) fn record_response(
        &mut self,
        response_id: String,
        message: &Message,
        calls: Vec<ToolCallIdentity>,
        events: &Sender<ProviderEvent>,
    ) -> Result<(), String> {
        self.record(
            ConversationRecord::Response {
                response_id,
                codec: MESSAGE_CODEC,
                message: serde_json::to_value(message)
                    .expect("Rig messages serialize")
                    .into(),
                calls,
            },
            events,
        )
    }

    fn record(
        &mut self,
        record: ConversationRecord,
        events: &Sender<ProviderEvent>,
    ) -> Result<(), String> {
        self.apply(&record)?;
        let _ = events.send(Event::ConversationRecorded(record).into());
        Ok(())
    }

    fn apply(&mut self, record: &ConversationRecord) -> Result<(), String> {
        if let ConversationRecord::Response { response_id, .. }
        | ConversationRecord::ToolAnnounced { response_id, .. } = record
        {
            if response_id.is_empty() {
                return Err("Empty conversation response identity".into());
            }
            if let Some(&index) = self.response_indices.get(response_id) {
                if self.entries[index].turn != self.turn || index + 1 != self.entries.len() {
                    return Err(
                        "Conversation response identity belongs to an earlier response".into(),
                    );
                }
            }
        }
        match record {
            ConversationRecord::TurnOpened => self.turn += 1,
            ConversationRecord::Input { kind, text } => self.entries.push(Entry {
                turn: self.turn,
                content: Content::Input {
                    kind: *kind,
                    text: text.clone(),
                },
            }),
            ConversationRecord::ToolAnnounced {
                response_id,
                identity,
                codec,
                tool_call,
                reasoning,
            } => {
                check_codec(*codec)?;
                let tool: ToolCall =
                    serde_json::from_value(tool_call.0.clone()).map_err(|e| e.to_string())?;
                if provider_call_id(&tool) != identity.call_id.0 {
                    return Err("Announced tool identity mismatch".into());
                }
                let reasoning =
                    serde_json::from_value(reasoning.0.clone()).map_err(|e| e.to_string())?;
                if let Some(&(entry, call)) = self.occurrences.get(&identity.occurrence_id) {
                    if self.response(entry).id != *response_id
                        || self.response(entry).calls[call].identity != *identity
                    {
                        return Err("Reused execution identity in conversation".into());
                    }
                }
                if let Some(&entry) = self.response_indices.get(response_id) {
                    if self.response(entry).calls.iter().any(|call| {
                        call.identity.call_id == identity.call_id && call.identity != *identity
                    }) {
                        return Err("Duplicate provider call ID within one response".into());
                    }
                }
                let entry = self.ensure_response(response_id);
                self.add_call(entry, identity, tool)?;
                self.response_mut(entry).reasoning = reasoning;
            }
            ConversationRecord::Response {
                response_id,
                codec,
                message,
                calls,
            } => {
                check_codec(*codec)?;
                let message: Message =
                    serde_json::from_value(message.0.clone()).map_err(|e| e.to_string())?;
                let Message::Assistant { content, .. } = &message else {
                    return Err("Response must have assistant role".into());
                };
                let tools: Vec<_> = content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(tool) => Some(tool.clone()),
                        _ => None,
                    })
                    .collect();
                if tools.len() != calls.len()
                    || tools
                        .iter()
                        .zip(calls)
                        .any(|(tool, identity)| provider_call_id(tool) != identity.call_id.0)
                {
                    return Err("Response tool identities disagree with content".into());
                }
                let mut occurrences = HashSet::new();
                let mut provider_ids = HashSet::new();
                for identity in calls {
                    if !occurrences.insert(&identity.occurrence_id)
                        || !provider_ids.insert(&identity.call_id)
                    {
                        return Err("Duplicate response tool identity".into());
                    }
                    if let Some(&(entry, call)) = self.occurrences.get(&identity.occurrence_id) {
                        if self.response(entry).id != *response_id
                            || self.response(entry).calls[call].identity != *identity
                        {
                            return Err("Reused execution identity in conversation".into());
                        }
                    }
                }
                let entry = self.ensure_response(response_id);
                if self
                    .response(entry)
                    .calls
                    .iter()
                    .enumerate()
                    .any(|(i, call)| calls.get(i) != Some(&call.identity))
                {
                    return Err("Response changed an already announced tool".into());
                }
                for (tool, identity) in tools.into_iter().zip(calls) {
                    self.add_call(entry, identity, tool)?;
                }
                self.response_mut(entry).message = Some(message);
            }
        }
        Ok(())
    }

    fn ensure_response(&mut self, id: &str) -> usize {
        if let Some(index) = self.response_indices.get(id) {
            return *index;
        }
        let index = self.entries.len();
        self.entries.push(Entry {
            turn: self.turn,
            content: Content::Response(Response {
                id: id.into(),
                message: None,
                completed: false,
                reasoning: Vec::new(),
                calls: Vec::new(),
            }),
        });
        self.response_indices.insert(id.into(), index);
        index
    }
    fn add_call(
        &mut self,
        entry: usize,
        identity: &ToolCallIdentity,
        tool: ToolCall,
    ) -> Result<(), String> {
        if let Some(&(prior, call)) = self.occurrences.get(&identity.occurrence_id) {
            if prior != entry || self.response(prior).calls[call].identity != *identity {
                return Err("Reused execution identity in conversation".into());
            }
            self.response_mut(prior).calls[call].tool = tool;
            return Ok(());
        }
        if self
            .response(entry)
            .calls
            .iter()
            .any(|call| call.identity.call_id == identity.call_id)
        {
            return Err("Duplicate provider call ID within one response".into());
        }
        let call = self.response(entry).calls.len();
        self.response_mut(entry).calls.push(Call {
            identity: identity.clone(),
            active: identity.occurrence_id.clone(),
            tool,
            result: None,
        });
        self.occurrences
            .insert(identity.occurrence_id.clone(), (entry, call));
        Ok(())
    }
    fn response(&self, index: usize) -> &Response {
        let Content::Response(response) = &self.entries[index].content else {
            unreachable!()
        };
        response
    }
    fn response_mut(&mut self, index: usize) -> &mut Response {
        let Content::Response(response) = &mut self.entries[index].content else {
            unreachable!()
        };
        response
    }
    fn pending(&self, id: &ToolCallId) -> Option<(usize, usize)> {
        self.entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(entry, item)| {
                let Content::Response(response) = &item.content else {
                    return None;
                };
                response
                    .calls
                    .iter()
                    .position(|call| call.identity.call_id == *id && call.result.is_none())
                    .map(|call| (entry, call))
            })
    }

    fn pending_identities(&self) -> Vec<ToolCallIdentity> {
        self.entries
            .iter()
            .flat_map(|entry| match &entry.content {
                Content::Input { .. } => Vec::new(),
                Content::Response(response) => response
                    .calls
                    .iter()
                    .filter(|call| call.result.is_none())
                    .map(|call| ToolCallIdentity {
                        call_id: call.identity.call_id.clone(),
                        occurrence_id: call.active.clone(),
                    })
                    .collect(),
            })
            .collect()
    }

    pub(super) fn validate_ready(&self) -> Result<(), String> {
        if self.entries.iter().any(|entry| {
            matches!(&entry.content,
            Content::Response(response) if response.calls.iter().any(|call| call.result.is_none()))
        }) {
            return Err(
                "Conversation contains unsettled tool calls; restore through the session host"
                    .into(),
            );
        }
        Ok(())
    }
}

fn check_codec(codec: u32) -> Result<(), String> {
    if codec == MESSAGE_CODEC {
        Ok(())
    } else {
        Err(format!("Unsupported conversation message codec {codec}"))
    }
}
fn provider_call_id(tool: &ToolCall) -> &str {
    tool.provider
        .as_ref()
        .map(|p| p.call_id.as_str())
        .unwrap_or_else(|| tool.id.as_str())
}

/// Pending canonical calls can include an announcement persisted immediately
/// before a crash, before the corresponding host dispatch event was written.
pub fn interrupted_conversation_calls(events: &[Event]) -> Result<Vec<ToolCallIdentity>, String> {
    if !events
        .iter()
        .any(|event| matches!(event, Event::ConversationRecorded(_)))
    {
        return Ok(Vec::new());
    }
    let history = ConversationHistory::from_events(events)?;
    Ok(history.pending_identities())
}

#[cfg(test)]
mod tests;
