use crate::agent::contract::{Message, MessageRole, SessionState};
use crate::agent::frame::{AgentFrame, AgentFrameItem};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct TranscriptBlock {
    pub(super) id: usize,
    pub(super) label: Option<&'static str>,
    pub(super) text: String,
    pub(super) tone: TranscriptTone,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TranscriptTone {
    User,
    Assistant,
    Thinking,
    Status,
    Tool,
    Approval,
    Error,
    Lifecycle,
}

pub(super) fn transcript_blocks(frame: &AgentFrame) -> Vec<TranscriptBlock> {
    let mut blocks = frame
        .items
        .iter()
        .enumerate()
        .filter_map(|(id, item)| transcript_block(id, item))
        .collect::<Vec<_>>();
    if let Some(status) = status_block(frame.state, blocks.len(), blocks.last()) {
        blocks.push(status);
    }
    blocks
}

pub(super) fn current_block_text(
    frame: &AgentFrame,
    id: usize,
    tone: TranscriptTone,
    label: Option<&'static str>,
) -> String {
    let block = frame
        .items
        .get(id)
        .and_then(|item| transcript_block(id, item))
        .or_else(|| {
            let blocks = frame
                .items
                .iter()
                .enumerate()
                .filter_map(|(id, item)| transcript_block(id, item))
                .collect::<Vec<_>>();
            status_block(frame.state, blocks.len(), blocks.last())
        });

    block
        .filter(|block| block.id == id && block.tone == tone && block.label == label)
        .map(|block| block.text)
        .unwrap_or_default()
}

pub(super) fn transcript_revision(frame: &AgentFrame) -> usize {
    let state = usize::from(frame.state.is_some());
    frame
        .items
        .iter()
        .map(|item| match item {
            AgentFrameItem::Message(message) => message.text.len(),
            AgentFrameItem::ReasoningDelta(delta) => delta.text.len(),
            AgentFrameItem::AssistantTextDelta(delta) => delta.text.len(),
            AgentFrameItem::ToolCallRequested(request) => request.tool_id.len(),
            AgentFrameItem::ToolCallStarted(call_id) => call_id.0.len(),
            AgentFrameItem::ToolCallFinished(result) => result.call_id.0.len(),
            AgentFrameItem::ToolCallPreparing(progress) => progress.bytes,
            AgentFrameItem::ApprovalRequested(request) => request.reason.len(),
            AgentFrameItem::Error(error) => error.message.len(),
            AgentFrameItem::Exited(exit) => exit.reason.len(),
        })
        .sum::<usize>()
        + frame.items.len()
        + state
}

fn transcript_block(id: usize, item: &AgentFrameItem) -> Option<TranscriptBlock> {
    match item {
        AgentFrameItem::Message(message) => Some(message_block(id, message)),
        AgentFrameItem::ReasoningDelta(delta) => Some(TranscriptBlock {
            id,
            label: Some("thinking"),
            text: delta.text.clone(),
            tone: TranscriptTone::Thinking,
        }),
        AgentFrameItem::AssistantTextDelta(delta) => Some(TranscriptBlock {
            id,
            label: None,
            text: delta.text.clone(),
            tone: TranscriptTone::Assistant,
        }),
        AgentFrameItem::ToolCallRequested(request) => Some(TranscriptBlock {
            id,
            label: Some("tool request"),
            text: format!("{} {}", request.tool_id, request.input),
            tone: TranscriptTone::Tool,
        }),
        AgentFrameItem::ToolCallStarted(call_id) => Some(TranscriptBlock {
            id,
            label: Some("tool running"),
            text: call_id.0.clone(),
            tone: TranscriptTone::Tool,
        }),
        AgentFrameItem::ToolCallFinished(result) => Some(TranscriptBlock {
            id,
            label: Some("tool result"),
            text: tool_result_summary(result.call_id.0.as_str(), &result.output),
            tone: TranscriptTone::Tool,
        }),
        AgentFrameItem::ToolCallPreparing(progress) => Some(TranscriptBlock {
            id,
            label: Some("preparing"),
            text: tool_call_preparing_summary(progress),
            tone: TranscriptTone::Tool,
        }),
        AgentFrameItem::ApprovalRequested(request) => Some(TranscriptBlock {
            id,
            label: Some("approval"),
            text: request.reason.clone(),
            tone: TranscriptTone::Approval,
        }),
        AgentFrameItem::Error(error) => Some(TranscriptBlock {
            id,
            label: Some("error"),
            text: error.message.clone(),
            tone: TranscriptTone::Error,
        }),
        AgentFrameItem::Exited(exit) => Some(TranscriptBlock {
            id,
            label: Some("exited"),
            text: exit.reason.clone(),
            tone: TranscriptTone::Lifecycle,
        }),
    }
}

fn message_block(id: usize, message: &Message) -> TranscriptBlock {
    let tone = match message.role {
        MessageRole::User => TranscriptTone::User,
        MessageRole::Assistant => TranscriptTone::Assistant,
    };

    TranscriptBlock {
        id,
        label: None,
        text: message.text.clone(),
        tone,
    }
}

fn status_block(
    state: Option<SessionState>,
    id: usize,
    last_block: Option<&TranscriptBlock>,
) -> Option<TranscriptBlock> {
    let text = match state? {
        SessionState::Running => {
            if !should_show_initial_reply_status(last_block) {
                return None;
            }
            "Agent is replying..."
        }
        SessionState::ToolRunning => {
            if matches!(
                last_block.map(|block| block.tone),
                Some(TranscriptTone::Tool | TranscriptTone::Assistant | TranscriptTone::Thinking)
            ) {
                return None;
            }
            "Running tool..."
        }
        SessionState::WaitingForApproval => {
            if matches!(
                last_block.map(|block| block.tone),
                Some(TranscriptTone::Approval)
            ) {
                return None;
            }
            "Approval required"
        }
        SessionState::Cancelled => "Turn cancelled",
        SessionState::Failed => "Agent failed",
        SessionState::Terminated => "Agent terminated",
        SessionState::Created | SessionState::WaitingForUser | SessionState::Completed => {
            return None
        }
    };

    Some(TranscriptBlock {
        id,
        label: Some("status"),
        text: text.to_string(),
        tone: TranscriptTone::Status,
    })
}

fn should_show_initial_reply_status(last_block: Option<&TranscriptBlock>) -> bool {
    matches!(
        last_block.map(|block| block.tone),
        None | Some(TranscriptTone::User)
    )
}

/// Renders the ephemeral "arguments are still streaming in" feedback for a
/// [`crate::agent::contract::ToolCallProgress`] tick — see
/// `ToolCallProgressBuffer` in the rig provider for where these come from.
fn tool_call_preparing_summary(progress: &crate::agent::contract::ToolCallProgress) -> String {
    match &progress.tool_id {
        Some(tool_id) => format!(
            "preparing `{tool_id}`… ({} byte{} so far)",
            progress.bytes,
            if progress.bytes == 1 { "" } else { "s" }
        ),
        None => format!(
            "preparing a tool call… ({} byte{} so far)",
            progress.bytes,
            if progress.bytes == 1 { "" } else { "s" }
        ),
    }
}

fn tool_result_summary(call_id: &str, output: &serde_json::Value) -> String {
    match output {
        serde_json::Value::Object(map) => {
            let mut keys = map.keys().take(4).cloned().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{call_id} completed ({} field{}){}",
                map.len(),
                if map.len() == 1 { "" } else { "s" },
                if keys.is_empty() {
                    String::new()
                } else {
                    format!(": {}", keys.join(", "))
                }
            )
        }
        serde_json::Value::Array(values) => {
            format!("{call_id} completed ({} item array)", values.len())
        }
        _ => format!("{call_id} {output}"),
    }
}
