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

/// Caps how many transcript blocks `agent_frame_view`'s dyn_stack actually
/// materializes as view nodes. Floem repaints by walking the whole view
/// tree (`ViewId::request_paint` bubbles a dirty flag up to the root, and
/// the paint pass then traverses down from there), so an unbounded
/// transcript makes every repaint -- including ones triggered by unrelated
/// state like message-box keystrokes -- cost O(session length). Blocks
/// older than the trailing window are summarized instead of rendered; see
/// [`compute_transcript_window`].
pub(super) const TRANSCRIPT_WINDOW: usize = 200;

/// A [`transcript_blocks`] result trimmed to the trailing [`TRANSCRIPT_WINDOW`]
/// blocks, tagged with the [`transcript_revision`] it was computed from so
/// [`compute_transcript_window`] can skip recomputation on a reactive
/// re-run that isn't actually about this session's own frame (e.g. another
/// pane's agent frame updating the shared `Frames` signal).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TranscriptWindow {
    pub(super) revision: usize,
    pub(super) omitted: usize,
    pub(super) blocks: Vec<TranscriptBlock>,
}

/// Builds the windowed transcript for `frame`, reusing `previous` verbatim
/// -- without re-walking `frame.items` or reallocating `blocks` -- when
/// `frame`'s content hasn't changed since `previous` was computed. This is
/// the memoization `agent_frame_view` wires through `create_memo`.
pub(super) fn compute_transcript_window(
    frame: &AgentFrame,
    previous: Option<&TranscriptWindow>,
) -> TranscriptWindow {
    let revision = transcript_revision(frame);
    if let Some(previous) = previous {
        if previous.revision == revision {
            return previous.clone();
        }
    }

    let (omitted, blocks) = window_blocks(transcript_blocks(frame), TRANSCRIPT_WINDOW);
    TranscriptWindow {
        revision,
        omitted,
        blocks,
    }
}

/// Splits `blocks` into (how many leading blocks fall outside `window`, the
/// trailing `window` blocks to render) -- the oldest blocks are always the
/// ones summarized, never the most recent ones.
fn window_blocks(mut blocks: Vec<TranscriptBlock>, window: usize) -> (usize, Vec<TranscriptBlock>) {
    if blocks.len() <= window {
        return (0, blocks);
    }

    let omitted = blocks.len() - window;
    (omitted, blocks.split_off(omitted))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Message, MessageRole};

    fn message_frame(count: usize) -> AgentFrame {
        AgentFrame {
            state: None,
            items: (0..count)
                .map(|i| {
                    AgentFrameItem::Message(Message {
                        role: MessageRole::Assistant,
                        text: format!("message {i}"),
                    })
                })
                .collect(),
        }
    }

    #[test]
    fn window_blocks_keeps_everything_under_the_window() {
        let frame = message_frame(TRANSCRIPT_WINDOW - 1);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 0);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW - 1);
        assert_eq!(blocks[0].text, "message 0");
    }

    #[test]
    fn window_blocks_keeps_everything_at_exactly_the_boundary() {
        let frame = message_frame(TRANSCRIPT_WINDOW);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 0);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW);
        assert_eq!(blocks[0].text, "message 0");
    }

    #[test]
    fn window_blocks_omits_the_oldest_blocks_past_the_boundary() {
        let frame = message_frame(TRANSCRIPT_WINDOW + 1);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 1);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW);
        // The single omitted block is the oldest one ("message 0"); the
        // trailing window keeps the most recent blocks, in order.
        assert_eq!(blocks[0].text, "message 1");
        assert_eq!(
            blocks.last().unwrap().text,
            format!("message {TRANSCRIPT_WINDOW}")
        );
    }

    #[test]
    fn window_blocks_omits_many_blocks_past_the_boundary() {
        let extra = 350;
        let frame = message_frame(TRANSCRIPT_WINDOW + extra);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, extra);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW);
        assert_eq!(blocks[0].text, format!("message {extra}"));
    }

    #[test]
    fn compute_transcript_window_reuses_previous_when_revision_matches() {
        let frame = message_frame(3);
        let revision = transcript_revision(&frame);
        // Deliberately stale/wrong compared to what a fresh computation
        // from `frame` would produce, so the assertion below can only pass
        // if `compute_transcript_window` actually short-circuited on the
        // matching revision instead of recomputing.
        let stale_previous = TranscriptWindow {
            revision,
            omitted: 999,
            blocks: Vec::new(),
        };

        let result = compute_transcript_window(&frame, Some(&stale_previous));

        assert_eq!(
            result, stale_previous,
            "a matching revision must return the cached value verbatim"
        );
    }

    #[test]
    fn compute_transcript_window_recomputes_when_revision_differs() {
        let frame = message_frame(3);
        let stale_previous = TranscriptWindow {
            revision: transcript_revision(&frame).wrapping_add(1),
            omitted: 999,
            blocks: Vec::new(),
        };

        let result = compute_transcript_window(&frame, Some(&stale_previous));

        assert_eq!(result.revision, transcript_revision(&frame));
        assert_eq!(result.omitted, 0);
        assert_eq!(result.blocks.len(), 3);
    }

    #[test]
    fn compute_transcript_window_recomputes_with_no_previous() {
        let frame = message_frame(3);

        let result = compute_transcript_window(&frame, None);

        assert_eq!(result.revision, transcript_revision(&frame));
        assert_eq!(result.omitted, 0);
        assert_eq!(result.blocks.len(), 3);
    }
}
