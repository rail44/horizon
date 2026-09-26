//! Explicit offline v3 conversion. Never called by session restoration.
use super::*;
use crate::contract::{MessageRole, ToolCallRequest};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default)]
struct Session {
    history: ConversationHistory,
    response_id: Option<LegacyResponse>,
    opened: bool,
    turn_ended: bool,
    cleared: HashSet<OccurrenceId>,
}

enum LegacyResponse {
    Explicit(String),
    Inferred(String),
}
impl LegacyResponse {
    fn id(&self) -> &str {
        match self {
            Self::Explicit(id) | Self::Inferred(id) => id,
        }
    }
}

/// Upgrade a stopped v3 log in memory. Existing v4 data is returned unchanged.
/// Original record identities and authority metadata survive; inserted records
/// receive fresh IDs and the output's sequence is assigned in source order.
pub fn upgrade_conversation_records(records: &[Value]) -> Result<Vec<Value>, String> {
    if records.iter().all(|row| row["version"] == 4) {
        validate_conversation_records(records)?;
        return Ok(records.to_vec());
    }
    if records.iter().any(|row| row["version"] != 3) {
        return Err(
            "Expected one v3 log; first convert v1/v2 with migrate-agent-history.py".into(),
        );
    }
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut retries = HashSet::new();
    for row in records {
        let event = &row["event"];
        if let Some(id) = event
            .pointer("/ToolCallFinished/outcome/Superseded/retry_occurrence_id")
            .and_then(Value::as_str)
        {
            retries.insert(id.to_owned());
        }
        if let Some(approval) = event.get("ApprovalRequested") {
            if approval["kind"].as_object().is_some_and(|kind| {
                kind.values()
                    .any(|value| value.get("prior_result").is_some())
            }) {
                if let Some(id) = approval["occurrence_id"].as_str() {
                    retries.insert(id.to_owned());
                }
            }
        }
    }
    let mut output = Vec::new();
    let mut ids = HashSet::new();
    let mut sequences = HashSet::new();
    for source in records {
        let source_id = source["event_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("Missing event identity")?;
        if !ids.insert(source_id)
            || !sequences.insert(source["sequence"].as_u64().ok_or("Invalid sequence")?)
        {
            return Err("Duplicate source event identity or sequence".into());
        }
        let session_id = source["session_id"]
            .as_str()
            .ok_or("Missing session identity")?;
        let session = sessions.entry(session_id.into()).or_default();
        let mut row = source.clone();
        row["version"] = 4.into();
        if let Some(cleared) = row["event"].get_mut("HistoryCleared") {
            convert_clearing(session, cleared)?;
        }
        let event: Event = serde_json::from_value(row["event"].clone())
            .map_err(|error| format!("Event {source_id}: {error}"))?;
        // Validate all envelope fields with the current reader's actual type.
        let _: crate::persistence::event_log::Record =
            serde_json::from_value(row.clone()).map_err(|error| error.to_string())?;
        if matches!(&event, Event::MessageCommitted(message) if message.role == MessageRole::User)
            || matches!(event, Event::TurnEnded(_))
        {
            close_pending(session, &row, &mut output)?;
        }
        let extra = conversation_changes(session, &event, source, &retries)?;
        for record in extra {
            let event = Event::ConversationRecorded(record);
            session
                .history
                .apply_event(&event)
                .map_err(|error| format!("Session {session_id}, event {source_id}: {error}"))?;
            let mut inserted = row.clone();
            inserted["event_id"] = uuid::Uuid::new_v4().to_string().into();
            inserted["event_kind"] = "conversation_recorded".into();
            inserted["provider_payload"] = Value::Null;
            inserted["event"] = serde_json::to_value(event).unwrap();
            output.push(inserted);
        }
        session
            .history
            .apply_event(&event)
            .map_err(|error| format!("Session {session_id}, event {source_id}: {error}"))?;
        output.push(row);
    }
    for (id, session) in &mut sessions {
        let template = records
            .iter()
            .rev()
            .find(|row| row["session_id"].as_str() == Some(id))
            .unwrap();
        close_pending(session, template, &mut output)?;
    }
    for (index, row) in output.iter_mut().enumerate() {
        row["sequence"] = (index as u64 + 1).into();
    }
    validate_conversation_records(&output)?;
    Ok(output)
}
fn close_pending(
    session: &mut Session,
    template: &Value,
    output: &mut Vec<Value>,
) -> Result<(), String> {
    let pending = session.history.pending_identities();
    for identity in pending {
        let event = Event::ToolCallFinished(ToolCallResult::cancelled(identity));
        session.history.apply_event(&event)?;
        let mut row = template.clone();
        row["version"] = 4.into();
        row["event_id"] = uuid::Uuid::new_v4().to_string().into();
        row["event_kind"] = crate::contract::event_kind(&event).into();
        row["provider_payload"] = Value::Null;
        row["event"] = serde_json::to_value(event).unwrap();
        output.push(row);
    }
    Ok(())
}

fn restore_tool(request: &ToolCallRequest, payload: Option<&Value>) -> Result<ToolCall, String> {
    let mut tool = ToolCall::new(
        rig_core::message::ToolCallId::new_or_mint(request.call_id.0.clone()),
        rig_core::completion::message::ToolFunction::new(
            request.tool_id.clone(),
            request.input.0.clone(),
        ),
    );
    if let Some(raw) = payload.and_then(|p| p.pointer("/rig/tool_call")) {
        if let Some(id) = raw["id"].as_str() {
            tool.id = rig_core::message::ToolCallId::new_or_mint(id);
        }
        tool.provider = raw["call_id"]
            .as_str()
            .and_then(rig_core::completion::message::ProviderCallId::new);
        tool.signature = raw["signature"].as_str().map(str::to_owned);
        tool.additional_params = raw
            .get("additional_params")
            .filter(|value| !value.is_null())
            .cloned();
    }
    if provider_call_id(&tool) != request.call_id.0 {
        return Err("Provider payload disagrees with the recorded call identity".into());
    }
    super::super::completion::replay_safe_tool_arguments(&mut tool.function.arguments);
    Ok(tool)
}

#[cfg(test)]
pub(crate) fn fixture(events: Vec<Event>) -> Vec<Event> {
    let session_id = crate::contract::SessionId::new();
    let rows: Vec<_> = events.into_iter().enumerate().map(|(index, event)| serde_json::json!({
        "schema": "horizon.agent.event_log", "version": 3, "event_id": format!("fixture-{index}"),
        "sequence": index + 1, "session_id": session_id, "turn_id": "fixture-turn",
        "provider_id": "builtin.agent.rig", "role_id": null, "session_context": null,
        "event_kind": crate::contract::event_kind(&event), "event": event, "provider_payload": null, "created_at_unix_ms": 0
    })).collect();
    upgrade_conversation_records(&rows)
        .unwrap()
        .into_iter()
        .map(|row| serde_json::from_value(row["event"].clone()).unwrap())
        .collect()
}

/// Validate the exact decoder, event envelopes, conversation codec and identities.
pub fn validate_conversation_records(rows: &[Value]) -> Result<(), String> {
    let mut sessions: HashMap<crate::contract::SessionId, (bool, Vec<Event>)> = HashMap::new();
    let mut ids = HashSet::new();
    let mut previous = None;
    for row in rows {
        let record: crate::persistence::event_log::Record =
            serde_json::from_value(row.clone()).map_err(|error| error.to_string())?;
        if record.schema != "horizon.agent.event_log"
            || record.version != 4
            || record.event_id.is_empty()
            || !ids.insert(record.event_id)
            || previous.is_some_and(|prior| record.sequence <= prior)
        {
            return Err("Invalid event envelope, duplicate identity or unordered sequence".into());
        }
        previous = Some(record.sequence);
        let session = sessions.entry(record.session_id).or_default();
        session.0 |= record
            .provider_id
            .as_ref()
            .is_some_and(|id| id.0 == "builtin.agent.rig")
            || matches!(record.event, Event::ConversationRecorded(_));
        session.1.push(record.event);
    }
    for (session, (rig, events)) in sessions {
        if rig {
            ConversationHistory::from_events(&events)
                .map_err(|error| format!("Session {session:?}: {error}"))?;
        }
    }
    Ok(())
}

fn convert_clearing(session: &mut Session, cleared: &mut Value) -> Result<(), String> {
    let old = cleared
        .as_object_mut()
        .ok_or("Invalid HistoryCleared record")?
        .remove("cleared_call_ids")
        .ok_or("Missing v3 clearing IDs")?;
    let mut exact = Vec::new();
    for id in old.as_array().ok_or("Invalid clearing IDs")? {
        let call = id.as_str().ok_or("Invalid clearing ID")?;
        let site = session
            .history
            .result_sites()
            .into_iter()
            .find(|site| {
                site.result.call_id.0 == call
                    && !session.cleared.contains(&site.result.occurrence_id)
            })
            .ok_or_else(|| format!("Cannot identify cleared result {call}"))?;
        let occurrence = site.result.occurrence_id.clone();
        session.cleared.insert(occurrence.clone());
        exact.push(occurrence);
    }
    cleared["cleared_occurrence_ids"] = serde_json::to_value(exact).unwrap();
    Ok(())
}

fn conversation_changes(
    session: &mut Session,
    event: &Event,
    source: &Value,
    retries: &HashSet<String>,
) -> Result<Vec<ConversationRecord>, String> {
    let source_id = source["event_id"]
        .as_str()
        .ok_or("Missing event identity")?;
    let mut extra = Vec::new();
    match event {
        Event::MessageCommitted(message)
            if message.role.provider_side() == crate::contract::ProviderSide::User =>
        {
            if message.role == MessageRole::User
                || !session.opened
                || (message.role == MessageRole::TaskNotification && session.turn_ended)
            {
                extra.push(ConversationRecord::TurnOpened);
                session.opened = true;
                session.turn_ended = false;
                session.response_id = None;
            }
            let kind = match message.role {
                MessageRole::User => ConversationInputKind::User,
                MessageRole::TaskNotification => ConversationInputKind::Notification,
                MessageRole::AutoContinue => ConversationInputKind::Continuation,
                MessageRole::Assistant => unreachable!(),
            };
            extra.push(ConversationRecord::Input {
                kind,
                text: message.text.clone(),
            });
        }
        Event::ProviderRequestSent(_) => {
            session.response_id = Some(LegacyResponse::Explicit(source_id.into()))
        }
        Event::ToolCallRequested(request) if !retries.contains(&request.occurrence_id.0) => {
            if session
                .response_id
                .as_ref()
                .filter(|response| matches!(response, LegacyResponse::Inferred(_)))
                .and_then(|response| session.history.response_indices.get(response.id()))
                .is_some_and(|&entry| {
                    let response = session.history.response(entry);
                    response.calls.iter().any(|call| call.result.is_some())
                })
            {
                session.response_id = None;
            }
            let response_id = session
                .response_id
                .get_or_insert_with(|| LegacyResponse::Inferred(source_id.to_owned()))
                .id()
                .to_owned();
            let tool = restore_tool(request, source.get("provider_payload"))?;
            extra.push(ConversationRecord::ToolAnnounced {
                response_id,
                identity: request.identity(),
                codec: MESSAGE_CODEC,
                tool_call: serde_json::to_value(tool).unwrap().into(),
                reasoning: serde_json::json!([]).into(),
            });
        }
        Event::MessageCommitted(message) if session.opened || !session.history.is_empty() => {
            let response_id = session
                .response_id
                .get_or_insert_with(|| LegacyResponse::Inferred(source_id.to_owned()))
                .id()
                .to_owned();
            let mut content = Vec::new();
            let mut calls = Vec::new();
            if let Some(&entry) = session.history.response_indices.get(&response_id) {
                for call in &session.history.response(entry).calls {
                    content.push(AssistantContent::ToolCall(call.tool.clone()));
                    calls.push(call.identity.clone());
                }
            }
            content.push(AssistantContent::text(&message.text));
            let message = Message::Assistant { id: None, content };
            extra.push(ConversationRecord::Response {
                response_id,
                codec: MESSAGE_CODEC,
                message: serde_json::to_value(message).unwrap().into(),
                calls,
            });
        }
        Event::TurnEnded(_) => {
            session.turn_ended = true;
            session.response_id = None;
        }
        _ => {}
    }
    Ok(extra)
}
