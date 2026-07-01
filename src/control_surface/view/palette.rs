use std::path::PathBuf;

use crate::agent_config::AgentConfig;
use crate::app::commands::PaneFocusRequests;
use crate::control_surface::{palette_items, palette_visible_start, ControlMode};
use crate::session::Frames;
use crate::session::Registry;
use crate::workspace::Workspace;
use floem::event::{Event, EventListener, EventPropagation};
use floem::prelude::*;

use super::actions::execute_palette_selection;
use super::chrome::control_mode_tabs;
use super::input::handle_control_key;

pub fn command_palette(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> impl IntoView {
    let terminal_dump_for_key = terminal_dump.clone();
    let clipboard_dump_for_key = clipboard_dump.clone();

    container(
        v_stack((
            control_mode_tabs(control_mode),
            label(move || {
                let query = palette_query.get();
                if query.is_empty() {
                    "> Search commands, sessions, tabs".to_string()
                } else {
                    format!("> {query}")
                }
            })
            .style(|s| {
                s.width_full()
                    .height(38)
                    .items_center()
                    .padding_horiz(12)
                    .font_size(14)
                    .color(floem::peniko::Color::rgb8(233, 236, 242))
                    .background(floem::peniko::Color::rgb8(31, 34, 41))
            }),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                0,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                1,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                2,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                3,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                4,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                5,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump,
                clipboard_dump,
            ),
        ))
        .style(|s| s.width_full()),
    )
    .keyboard_navigable()
    .request_focus(move || {
        palette_focus_request.get();
    })
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            if handle_control_key(
                key_event,
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                control_mode,
                overview_selection,
                pane_focus_requests,
                agent_state_status,
                agent_config.clone(),
                terminal_dump_for_key.clone(),
                clipboard_dump_for_key.clone(),
            ) {
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Stop
    })
    .style(move |s| {
        if !palette_open.get() || control_mode.get() != ControlMode::Commands {
            return s.hide();
        }

        s.absolute()
            .inset_top(74.0)
            .inset_left(240.0)
            .width(620)
            .z_index(10)
            .border(1.0)
            .border_color(floem::peniko::Color::rgb8(132, 220, 198))
            .background(floem::peniko::Color::rgb8(22, 24, 29))
    })
}

fn palette_row(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    index: usize,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> impl IntoView {
    let item = move || {
        let query = palette_query.get();
        workspace.with(|ws| {
            let items = palette_items(ws, &query);
            let start = palette_visible_start(palette_selection.get(), items.len());
            items.get(start + index).cloned()
        })
    };
    let item_index = move || {
        let query = palette_query.get();
        workspace.with(|ws| {
            let item_count = palette_items(ws, &query).len();
            palette_visible_start(palette_selection.get(), item_count) + index
        })
    };
    let selected = move || palette_selection.get() == item_index();

    h_stack((
        label(move || item().map(|item| item.kind_label()).unwrap_or_default()).style(move |s| {
            let Some(item) = item() else {
                return s.hide();
            };

            s.width(72)
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
        palette_selection.set(item_index());
        execute_palette_selection(
            workspace,
            frames,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
            pane_focus_requests,
            agent_state_status,
            agent_config.clone(),
            terminal_dump.clone(),
            clipboard_dump.clone(),
        );
    })
    .style(move |s| {
        let Some(item) = item() else {
            return s.hide();
        };

        let background = if selected() {
            floem::peniko::Color::rgb8(54, 59, 70)
        } else {
            floem::peniko::Color::rgb8(22, 24, 29)
        };
        let text_color = if item.enabled() {
            floem::peniko::Color::rgb8(233, 236, 242)
        } else {
            floem::peniko::Color::rgb8(115, 122, 136)
        };

        s.width_full()
            .height(48)
            .items_center()
            .gap(10)
            .padding_horiz(12)
            .padding_vert(6)
            .background(background)
            .color(text_color)
    })
}
