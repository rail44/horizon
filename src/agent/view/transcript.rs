use std::collections::HashMap;

use serde_json::Value;

use crate::agent::contract::{Message, MessageRole, SessionState, ToolCallId};
use crate::agent::frame::{AgentFrame, AgentFrameItem};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TranscriptBlock {
    pub(super) id: usize,
    pub(super) tone: TranscriptTone,
    pub(super) kind: BlockKind,
}

/// A block's content. `Text` covers every item that was already one block
/// per item pre-slice-1 (messages, thinking, status, approval, error,
/// exit); `Tool` is the one-block-per-call-id merge (`docs/
/// agent-output-ui-design.md` decision 1) of what used to be up to three
/// separate blocks (`ToolCallRequested`/`ToolCallStarted`/
/// `ToolCallFinished`).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BlockKind {
    Text {
        label: Option<&'static str>,
        text: String,
    },
    Tool(ToolBlock),
}

/// One tool call's merged state, keyed by `call_id` -- see
/// [`transcript_blocks`] for how the three lifecycle items fold into this.
/// `call_id`/`tool_id`/`input` are `None` only for a still-in-flight
/// `ToolCallPreparing` progress tick, before the real request (which always
/// carries all three) arrives.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ToolBlock {
    pub(super) call_id: Option<ToolCallId>,
    pub(super) tool_id: Option<String>,
    pub(super) input: Option<Value>,
    pub(super) status: ToolStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ToolStatus {
    Preparing { bytes: usize },
    Requested,
    Started,
    Finished { output: Value },
}

impl ToolBlock {
    /// The `old_string`/`new_string` pair an `fs.edit` request carries, if
    /// present -- what `diff::line_diff` reconstructs this block's line
    /// diff from (`docs/agent-output-ui-design.md` decision 3: "joining the
    /// finished result to its originating request's `old_string`/
    /// `new_string`"). `None` for any other tool, or if the input is
    /// somehow missing one of the two fields.
    pub(super) fn edit_strings(&self) -> Option<(&str, &str)> {
        let input = self.input.as_ref()?;
        let old = input.get("old_string")?.as_str()?;
        let new = input.get("new_string")?.as_str()?;
        Some((old, new))
    }

    /// Whether this call finished with Horizon's `is_error` result shape.
    /// `false` for any call still in flight -- only a `Finished` status can
    /// be an error.
    pub(super) fn is_error(&self) -> bool {
        match &self.status {
            ToolStatus::Finished { output } => is_error_output(output),
            _ => false,
        }
    }
}

