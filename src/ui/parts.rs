//! The generic parts views draw with: chips, running text, and the fade
//! over folded text.

use gpui::{
    div, rems, rgb, ElementId, Hsla, IntoElement, Overflow, ParentElement as _, SharedString,
    StyleRefinement, Styled as _,
};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::v_flex;

use crate::theme;
use crate::ui::spec::{
    body, fade_height, gap_label, micro, t2, CHIP_RADIUS, CHIP_TINT, FADE_STEPS, REGULAR,
};

/// A status chip: micro type on a tint of its own tone, with the label
/// contrast-floored against the tint it lands on.
pub(crate) fn chip(label: impl Into<SharedString>, tone: Hsla) -> impl IntoElement {
    let surface = theme::tint_over_background(tone, CHIP_TINT);
    let micro = micro();
    div()
        .flex_none()
        .px(gap_label())
        .rounded(CHIP_RADIUS)
        .bg(surface)
        .text_size(micro.size)
        .line_height(micro.line_height)
        .font_weight(REGULAR)
        .text_color(theme::readable_on(tone, surface))
        .child(label.into())
}

/// A keyboard hint. Latin inside a chip is the one place Latin sits in a
/// Japanese label.
pub(crate) fn key_chip(key: &'static str) -> impl IntoElement {
    chip(key, theme::text_muted())
}

/// The steps that fade folded text out into the background. A quad
/// crosses the preview host boundary carrying one solid color, so the fade
/// is stacked solid strips rather than a gradient
/// (`docs/preview-pane-design.md`, the display list's `quad` record).
pub(crate) fn fade() -> impl IntoElement {
    let base: Hsla = rgb(theme::background()).into();
    let height = fade_height();
    let step = height / FADE_STEPS as f32;
    v_flex()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(height)
        .children((0..FADE_STEPS).map(|index| {
            let alpha = (index + 1) as f32 / FADE_STEPS as f32;
            div().h(step).w_full().bg(base.alpha(alpha))
        }))
}

/// Running text across the container's width, with the heading scale
/// clamped below a title. Code blocks scroll horizontally instead of
/// widening the column.
///
/// `TextView` renders markdown itself and exposes a heading's *size* per
/// level; its weight and color are the renderer's own, so H3+ is not
/// separately muted and a heading's weight is whatever the renderer draws
/// bold as.
pub(crate) fn markdown_body(
    id: impl Into<ElementId>,
    source: String,
    color: Hsla,
) -> impl IntoElement {
    let mut code = StyleRefinement::default();
    code.overflow.x = Some(Overflow::Scroll);
    let body = body();
    let heading = t2().size;
    let style = TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(move |level, _base| if level <= 1 { heading } else { body.size })
        .code_block(code);
    div()
        .w_full()
        .text_size(body.size)
        .line_height(body.line_height)
        .font_weight(REGULAR)
        .child(
            TextView::markdown(id, source)
                .style(style)
                .text_color(color),
        )
}
