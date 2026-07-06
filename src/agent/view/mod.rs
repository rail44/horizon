use crate::agent::frame::AgentFrame;
use crate::ui::fonts::font_family;
use crate::ui::theme;
use floem::event::{Event, EventListener, EventPropagation};
use floem::peniko::kurbo::Point;
use floem::prelude::*;
use floem::reactive::create_memo;

mod diff;
mod labels;
mod markdown;
mod style;
mod tool_header;
mod tool_view;
mod transcript;

use labels::{block_label, shows_label};
use markdown::{markdown_lines, MarkdownLine, MarkdownLineKind};
use style::{block_colors, block_max_width, block_text_color};
use transcript::{
    compute_transcript_window, current_block_text, BlockKind, TranscriptBlock, TranscriptTone,
    TranscriptWindow,
};

pub(crate) fn agent_frame_view(
    frame: impl Fn() -> AgentFrame + Copy + 'static,
    visible: impl Fn() -> bool + Copy + 'static,
) -> impl IntoView {
    let follow_latest = RwSignal::new(true);
    let viewport = RwSignal::new(None::<floem::peniko::kurbo::Rect>);
    // Recomputed only when `transcript_revision` actually changes (see
    // `compute_transcript_window`), so a reactive re-run caused by some
    // *other* pane's agent frame updating the shared `Frames` signal is a
    // cheap no-op here instead of re-walking this session's whole item log.
    let window = create_memo(move |previous: Option<&TranscriptWindow>| {
        compute_transcript_window(&frame(), previous)
    });
    let content = v_stack((
        label(move || omitted_summary(window.with(|window| window.omitted))).style(move |s| {
            if window.with(|window| window.omitted) == 0 {
                return s.hide();
            }

            s.width_full().font_size(11).color(theme::text_muted())
        }),
        dyn_stack(
            move || window.with(|window| window.blocks.clone()),
            move |block| (block.id, block.tone),
            move |block| transcript_block_view(block, frame),
        )
        .style(|s| s.width_full().flex_col().gap(8)),
    ))
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

            // Track the memoized revision (a `usize` copy) instead of
            // calling `frame()` directly: this used to clone the whole
            // `AgentFrame` on every scroll re-check just to derive the same
            // revision the transcript memo above already computed.
            let _ = window.with(|window| window.revision);
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
                .background(theme::surface_panel())
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

fn omitted_summary(omitted: usize) -> String {
    format!(
        "{omitted} earlier item{} hidden",
        if omitted == 1 { "" } else { "s" }
    )
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
    let block_id = block.id;
    let expanded = RwSignal::new(!style::is_collapsible(tone));
    let kind = block.kind;

    h_stack((
        label(String::new).style(move |s| {
            if tone == TranscriptTone::User {
                s.flex_basis(0.0).flex_grow(1.0).min_width(40.0)
            } else {
                s.hide()
            }
        }),
        v_stack((
            transcript_header_view(block_id, tone, kind.clone(), expanded, frame),
            transcript_body_view(block_id, tone, kind, expanded, frame),
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

/// The block's one-line header. `Tool`-kind blocks route to
/// `tool_view::tool_header_view`, whose text/color re-derive live from
/// `frame` on every status transition; every other kind keeps the
/// pre-slice-1 static label (computed once -- these blocks' headers never
/// change over their lifetime, only their body text streams in).
fn transcript_header_view(
    block_id: usize,
    tone: TranscriptTone,
    kind: BlockKind,
    expanded: RwSignal<bool>,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    match kind {
        BlockKind::Tool(tool) => {
            tool_view::tool_header_view(block_id, tool, expanded, frame).into_any()
        }
        BlockKind::Text { .. } => {
            let text = block_label(tone, &kind);
            label(move || text.clone())
                .on_click_stop(move |_| {
                    if tone == TranscriptTone::Thinking {
                        expanded.update(|expanded| *expanded = !*expanded);
                    }
                })
                .style(move |s| {
                    if !shows_label(tone) {
                        return s.hide();
                    }
                    style::header_row_style(s, tone, expanded.get()).color(block_text_color(tone))
                })
                .into_any()
        }
    }
}

fn transcript_body_view(
    block_id: usize,
    tone: TranscriptTone,
    kind: BlockKind,
    expanded: RwSignal<bool>,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    match kind {
        BlockKind::Tool(tool) => {
            tool_view::tool_body_view(block_id, tool, expanded, frame).into_any()
        }
        BlockKind::Text {
            label: text_label, ..
        } => markdown_block_view(block_id, tone, text_label, expanded, frame).into_any(),
    }
}

fn markdown_block_view(
    block_id: usize,
    tone: TranscriptTone,
    body_label: Option<&'static str>,
    expanded: RwSignal<bool>,
    frame: impl Fn() -> AgentFrame + Copy + 'static,
) -> impl IntoView {
    dyn_stack(
        move || {
            if tone == TranscriptTone::Thinking && !expanded.get() {
                Vec::new()
            } else {
                let text = current_block_text(&frame(), block_id, tone, body_label);
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
            .font_family(font_family().to_string())
            .line_height(1.42)
            .color(block_text_color(tone));

        s = match line.kind {
            MarkdownLineKind::Heading => s.font_size(14).padding_top(5).padding_bottom(3),
            MarkdownLineKind::Bullet => s.font_size(12).padding_left(8),
            MarkdownLineKind::Code => s
                .font_size(12)
                .padding_horiz(8)
                .padding_vert(3)
                .background(theme::surface_base())
                .border(1.0)
                .border_color(theme::border_subtle()),
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
    use transcript::transcript_blocks;

    fn text_of(block: &TranscriptBlock) -> &str {
        match &block.kind {
            BlockKind::Text { text, .. } => text,
            BlockKind::Tool(_) => panic!("expected a text block, got a tool block"),
        }
    }

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
        assert_eq!(text_of(&blocks[0]), text);
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
        assert_eq!(text_of(&blocks[0]), "Agent is replying...");
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

    #[test]
    fn omitted_summary_pluralizes_the_item_count() {
        assert_eq!(omitted_summary(1), "1 earlier item hidden");
        assert_eq!(omitted_summary(2), "2 earlier items hidden");
    }
}