/// Whether a tool result `Value` is Horizon's `is_error` shape
/// (`{"is_error": true, ...}` -- see `tools/fs/mod.rs::error_output` and the
/// `bash` tool's own error paths). Shared by the header/body renderers
/// (`tool_header`, `tool_view`) and the status-color lookup (`style::
/// tool_status_color`), so "what counts as a failed tool call" has exactly
/// one definition.
pub(super) fn is_error_output(output: &Value) -> bool {
    output
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
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

/// Builds the transcript's blocks from `frame.items`, merging every tool
/// call's `ToolCallRequested`/`ToolCallStarted`/`ToolCallFinished` items
/// into the one [`ToolBlock`] first created for that `call_id` (at the
/// `ToolCallRequested` -- or, if it's still in flight, `ToolCallPreparing`
/// -- item's own index, which becomes the merged block's stable `id`).
/// `ApprovalRequested`/`Error`/`Exited`/messages/thinking are unaffected,
/// one block per item as before slice 1.
pub(super) fn transcript_blocks(frame: &AgentFrame) -> Vec<TranscriptBlock> {
    let mut blocks: Vec<TranscriptBlock> = Vec::new();
    // Maps a call id to its block's position in `blocks` (not its position
    // in `frame.items`), so `ToolCallStarted`/`ToolCallFinished` -- which
    // never produce a block of their own -- can mutate the existing one in
    // place instead of appending a new one.
    let mut tool_positions: HashMap<ToolCallId, usize> = HashMap::new();

    for (id, item) in frame.items.iter().enumerate() {
        match item {
            AgentFrameItem::ToolCallStarted(call_id) => {
                if let Some(&position) = tool_positions.get(call_id) {
                    if let BlockKind::Tool(tool) = &mut blocks[position].kind {
                        tool.status = ToolStatus::Started;
                    }
                }
                continue;
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(&position) = tool_positions.get(&result.call_id) {
                    if let BlockKind::Tool(tool) = &mut blocks[position].kind {
                        tool.status = ToolStatus::Finished {
                            output: result.output.clone(),
                        };
                    }
                } else {
                    // Defensive: a finished call whose request isn't in
                    // this frame (shouldn't happen -- requests and results
                    // are folded onto the same frame) still needs *some*
                    // representation rather than silently vanishing.
                    blocks.push(TranscriptBlock {
                        id,
                        tone: TranscriptTone::Tool,
                        kind: BlockKind::Tool(ToolBlock {
                            call_id: Some(result.call_id.clone()),
                            tool_id: None,
                            input: None,
                            status: ToolStatus::Finished {
                                output: result.output.clone(),
                            },
                        }),
                    });
                }
                continue;
            }
            _ => {}
        }

        let Some(block) = transcript_block(id, item) else {
            continue;
        };
        if let BlockKind::Tool(ToolBlock {
            call_id: Some(call_id),
            ..
        }) = &block.kind
        {
            tool_positions.insert(call_id.clone(), blocks.len());
        }
        blocks.push(block);
    }

    if let Some(status) = status_block(frame.state, blocks.len(), blocks.last()) {
        blocks.push(status);
    }
    blocks
}

/// The live text of a `Text`-kind block at `id`/`tone`/`label`, re-derived
/// directly from `frame` -- used by `markdown_block_view`'s reactive
/// closure so a streaming block's displayed text keeps growing even though
/// `dyn_stack` never rebuilds its view for an unchanged key (see that
/// view's call site). Not used for `Tool`-kind blocks: see
/// [`current_tool_block`] instead.
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
            transcript_blocks(frame)
                .into_iter()
                .find(|block| block.id == id)
        });

    block
        .filter(|block| block.tone == tone)
        .and_then(|block| match block.kind {
            BlockKind::Text {
                label: block_label,
                text,
            } if block_label == label => Some(text),
            _ => None,
        })
        .unwrap_or_default()
}

