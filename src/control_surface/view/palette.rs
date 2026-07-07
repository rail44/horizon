use crate::control_surface::items::palette_rows;
use crate::control_surface::{ControlInputState, PALETTE_VISIBLE_ROWS};
use crate::ui::list_row::{list_row, ListRowStyle};
use crate::ui::selectable_list::selectable_list;
use crate::ui::spacing;
use crate::ui::theme;
use floem::event::{Event, EventListener, EventPropagation};
use floem::prelude::*;
use floem::reactive::create_memo;

use super::row::palette_row_view;
use crate::control_surface::actions::{execute_palette_selection, PaletteActionState};
use crate::control_surface::handle_control_key;
use crate::control_surface::PaletteStage;

const PALETTE_ROW_HEIGHT: f64 = 48.0;
const PALETTE_ROW_STYLE: ListRowStyle = ListRowStyle {
    badge_width: 72.0,
    row_height: PALETTE_ROW_HEIGHT,
    padding_horiz: spacing::SPACING_MD,
};

#[derive(Clone)]
pub(crate) struct CommandPaletteState {
    pub(crate) control_input: ControlInputState,
    pub(crate) palette_focus_request: RwSignal<u64>,
}

impl CommandPaletteState {
    fn control_input_state(&self) -> ControlInputState {
        self.control_input.clone()
    }

    fn palette_action_state(&self) -> PaletteActionState {
        self.control_input_state().palette_action_state()
    }
}

pub(crate) fn command_palette(state: CommandPaletteState) -> impl IntoView {
    let control_input = state.control_input_state();
    let palette_action = state.palette_action_state();

    let workspace = control_input.command.workspace();
    let frames = control_input.command.frames();
    let palette_open = control_input.palette_open;
    let palette_query = control_input.palette_query;
    let palette_selection = control_input.palette_selection;
    let palette_stage = control_input.command.palette.palette_stage;
    let palette_focus_request = state.palette_focus_request;

    let items = create_memo(move |_| {
        let query = palette_query.get();
        let stage = palette_stage.get();
        workspace.with(|ws| frames.with(|fr| palette_rows(ws, fr, stage, &query)))
    });

    let list = selectable_list(
        move || items.with(|items| items.len()),
        move || palette_selection.get(),
        move |index| {
            let row = move || items.with(|items| items.get(index).map(palette_row_view));
            let palette_action = palette_action.clone();

            list_row(
                row,
                move || palette_selection.get() == index,
                PALETTE_ROW_STYLE,
                move || {
                    palette_selection.set(index);
                    execute_palette_selection(palette_action.clone());
                },
            )
        },
        PALETTE_VISIBLE_ROWS as f64 * PALETTE_ROW_HEIGHT,
    );

    container(
        v_stack((
            label(move || {
                let query = palette_query.get();
                match palette_stage.get() {
                    PaletteStage::Commands => {
                        if query.is_empty() {
                            "> Search commands, sessions, tabs".to_string()
                        } else {
                            format!("> {query}")
                        }
                    }
                    PaletteStage::ViewChooser { placement } => {
                        if query.is_empty() {
                            format!("> {}: choose a view", placement.label())
                        } else {
                            format!("> {query}")
                        }
                    }
                }
            })
            .style(|s| {
                s.width_full()
                    .height(38)
                    .items_center()
                    .padding_horiz(spacing::SPACING_MD)
                    .font_size(14)
                    .color(theme::text_primary())
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
            if handle_control_key(key_event, control_input.clone()) {
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Stop
    })
    .style(move |s| {
        if !palette_open.get() {
            return s.hide();
        }

        s.absolute()
            .inset_top(74.0)
            .inset_left(240.0)
            .width(620)
            .z_index(10)
            .border(1.0)
            .border_color(theme::accent())
            .background(theme::surface_base())
    })
}
