use floem::prelude::*;

pub(super) fn chrome_close_button(
    visible: impl Fn() -> bool + 'static + Copy,
    on_close: impl Fn() + 'static + Copy,
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
                .color(floem::peniko::Color::rgb8(170, 178, 190))
                .background(floem::peniko::Color::rgb8(35, 39, 48))
                .border(1.0)
                .border_color(floem::peniko::Color::rgb8(57, 64, 76))
        })
}

pub(super) fn pane_header(
    title: impl Fn() -> String + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    closeable: impl Fn() -> bool + 'static + Copy,
    on_close: impl Fn() + 'static + Copy,
) -> impl IntoView {
    h_stack((
        label(title).style(|s| {
            s.min_width(0.0)
                .font_size(13)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
        }),
        chrome_close_button(closeable, on_close),
    ))
    .style(move |s| {
        let background = if active() {
            floem::peniko::Color::rgb8(39, 44, 54)
        } else {
            floem::peniko::Color::rgb8(32, 36, 45)
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