/// The live merged state of the tool call whose block started at `id`
/// (the `ToolCallRequested`/`ToolCallPreparing` item's own index) --
/// `tool_view`'s reactive header/body closures call this so a block whose
/// `dyn_stack` key never changes (see [`transcript_blocks`]'s doc comment)
/// still picks up later `ToolCallStarted`/`ToolCallFinished` items. Scans
/// forward from `id` only as far as the terminal `ToolCallFinished` (or the
/// end of the frame if the call is still in flight), not the whole item
/// log.
pub(super) fn current_tool_block(frame: &AgentFrame, id: usize) -> Option<ToolBlock> {
    let (call_id, tool_id, input, mut status) = match frame.items.get(id)? {
        AgentFrameItem::ToolCallRequested(request) => (
            Some(request.call_id.clone()),
            Some(request.tool_id.clone()),
            Some(request.input.clone()),
            ToolStatus::Requested,
        ),
        AgentFrameItem::ToolCallPreparing(progress) => (
            None,
            progress.tool_id.clone(),
            None,
            ToolStatus::Preparing {
                bytes: progress.bytes,
            },
        ),
        _ => return None,
    };

    if let Some(call_id) = &call_id {
        for item in &frame.items[id + 1..] {
            match item {
                AgentFrameItem::ToolCallStarted(started_id) if started_id == call_id => {
                    status = ToolStatus::Started;
                }
                AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id => {
                    status = ToolStatus::Finished {
                        output: result.output.clone(),
                    };
                    break;
                }
                _ => {}
            }
        }
    }

    Some(ToolBlock {
        call_id,
        tool_id,
        input,
        status,
    })
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

/// A cheap proxy for "has `frame` changed since last computed", checked
/// purely against `frame.items`/`frame.state` -- independent of how
/// [`transcript_blocks`] groups those items into blocks, so merging tool
/// lifecycle items into one block (slice 1) doesn't require touching this:
/// every state transition this must detect (a new `ToolCallStarted`/
/// `ToolCallFinished` item, `ToolCallPreparing`'s growing byte count) is
/// already either a new item (bumping `frame.items.len()`) or a changed
/// field length summed below.
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
            tone: TranscriptTone::Thinking,
            kind: BlockKind::Text {
                label: Some("thinking"),
                text: delta.text.clone(),
            },
        }),
        AgentFrameItem::AssistantTextDelta(delta) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Assistant,
            kind: BlockKind::Text {
                label: None,
                text: delta.text.clone(),
            },
        }),
        AgentFrameItem::ToolCallRequested(request) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Tool,
            kind: BlockKind::Tool(ToolBlock {
                call_id: Some(request.call_id.clone()),
                tool_id: Some(request.tool_id.clone()),
                input: Some(request.input.clone()),
                status: ToolStatus::Requested,
            }),
        }),
        AgentFrameItem::ToolCallPreparing(progress) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Tool,
            kind: BlockKind::Tool(ToolBlock {
                call_id: None,
                tool_id: progress.tool_id.clone(),
                input: None,
                status: ToolStatus::Preparing {
                    bytes: progress.bytes,
                },
            }),
        }),
        AgentFrameItem::ApprovalRequested(request) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Approval,
            kind: BlockKind::Text {
                label: Some("approval"),
                text: request.reason.clone(),
            },
        }),
        AgentFrameItem::Error(error) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Error,
            kind: BlockKind::Text {
                label: Some("error"),
                text: error.message.clone(),
            },
        }),
        AgentFrameItem::Exited(exit) => Some(TranscriptBlock {
            id,
            tone: TranscriptTone::Lifecycle,
            kind: BlockKind::Text {
                label: Some("exited"),
                text: exit.reason.clone(),
            },
        }),
        // Merged into an already-emitted `ToolBlock` by `transcript_blocks`
        // above rather than producing a block of their own.
        AgentFrameItem::ToolCallStarted(_) | AgentFrameItem::ToolCallFinished(_) => None,
    }
}

fn message_block(id: usize, message: &Message) -> TranscriptBlock {
    let tone = match message.role {
        MessageRole::User => TranscriptTone::User,
        MessageRole::Assistant => TranscriptTone::Assistant,
    };

    TranscriptBlock {
        id,
        tone,
        kind: BlockKind::Text {
            label: None,
            text: message.text.clone(),
        },
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
        tone: TranscriptTone::Status,
        kind: BlockKind::Text {
            label: Some("status"),
            text: text.to_string(),
        },
    })
}

fn should_show_initial_reply_status(last_block: Option<&TranscriptBlock>) -> bool {
    matches!(
        last_block.map(|block| block.tone),
        None | Some(TranscriptTone::User)
    )
}

/// Whether the `ReasoningDelta` item at `id` is still actively streaming --
/// it's the last item in the frame (nothing has superseded it yet: an
/// `AssistantTextDelta`, a tool call, or a committed message all end a
/// thought) and the turn is still in flight. Drives `Thinking`'s
/// auto-expand-while-streaming behavior (`docs/agent-output-ui-design.md`
/// decision 5) -- `agent_frame_view`'s per-block effect composes this with
/// the block's manual override (a manual toggle always wins once set).
pub(super) fn is_thinking_streaming(frame: &AgentFrame, id: usize) -> bool {
    frame.is_turn_in_flight()
        && id + 1 == frame.items.len()
        && matches!(frame.items.get(id), Some(AgentFrameItem::ReasoningDelta(_)))
}

/// Whether a block of `tone` opens a fresh turn -- currently just "is this
/// a user message" (`docs/agent-output-ui-design.md` decision 6). A block's
/// tone never changes over its lifetime, so this is safe to read once at
/// view-construction time rather than re-derived live from `frame` the way
/// `is_thinking_streaming`/tool status must be.
pub(super) fn starts_new_turn(tone: TranscriptTone) -> bool {
    tone == TranscriptTone::User
}

