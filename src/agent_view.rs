use floem::event::{Event, EventListener, EventPropagation};
use floem::peniko::{kurbo::Point, Color};
use floem::prelude::*;
use horizon::agent::{
    AgentFrame, AgentFrameItem, AgentMessage, AgentMessageRole, AgentSessionState,
};
use horizon::fonts::HORIZON_FONT_FAMILY;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TranscriptBlock {
    id: usize,
    label: Option<&'static str>,
    text: String,
    tone: TranscriptTone,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TranscriptTone {
    User,
    Assistant,
    Thinking,
    Status,
    Tool,
    Approval,
    Error,
    Lifecycle,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MarkdownLine {
    index: usize,
    text: String,
    kind: MarkdownLineKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MarkdownLineKind {
    Heading,
    Paragraph,
    Bullet,
    Code,
    Blank,
}

pub fn agent_frame_view(
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    visible: impl Fn() -> bool + Copy + 'static,
) -> impl IntoView {
    let follow_latest = RwSignal::new(true);
    let viewport = RwSignal::new(None::<floem::peniko::kurbo::Rect>);
    let content = dyn_stack(
        move || transcript_blocks(&frame()),
        move |block| (block.id, block.tone, block.label),
        move |block| transcript_block_view(block, frame),
    )
    .style(|s| s.width_full().flex_col().gap(8).padding(16));
    let content_id = content.id();

    scroll(content)
        .on_scroll(move |rect| {
            viewport.set(Some(rect));
            if viewport_is_at_bottom(rect, content_height(content_id)) {
                follow_latest.set(true);
            }
        })
        .scroll_to(move || {
            if !visible() || !follow_latest.get() {
                return None;
            }

            let frame = frame();
            let _ = transcript_revision(&frame);
            Some(Point::new(0.0, 1_000_000_000.0))
        })
        .scroll_style(|s| s.shrink_to_fit().overflow_clip(true))
        .style(move |s| {
            if !visible() {
                return s.hide();
            }

            s.width_full()
                .flex_basis(0.0)
                .flex_grow(1.0)
                .min_height(0.0)
                .background(Color::rgb8(24, 27, 32))
        })
        .on_event(EventListener::PointerWheel, move |event| {
            if let Event::PointerWheel(pointer) = event {
                if pointer.delta.y < 0.0 {
                    follow_latest.set(false);
                } else if pointer.delta.y > 0.0
                    && viewport
                        .get_untracked()
                        .is_some_and(|rect| viewport_is_at_bottom(rect, content_height(content_id)))
                {
                    follow_latest.set(true);
                }
            }
            EventPropagation::Continue
        })
}

fn content_height(id: floem::ViewId) -> f64 {
    id.get_layout()
        .map(|layout| layout.size.height as f64)
        .unwrap_or(0.0)
}

fn viewport_is_at_bottom(viewport: floem::peniko::kurbo::Rect, content_height: f64) -> bool {
    content_height <= 0.0 || viewport.y1 >= content_height - 2.0
}

fn transcript_block_view(
    block: TranscriptBlock,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    let tone = block.tone;
    let expanded = RwSignal::new(tone != TranscriptTone::Thinking);
    let block_for_label = block.clone();

    h_stack((
        label(|| String::new()).style(move |s| {
            if tone == TranscriptTone::User {
                s.flex_basis(0.0).flex_grow(1.0).min_width(40.0)
            } else {
                s.hide()
            }
        }),
        v_stack((
            label(move || block_label(&block_for_label, expanded.get()))
                .on_click_stop(move |_| {
                    if tone == TranscriptTone::Thinking {
                        expanded.update(|expanded| *expanded = !*expanded);
                    }
                })
                .style(move |s| {
                    if !shows_label(tone) {
                        return s.hide();
                    }

                    let (background, border) = block_colors(tone);
                    let s = s
                        .width_full()
                        .min_height(28)
                        .items_center()
                        .padding_horiz(10)
                        .padding_vert(5)
                        .font_family(HORIZON_FONT_FAMILY.to_string())
                        .font_size(block_label_size(tone))
                        .line_height(1.35)
                        .color(block_text_color(tone))
                        .background(background)
                        .border(1.0)
                        .border_color(border);

                    if expanded.get() && tone == TranscriptTone::Thinking {
                        s.border_bottom(0.0)
                    } else {
                        s
                    }
                }),
            markdown_block_view(block.clone(), expanded, frame),
        ))
        .style(move |s| {
            let (background, border) = block_colors(tone);
            let s = s
                .flex_col()
                .min_width(0.0)
                .max_width(block_max_width(tone))
                .background(background)
                .border(1.0)
                .border_color(border);

            match tone {
                TranscriptTone::User => s,
                _ => s.flex_basis(0.0).flex_grow(1.0),
            }
        }),
    ))
    .style(move |s| s.width_full().items_start().gap(12))
}

fn markdown_block_view(
    block: TranscriptBlock,
    expanded: RwSignal<bool>,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    let tone = block.tone;
    let block_id = block.id;
    let block_label = block.label;

    dyn_stack(
        move || {
            if tone == TranscriptTone::Thinking && !expanded.get() {
                Vec::new()
            } else {
                let text = current_block_text(&frame(), block_id, tone, block_label);
                markdown_lines(&text)
            }
        },
        move |line| (line.index, line.kind, line.text.clone()),
        move |line| markdown_line_view(line, tone),
    )
    .style(move |s| {
        if tone == TranscriptTone::Thinking && !expanded.get() {
            return s.hide();
        }

        s.width_full()
            .flex_col()
            .gap(3)
            .padding_horiz(14)
            .padding_vert(10)
    })
}

fn markdown_line_view(line: MarkdownLine, tone: TranscriptTone) -> impl IntoView {
    label(move || line.text.clone()).style(move |s| {
        let mut s = s
            .width_full()
            .min_width(0.0)
            .font_family(HORIZON_FONT_FAMILY.to_string())
            .line_height(1.42)
            .color(block_text_color(tone));

        s = match line.kind {
            MarkdownLineKind::Heading => s.font_size(14).padding_top(5).padding_bottom(3),
            MarkdownLineKind::Bullet => s.font_size(12).padding_left(8),
            MarkdownLineKind::Code => s
                .font_size(12)
                .padding_horiz(8)
                .padding_vert(3)
                .background(Color::rgb8(20, 23, 28))
                .border(1.0)
                .border_color(Color::rgb8(43, 49, 60)),
            MarkdownLineKind::Blank => s.font_size(6).height(6),
            MarkdownLineKind::Paragraph => s.font_size(12),
        };

        s
    })
}

fn block_label(block: &TranscriptBlock, expanded: bool) -> String {
    if block.tone == TranscriptTone::Thinking {
        return if expanded {
            "thinking".to_string()
        } else {
            "thinking".to_string()
        };
    }

    match block.label {
        Some(label) => label.to_string(),
        None => String::new(),
    }
}

fn shows_label(tone: TranscriptTone) -> bool {
    !matches!(tone, TranscriptTone::User | TranscriptTone::Assistant)
}

fn transcript_blocks(frame: &AgentFrame) -> Vec<TranscriptBlock> {
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

fn current_block_text(
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

fn transcript_revision(frame: &AgentFrame) -> usize {
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

fn message_block(id: usize, message: &AgentMessage) -> TranscriptBlock {
    let tone = match message.role {
        AgentMessageRole::User => TranscriptTone::User,
        AgentMessageRole::Assistant => TranscriptTone::Assistant,
    };

    TranscriptBlock {
        id,
        label: None,
        text: message.text.clone(),
        tone,
    }
}

fn status_block(
    state: Option<AgentSessionState>,
    id: usize,
    last_block: Option<&TranscriptBlock>,
) -> Option<TranscriptBlock> {
    let text = match state? {
        AgentSessionState::Running => {
            if !should_show_initial_reply_status(last_block) {
                return None;
            }
            "Agent is replying..."
        }
        AgentSessionState::ToolRunning => {
            if matches!(
                last_block.map(|block| block.tone),
                Some(TranscriptTone::Tool | TranscriptTone::Assistant | TranscriptTone::Thinking)
            ) {
                return None;
            }
            "Running tool..."
        }
        AgentSessionState::WaitingForApproval => {
            if matches!(
                last_block.map(|block| block.tone),
                Some(TranscriptTone::Approval)
            ) {
                return None;
            }
            "Approval required"
        }
        AgentSessionState::Failed => "Agent failed",
        AgentSessionState::Terminated => "Agent terminated",
        AgentSessionState::Created
        | AgentSessionState::WaitingForUser
        | AgentSessionState::Completed => return None,
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

fn markdown_lines(text: &str) -> Vec<MarkdownLine> {
    let mut in_code = false;
    let mut lines = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let trimmed_end = raw_line.trim_end();
        let trimmed = trimmed_end.trim_start();

        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }

        let (kind, text) = if in_code {
            (MarkdownLineKind::Code, trimmed_end.to_string())
        } else if trimmed.is_empty() {
            (MarkdownLineKind::Blank, String::new())
        } else if trimmed.starts_with('#') {
            (
                MarkdownLineKind::Heading,
                trimmed.trim_start_matches('#').trim_start().to_string(),
            )
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            (
                MarkdownLineKind::Bullet,
                format!("- {}", strip_inline_markers(rest)),
            )
        } else {
            (
                MarkdownLineKind::Paragraph,
                strip_inline_markers(trimmed_end),
            )
        };

        lines.push(MarkdownLine { index, text, kind });
    }

    if lines.is_empty() {
        lines.push(MarkdownLine {
            index: 0,
            text: String::new(),
            kind: MarkdownLineKind::Blank,
        });
    }

    lines
}

fn strip_inline_markers(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "")
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

fn block_label_size(tone: TranscriptTone) -> f32 {
    match tone {
        TranscriptTone::User => 12.0,
        TranscriptTone::Assistant => 12.0,
        TranscriptTone::Thinking => 12.0,
        TranscriptTone::Status => 12.0,
        _ => 11.0,
    }
}

fn block_max_width(tone: TranscriptTone) -> f32 {
    match tone {
        TranscriptTone::User => 620.0,
        TranscriptTone::Assistant => 1120.0,
        _ => 1200.0,
    }
}

fn block_text_color(tone: TranscriptTone) -> Color {
    match tone {
        TranscriptTone::Status => Color::rgb8(166, 174, 188),
        TranscriptTone::Thinking => Color::rgb8(172, 178, 190),
        TranscriptTone::Tool | TranscriptTone::Approval => Color::rgb8(214, 221, 232),
        TranscriptTone::Error => Color::rgb8(255, 174, 178),
        _ => Color::rgb8(235, 238, 244),
    }
}

fn block_colors(tone: TranscriptTone) -> (Color, Color) {
    match tone {
        TranscriptTone::User => (Color::rgb8(30, 43, 63), Color::rgb8(65, 94, 133)),
        TranscriptTone::Assistant => (Color::rgb8(29, 33, 40), Color::rgb8(48, 56, 68)),
        TranscriptTone::Thinking => (Color::rgb8(23, 26, 31), Color::rgb8(43, 48, 57)),
        TranscriptTone::Status => (Color::rgb8(25, 30, 37), Color::rgb8(47, 56, 68)),
        TranscriptTone::Tool => (Color::rgb8(23, 32, 34), Color::rgb8(42, 66, 66)),
        TranscriptTone::Approval => (Color::rgb8(38, 34, 26), Color::rgb8(78, 66, 44)),
        TranscriptTone::Error => (Color::rgb8(42, 28, 32), Color::rgb8(88, 52, 58)),
        TranscriptTone::Lifecycle => (Color::rgb8(28, 32, 39), Color::rgb8(42, 48, 58)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon::agent::{AgentFrameItem, AgentMessage, AgentMessageDelta, AgentMessageRole};

    #[test]
    fn transcript_blocks_keep_full_assistant_text() {
        let text = "long assistant response ".repeat(80);
        let frame = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::Message(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: text.clone(),
            })],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, text);
        assert_eq!(blocks[0].tone, TranscriptTone::Assistant);
    }

    #[test]
    fn transcript_blocks_append_ephemeral_status() {
        let frame = AgentFrame {
            state: Some(AgentSessionState::Running),
            items: Vec::new(),
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].tone, TranscriptTone::Status);
        assert_eq!(blocks[0].text, "Agent is replying...");
    }

    #[test]
    fn transcript_blocks_hide_reply_status_after_stream_starts() {
        let frame = AgentFrame {
            state: Some(AgentSessionState::Running),
            items: vec![AgentFrameItem::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "thinking".to_string(),
            })],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].tone, TranscriptTone::Thinking);
    }
}
