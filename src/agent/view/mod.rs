use crate::agent::frame::AgentFrame;
use crate::ui::fonts::HORIZON_FONT_FAMILY;
use floem::event::{Event, EventListener, EventPropagation};
use floem::peniko::{kurbo::Point, Color};
use floem::prelude::*;

mod labels;
mod markdown;
mod style;
mod transcript;

use labels::{block_label, shows_label};
use markdown::{markdown_lines, MarkdownLine, MarkdownLineKind};
use style::{block_colors, block_label_size, block_max_width, block_text_color};
use transcript::{
    current_block_text, transcript_blocks, transcript_revision, TranscriptBlock, TranscriptTone,
};

pub(crate) fn agent_frame_view(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Message, MessageDelta, MessageRole, SessionState};
    use crate::agent::frame::AgentFrameItem;

    #[test]
    fn transcript_blocks_keep_full_assistant_text() {
        let text = "long assistant response ".repeat(80);
        let frame = AgentFrame {
            state: None,
            items: vec![AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
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
            state: Some(SessionState::Running),
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
            state: Some(SessionState::Running),
            items: vec![AgentFrameItem::ReasoningDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "thinking".to_string(),
            })],
        };

        let blocks = transcript_blocks(&frame);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].tone, TranscriptTone::Thinking);
    }
}