/// Whether a trailing turn-end rule should render after the last rendered
/// block: the turn has finished (not in flight) and that last block is
/// actual turn content, not a bare user message still awaiting a reply (in
/// which case there's no turn to mark the end of yet). `last_tone` is the
/// window's last block's tone, if any. No model/duration footer text here
/// -- `AgentFrame` carries neither (see this slice's report); a rule is all
/// that's implemented.
pub(super) fn show_turn_end_rule(frame: &AgentFrame, last_tone: Option<TranscriptTone>) -> bool {
    !frame.is_turn_in_flight() && last_tone.is_some_and(|tone| tone != TranscriptTone::User)
}

/// The block id of the most recent user message in `frame`, if any --
/// what the "jump to latest user message" return pill (`docs/
/// agent-output-ui-design.md` decision 7) resolves its target from. A
/// message item's block id is always its own index in `frame.items` (see
/// [`message_block`]), so this can scan `frame.items` directly rather than
/// building the full windowed block list just to find one id.
pub(super) fn latest_user_block_id(frame: &AgentFrame) -> Option<usize> {
    frame.items.iter().enumerate().rev().find_map(|(id, item)| {
        matches!(
            item,
            AgentFrameItem::Message(Message {
                role: MessageRole::User,
                ..
            })
        )
        .then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{
        ApprovalRequest, Message, MessageRole, ToolCallId, ToolCallRequest, ToolCallResult,
    };

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

    fn text_of(block: &TranscriptBlock) -> &str {
        match &block.kind {
            BlockKind::Text { text, .. } => text,
            BlockKind::Tool(_) => panic!("expected a text block, got a tool block"),
        }
    }

    fn tool_of(block: &TranscriptBlock) -> &ToolBlock {
        match &block.kind {
            BlockKind::Tool(tool) => tool,
            BlockKind::Text { .. } => panic!("expected a tool block, got a text block"),
        }
    }

    #[test]
    fn window_blocks_keeps_everything_under_the_window() {
        let frame = message_frame(TRANSCRIPT_WINDOW - 1);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 0);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW - 1);
        assert_eq!(text_of(&blocks[0]), "message 0");
    }

    #[test]
    fn window_blocks_keeps_everything_at_exactly_the_boundary() {
        let frame = message_frame(TRANSCRIPT_WINDOW);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 0);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW);
        assert_eq!(text_of(&blocks[0]), "message 0");
    }

    #[test]
    fn window_blocks_omits_the_oldest_blocks_past_the_boundary() {
        let frame = message_frame(TRANSCRIPT_WINDOW + 1);
        let (omitted, blocks) = window_blocks(transcript_blocks(&frame), TRANSCRIPT_WINDOW);

        assert_eq!(omitted, 1);
        assert_eq!(blocks.len(), TRANSCRIPT_WINDOW);
        // The single omitted block is the oldest one ("message 0"); the
        // trailing window keeps the most recent blocks, in order.
        assert_eq!(text_of(&blocks[0]), "message 1");
        assert_eq!(
            text_of(blocks.last().unwrap()),
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
        assert_eq!(text_of(&blocks[0]), format!("message {extra}"));
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

    // --- tool call merge (slice 1) ----------------------------------------

    fn request(call_id: &str) -> ToolCallRequest {
        ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::json!({ "path": "src/lib.rs" }),
        }
    }

    #[test]
    fn a_lone_request_produces_one_pending_tool_block() {
        let frame = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::ToolCallRequested(request("call-1"))],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        let tool = tool_of(&blocks[0]);
        assert_eq!(tool.status, ToolStatus::Requested);
        assert_eq!(tool.call_id, Some(ToolCallId("call-1".to_string())));
    }

    #[test]
    fn request_started_and_finished_merge_into_a_single_block() {
        let call_id = ToolCallId("call-1".to_string());
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(request("call-1")),
                AgentFrameItem::ToolCallStarted(call_id.clone()),
                AgentFrameItem::ToolCallFinished(ToolCallResult {
                    call_id: call_id.clone(),
                    output: serde_json::json!({ "content": "fn main() {}\n" }),
                }),
            ],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(
            blocks.len(),
            1,
            "the three lifecycle items must collapse into one block"
        );
        let tool = tool_of(&blocks[0]);
        assert_eq!(tool.call_id, Some(call_id));
        assert!(matches!(tool.status, ToolStatus::Finished { .. }));
    }

    #[test]
    fn the_merged_block_transitions_through_every_status_in_place() {
        let call_id = ToolCallId("call-1".to_string());

        let after_request = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::ToolCallRequested(request("call-1"))],
        };
        let blocks = transcript_blocks(&after_request);
        assert_eq!(blocks.len(), 1);
        assert_eq!(tool_of(&blocks[0]).status, ToolStatus::Requested);
        let block_id = blocks[0].id;

        let mut after_started = after_request.clone();
        after_started
            .items
            .push(AgentFrameItem::ToolCallStarted(call_id.clone()));
        let blocks = transcript_blocks(&after_started);
        assert_eq!(blocks.len(), 1, "started must not add a second block");
        assert_eq!(blocks[0].id, block_id, "the block's id must stay stable");
        assert_eq!(tool_of(&blocks[0]).status, ToolStatus::Started);

        let mut after_finished = after_started.clone();
        after_finished
            .items
            .push(AgentFrameItem::ToolCallFinished(ToolCallResult {
                call_id: call_id.clone(),
                output: serde_json::json!({ "content": "" }),
            }));
        let blocks = transcript_blocks(&after_finished);
        assert_eq!(blocks.len(), 1, "finished must not add a second block");
        assert_eq!(blocks[0].id, block_id, "the block's id must stay stable");
        assert!(matches!(
            tool_of(&blocks[0]).status,
            ToolStatus::Finished { .. }
        ));
    }

    #[test]
    fn an_error_result_is_reflected_on_the_merged_block() {
        let call_id = ToolCallId("call-1".to_string());
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(request("call-1")),
                AgentFrameItem::ToolCallFinished(ToolCallResult {
                    call_id,
                    output: serde_json::json!({ "is_error": true, "message": "boom" }),
                }),
            ],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert!(tool_of(&blocks[0]).is_error());
    }

    #[test]
    fn independent_calls_produce_independent_blocks() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(request("call-1")),
                AgentFrameItem::ToolCallRequested(request("call-2")),
                AgentFrameItem::ToolCallFinished(ToolCallResult {
                    call_id: ToolCallId("call-1".to_string()),
                    output: serde_json::json!({}),
                }),
            ],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            tool_of(&blocks[0]).status,
            ToolStatus::Finished { .. }
        ));
        assert_eq!(tool_of(&blocks[1]).status, ToolStatus::Requested);
    }

    #[test]
    fn current_tool_block_reflects_the_latest_status() {
        let call_id = ToolCallId("call-1".to_string());
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(request("call-1")),
                AgentFrameItem::ToolCallStarted(call_id.clone()),
                AgentFrameItem::ToolCallFinished(ToolCallResult {
                    call_id,
                    output: serde_json::json!({ "content": "" }),
                }),
            ],
        };

        let tool = current_tool_block(&frame, 0).expect("block at id 0");

        assert!(matches!(tool.status, ToolStatus::Finished { .. }));
    }

    // --- thinking auto-expand (`is_thinking_streaming`, slice 2) ----------

    fn reasoning_frame(state: Option<SessionState>, trailing: Vec<AgentFrameItem>) -> AgentFrame {
        let mut items = vec![AgentFrameItem::ReasoningDelta(
            crate::agent::contract::MessageDelta {
                role: MessageRole::Assistant,
                text: "thinking...".to_string(),
            },
        )];
        items.extend(trailing);
        AgentFrame { state, items }
    }

    #[test]
    fn thinking_streams_while_it_is_the_last_item_and_the_turn_is_in_flight() {
        let frame = reasoning_frame(Some(SessionState::Running), Vec::new());
        assert!(is_thinking_streaming(&frame, 0));
    }

    #[test]
    fn thinking_stops_streaming_once_a_later_item_supersedes_it() {
        let frame = reasoning_frame(
            Some(SessionState::Running),
            vec![AgentFrameItem::AssistantTextDelta(
                crate::agent::contract::MessageDelta {
                    role: MessageRole::Assistant,
                    text: "answer".to_string(),
                },
            )],
        );
        assert!(!is_thinking_streaming(&frame, 0));
    }

    #[test]
    fn thinking_stops_streaming_once_the_turn_is_no_longer_in_flight() {
        let frame = reasoning_frame(Some(SessionState::Completed), Vec::new());
        assert!(!is_thinking_streaming(&frame, 0));
    }

    #[test]
    fn thinking_is_not_streaming_for_a_non_reasoning_item() {
        let frame = AgentFrame {
            state: Some(SessionState::Running),
            items: vec![AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                text: "hi".to_string(),
            })],
        };
        assert!(!is_thinking_streaming(&frame, 0));
    }

    // --- turn boundaries (`starts_new_turn`/`show_turn_end_rule`, slice 2) -

    #[test]
    fn a_user_block_starts_a_new_turn() {
        assert!(starts_new_turn(TranscriptTone::User));
    }

    #[test]
    fn non_user_blocks_do_not_start_a_new_turn() {
        assert!(!starts_new_turn(TranscriptTone::Assistant));
        assert!(!starts_new_turn(TranscriptTone::Tool));
        assert!(!starts_new_turn(TranscriptTone::Thinking));
    }

    #[test]
    fn turn_end_rule_shows_once_a_completed_turns_last_block_is_not_a_user_message() {
        let frame = AgentFrame {
            state: Some(SessionState::Completed),
            items: Vec::new(),
        };
        assert!(show_turn_end_rule(&frame, Some(TranscriptTone::Assistant)));
        assert!(show_turn_end_rule(&frame, Some(TranscriptTone::Tool)));
    }

    #[test]
    fn turn_end_rule_hides_while_the_turn_is_still_in_flight() {
        let frame = AgentFrame {
            state: Some(SessionState::Running),
            items: Vec::new(),
        };
        assert!(!show_turn_end_rule(&frame, Some(TranscriptTone::Assistant)));
    }

    #[test]
    fn turn_end_rule_hides_when_the_last_block_is_the_awaiting_user_message() {
        let frame = AgentFrame {
            state: Some(SessionState::WaitingForUser),
            items: Vec::new(),
        };
        assert!(!show_turn_end_rule(&frame, Some(TranscriptTone::User)));
    }

    #[test]
    fn turn_end_rule_hides_when_there_are_no_blocks_yet() {
        let frame = AgentFrame {
            state: Some(SessionState::WaitingForUser),
            items: Vec::new(),
        };
        assert!(!show_turn_end_rule(&frame, None));
    }

    // --- latest user message resolution (follow-scroll return pill, slice 3)

    #[test]
    fn latest_user_block_id_finds_the_last_user_message() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::Message(Message {
                    role: MessageRole::User,
                    text: "first".to_string(),
                }),
                AgentFrameItem::Message(Message {
                    role: MessageRole::Assistant,
                    text: "reply".to_string(),
                }),
                AgentFrameItem::Message(Message {
                    role: MessageRole::User,
                    text: "second".to_string(),
                }),
                AgentFrameItem::Message(Message {
                    role: MessageRole::Assistant,
                    text: "reply 2".to_string(),
                }),
            ],
        };

        assert_eq!(latest_user_block_id(&frame), Some(2));
    }

    #[test]
    fn latest_user_block_id_is_none_without_a_user_message() {
        let frame = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                text: "hi".to_string(),
            })],
        };

        assert_eq!(latest_user_block_id(&frame), None);
    }

    #[test]
    fn latest_user_block_id_is_none_for_an_empty_frame() {
        let frame = AgentFrame {
            state: None,
            items: Vec::new(),
        };

        assert_eq!(latest_user_block_id(&frame), None);
    }

    #[test]
    fn approval_and_status_blocks_are_unaffected_by_the_tool_merge() {
        let call_id = ToolCallId("call-1".to_string());
        let frame = AgentFrame {
            state: Some(SessionState::WaitingForApproval),
            items: vec![
                AgentFrameItem::ToolCallRequested(request("call-1")),
                AgentFrameItem::ApprovalRequested(ApprovalRequest {
                    call_id,
                    reason: "writes outside the workspace".to_string(),
                }),
            ],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].tone, TranscriptTone::Approval);
        assert_eq!(text_of(&blocks[1]), "writes outside the workspace");
    }
}
