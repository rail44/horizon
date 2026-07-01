use crate::control_surface::ControlMode;
use crate::ui::theme;
use floem::prelude::*;

pub(super) fn control_mode_tabs(control_mode: RwSignal<ControlMode>) -> impl IntoView {
    h_stack((
        control_mode_tab(control_mode, ControlMode::Commands, "Commands"),
        control_mode_tab(control_mode, ControlMode::Workspace, "Workspace"),
    ))
    .style(|s| {
        s.width_full()
            .height(34)
            .items_center()
            .gap(8)
            .padding_horiz(12)
            .background(theme::surface_chrome())
    })
}

fn control_mode_tab(
    control_mode: RwSignal<ControlMode>,
    mode: ControlMode,
    title: &'static str,
) -> impl IntoView {
    label(move || title.to_string())
        .on_click_stop(move |_| {
            control_mode.set(mode);
        })
        .style(move |s| {
            let active = control_mode.get() == mode;
            let color = if active {
                theme::text_primary()
            } else {
                theme::text_muted()
            };
            let border = if active {
                theme::accent()
            } else {
                theme::border_default()
            };

            s.height(24)
                .padding_horiz(10)
                .items_center()
                .font_size(12)
                .color(color)
                .border(1.0)
                .border_color(border)
        })
}
