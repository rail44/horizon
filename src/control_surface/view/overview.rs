use crate::control_surface::{overview_items, ControlMode, OVERVIEW_VISIBLE_ROWS};
use crate::ui::list_row::{list_row, ListRowStyle};
use crate::ui::selectable_list::selectable_list;
use crate::ui::theme;
use crate::workspace::Workspace;
use floem::event::{Event, EventListener, EventPropagation};
use floem::prelude::*;
use floem::reactive::create_memo;

use super::actions::execute_overview_selection;
use super::chrome::control_mode_tabs;
use super::input::handle_workspace_control_key;
use super::row::overview_item_row;

const OVERVIEW_ROW_HEIGHT: f64 = 52.0;
const OVERVIEW_ROW_STYLE: ListRowStyle = ListRowStyle {
    badge_width: 86.0,
    row_height: OVERVIEW_ROW_HEIGHT,
    padding_horiz: 14.0,
};

pub fn workspace_overview(
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
) -> impl IntoView {
    let items = create_memo(move |_| workspace.with(|ws| overview_items(ws)));

    let list = selectable_list(
        move || items.with(|items| items.len()),
        move || overview_selection.get(),
        move |index| {
            let row = move || items.with(|items| items.get(index).map(overview_item_row));

            list_row(
                row,
                move || overview_selection.get() == index,
                OVERVIEW_ROW_STYLE,
                move || {
                    overview_selection.set(index);
                    execute_overview_selection(workspace, palette_open, overview_selection);
                },
            )
        },
        OVERVIEW_VISIBLE_ROWS as f64 * OVERVIEW_ROW_HEIGHT,
    );

    container(
        v_stack((
            control_mode_tabs(control_mode),
            v_stack((
                label(|| "Workspace Overview".to_string())
                    .style(|s| s.width_full().font_size(16).color(theme::text_primary())),
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
                .style(|s| s.width_full().font_size(12).color(theme::text_muted())),
            ))
            .style(|s| {
                s.width_full()
                    .padding_horiz(14)
                    .padding_vert(12)
                    .gap(4)
                    .background(theme::surface_raised())
            }),
            list,
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
            .border_color(theme::accent())
            .background(theme::surface_base())
    })
}
