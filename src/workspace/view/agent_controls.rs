use crate::agent::contract::ToolCallId;
use crate::ui::fonts::HORIZON_FONT_FAMILY;
use floem::prelude::*;
use floem::{
    action::set_ime_cursor_area,
    peniko::kurbo::{Point, Size},
};

pub(super) fn agent_composer(
    visible: impl Fn() -> bool + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    draft: RwSignal<String>,
    preedit: impl Fn() -> Option<String> + 'static + Copy,
    ime_cursor_area: RwSignal<(Point, Size)>,
) -> impl IntoView {
    label(move || {
        let text = draft.get();
        let preedit = preedit().unwrap_or_default();
        if text.is_empty() && preedit.is_empty() {
            "Message agent...".to_string()
        } else if preedit.is_empty() {
            text
        } else {
            format!("{text}{preedit}")
        }
    })
    .style(move |s| {
        if !visible() {
            return s.hide();
        }

        let border = if active() {
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(57, 64, 76)
        };
        let color = if draft.with(|text| text.is_empty()) && preedit().is_none() {
            floem::peniko::Color::rgb8(115, 122, 136)
        } else {
            floem::peniko::Color::rgb8(233, 236, 242)
        };

        s.width_full()
            .height(34)
            .min_height(34)
            .items_center()
            .padding_horiz(10)
            .margin_horiz(8)
            .margin_bottom(7)
            .font_family(HORIZON_FONT_FAMILY.to_string())
            .font_size(12)
            .line_height(1.2)
            .color(color)
            .background(floem::peniko::Color::rgb8(21, 24, 30))
            .border(1.0)
            .border_color(border)
    })
    .on_move(move |origin| {
        if active() && visible() {
            let position = origin + Point::new(10.0, 6.0).to_vec2();
            let size = Size::new(8.0, 18.0);
            ime_cursor_area.set((position, size));
            set_ime_cursor_area(position, size);
        }
    })
}

pub(super) fn agent_approval_actions(
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_approve: impl Fn(ToolCallId) + 'static + Copy,
    on_deny: impl Fn(ToolCallId) + 'static + Copy,
) -> impl IntoView {
    h_stack((
        agent_approval_button(
            "Approve",
            visible,
            pending_approval,
            move |call_id| on_approve(call_id),
            floem::peniko::Color::rgb8(48, 84, 75),
            floem::peniko::Color::rgb8(132, 220, 198),
        ),
        agent_approval_button(
            "Deny",
            visible,
            pending_approval,
            move |call_id| on_deny(call_id),
            floem::peniko::Color::rgb8(80, 50, 54),
            floem::peniko::Color::rgb8(246, 137, 146),
        ),
    ))
    .style(move |s| {
        if !visible() || pending_approval().is_none() {
            return s.hide();
        }

        s.width_full()
            .height(30)
            .min_height(30)
            .items_center()
            .justify_end()
            .padding_horiz(8)
            .gap(8)
    })
}

fn agent_approval_button(
    text: &'static str,
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_click: impl Fn(ToolCallId) + 'static + Copy,
    background: floem::peniko::Color,
    border: floem::peniko::Color,
) -> impl IntoView {
    label(move || text.to_string())
        .on_click_stop(move |_| {
            if let Some(call_id) = pending_approval() {
                on_click(call_id);
            }
        })
        .style(move |s| {
            if !visible() || pending_approval().is_none() {
                return s.hide();
            }

            s.height(26)
                .padding_horiz(12)
                .items_center()
                .justify_center()
                .font_family(HORIZON_FONT_FAMILY.to_string())
                .font_size(12)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
                .background(background)
                .border(1.0)
                .border_color(border)
        })
}
