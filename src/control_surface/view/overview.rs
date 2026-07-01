use crate::control_surface::{overview_items, overview_visible_start, ControlMode};
use crate::workspace::Workspace;
use floem::event::{Event, EventListener, EventPropagation};
use floem::prelude::*;

use super::actions::execute_overview_selection;
use super::chrome::control_mode_tabs;
use super::input::handle_workspace_control_key;

pub fn workspace_overview(
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
) -> impl IntoView {
    container(
        v_stack((
            control_mode_tabs(control_mode),
            v_stack((
                label(|| "Workspace Overview".to_string()).style(|s| {
                    s.width_full()
                        .font_size(16)
                        .color(floem::peniko::Color::rgb8(233, 236, 242))
                }),
                label(move || {
                    workspace.with(|ws| {
                        format!(
                            "{} tab(s) · {} visible pane(s) · {} session(s), {} detached",
                            ws.tab_count(),
                            ws.visible_panes().len(),
                            ws.session_count(),
                            ws.detached_session_count()
                        )
                    })
                })
                .style(|s| {
                    s.width_full()
                        .font_size(12)
                        .color(floem::peniko::Color::rgb8(178, 185, 198))
                }),
            ))
            .style(|s| {
                s.width_full()
                    .padding_horiz(14)
                    .padding_vert(12)
                    .gap(4)
                    .background(floem::peniko::Color::rgb8(31, 34, 41))
            }),
            overview_row(workspace, palette_open, overview_selection, 0),
            overview_row(workspace, palette_open, overview_selection, 1),
            overview_row(workspace, palette_open, overview_selection, 2),
            overview_row(workspace, palette_open, overview_selection, 3),
            overview_row(workspace, palette_open, overview_selection, 4),
            overview_row(workspace, palette_open, overview_selection, 5),
            overview_row(workspace, palette_open, overview_selection, 6),
            overview_row(workspace, palette_open, overview_selection, 7),
        ))
        .style(|s| s.width_full()),
    )
    .keyboard_navigable()
    .request_focus(move || {
        palette_focus_request.get();
    })
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            if handle_workspace_control_key(
                key_event,
                workspace,
                palette_open,
                control_mode,
                overview_selection,
            ) {
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Stop
    })
    .style(move |s| {
        if !palette_open.get() || control_mode.get() != ControlMode::Workspace {
            return s.hide();
        }

        s.absolute()
            .inset_top(74.0)
            .inset_left(240.0)
            .width(680)
            .z_index(10)
            .border(1.0)
            .border_color(floem::peniko::Color::rgb8(132, 220, 198))
            .background(floem::peniko::Color::rgb8(22, 24, 29))
    })
}

fn overview_row(
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    overview_selection: RwSignal<usize>,
    index: usize,
) -> impl IntoView {
    let item = move || {
        workspace.with(|ws| {
            let items = overview_items(ws);
            let start = overview_visible_start(overview_selection.get(), items.len());
            items.get(start + index).cloned()
        })
    };
    let item_index = move || {
        workspace.with(|ws| {
            let item_count = overview_items(ws).len();
            overview_visible_start(overview_selection.get(), item_count) + index
        })
    };
    let selected = move || overview_selection.get() == item_index();

    h_stack((
        label(move || item().map(|item| item.kind_label()).unwrap_or_default()).style(move |s| {
            let Some(item) = item() else {
                return s.hide();
            };

            s.width(86)
                .height(22)
                .items_center()
                .justify_center()
                .font_size(10)
                .border(1.0)
                .border_color(item.kind_color())
                .color(item.kind_color())
        }),
        v_stack((
            label(move || item().map(|item| item.title()).unwrap_or_default()).style(|s| {
                s.width_full()
                    .font_size(13)
                    .color(floem::peniko::Color::rgb8(233, 236, 242))
            }),
            label(move || item().map(|item| item.description()).unwrap_or_default()).style(|s| {
                s.width_full()
                    .font_size(11)
                    .color(floem::peniko::Color::rgb8(178, 185, 198))
            }),
        ))
        .style(|s| {
            s.flex()
                .flex_col()
                .min_width(0.0)
                .flex_basis(0.0)
                .flex_grow(1.0)
        }),
    ))
    .on_click_stop(move |_| {
        overview_selection.set(item_index());
        execute_overview_selection(workspace, palette_open, overview_selection);
    })
    .style(move |s| {
        let Some(_) = item() else {
            return s.hide();
        };

        let background = if selected() {
            floem::peniko::Color::rgb8(54, 59, 70)
        } else {
            floem::peniko::Color::rgb8(22, 24, 29)
        };

        s.width_full()
            .height(52)
            .items_center()
            .gap(10)
            .padding_horiz(14)
            .padding_vert(6)
            .background(background)
    })
}
