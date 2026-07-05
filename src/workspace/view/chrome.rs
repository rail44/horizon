use crate::ui::theme;
use floem::prelude::*;

pub(super) fn chrome_close_button(
    visible: impl Fn() -> bool + 'static + Copy,
    on_close: impl Fn() + 'static,
) -> impl IntoView {
    label(|| "×".to_string())
        .on_click_stop(move |_| on_close())
        .style(move |s| {
            if !visible() {
                return s.hide();
            }

            s.width(20)
                .height(20)
                .items_center()
                .justify_center()
                .font_size(13)
                .color(theme::text_muted())
                .background(floem::peniko::Color::from_rgb8(35, 39, 48))
                .border(1.0)
                .border_color(floem::peniko::Color::from_rgb8(57, 64, 76))
        })
}

/// The pane header's "Cancel turn" affordance -- relocated here (out of the
/// approval area, see `agent_controls::agent_approval_banner`) so it reads
/// as a standing, always-reachable action on the pane's own chrome rather
/// than something bundled with approve/deny. Mouse-only in this pass (no
/// bare-key binding); danger-accented per `ui::theme::danger()` since it's
/// destructive, and kept visually modest -- a small bordered label, not a
/// filled button -- so it doesn't compete with the header's title/status.
pub(super) fn chrome_cancel_button(
    visible: impl Fn() -> bool + 'static + Copy,
    on_cancel: impl Fn() + 'static,
) -> impl IntoView {
    label(|| "Cancel turn".to_string())
        .on_click_stop(move |_| on_cancel())
        .style(move |s| {
            if !visible() {
                return s.hide();
            }

            s.height(20)
                .margin_left(10)
                .padding_horiz(8)
                .items_center()
                .justify_center()
                .font_size(11)
                .color(theme::danger())
                .border(1.0)
                .border_color(theme::danger())
        })
}

/// `status` is the compact pane-state label (see
/// `agent_controls::agent_pane_status_label`), shown between the title and
/// the close button. Panes with no state to show (terminals, or an agent
/// pane with no session yet) return `None` and the label collapses away —
/// see `docs/ux-principles.md`'s Persistent UI Requirement that the pane
/// header show pane state.
///
/// `cancel_visible` gates `chrome_cancel_button`, shown right after the
/// status label — see that function's doc comment for why "Cancel turn"
/// lives in the header rather than the approval area.
#[allow(clippy::too_many_arguments)]
pub(super) fn pane_header(
    title: impl Fn() -> String + 'static + Copy,
    status: impl Fn() -> Option<String> + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    closeable: impl Fn() -> bool + 'static + Copy,
    cancel_visible: impl Fn() -> bool + 'static + Copy,
    on_cancel: impl Fn() + 'static,
    on_close: impl Fn() + 'static,
) -> impl IntoView {
    h_stack((
        label(title).style(|s| s.min_width(0.0).font_size(13).color(theme::text_primary())),
        label(move || status().unwrap_or_default()).style(move |s| {
            if status().is_none() {
                return s.hide();
            }

            s.min_width(0.0).font_size(11).color(theme::text_muted())
        }),
        chrome_cancel_button(cancel_visible, on_cancel),
        chrome_close_button(closeable, on_close),
    ))
    .style(move |s| {
        let background = if active() {
            theme::surface_selected()
        } else {
            floem::peniko::Color::from_rgb8(32, 36, 45)
        };

        s.width_full()
            .height(35)
            .items_center()
            .gap(10)
            .padding_left(11)
            .padding_right(6)
            .background(background)
    })
}
