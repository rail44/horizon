use std::path::PathBuf;

use floem::prelude::*;
use floem::{
    event::{Event, EventListener, EventPropagation},
    keyboard::{Key, KeyEvent, NamedKey},
};
use horizon::agent_config::AgentConfig;
use horizon::app_commands::{execute_command, request_active_pane_focus, PaneFocusRequests};
use horizon::commands::clamp_palette_selection;
use horizon::control_surface::{
    overview_items, overview_visible_start, palette_items, palette_visible_start, ControlMode,
    OverviewItem, PaletteItem,
};
use horizon::input::palette_accepts_text_input;
use horizon::session::SessionRegistry;
use horizon::session_frames::SessionFrames;
use horizon::workspace::Workspace;

pub(crate) fn command_palette(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
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

fn control_mode_tabs(control_mode: RwSignal<ControlMode>) -> impl IntoView {
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
            .background(floem::peniko::Color::rgb8(25, 28, 34))
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
                floem::peniko::Color::rgb8(233, 236, 242)
            } else {
                floem::peniko::Color::rgb8(178, 185, 198)
            };
            let border = if active {
                floem::peniko::Color::rgb8(132, 220, 198)
            } else {
                floem::peniko::Color::rgb8(54, 59, 70)
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

fn palette_row(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
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

pub(crate) fn workspace_overview(
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

fn handle_palette_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> bool {
    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_palette(palette_open, palette_query);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_palette_selection(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                pane_focus_requests,
                agent_state_status,
                agent_config,
                terminal_dump,
                clipboard_dump,
            );
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_palette_selection(workspace, palette_query, palette_selection, -1);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_palette_selection(workspace, palette_query, palette_selection, 1);
            true
        }
        Key::Named(NamedKey::Backspace) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.pop();
            });
            true
        }
        Key::Named(NamedKey::Space) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.push(' ');
            });
            true
        }
        Key::Character(text) if palette_accepts_text_input(key_event.modifiers) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.push_str(text.as_str());
            });
            true
        }
        _ => false,
    }
}

pub(crate) fn handle_control_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(control_mode);
        return true;
    }

    match control_mode.get_untracked() {
        ControlMode::Commands => handle_palette_key(
            key_event,
            workspace,
            frames,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
            pane_focus_requests,
            agent_state_status,
            agent_config,
            terminal_dump,
            clipboard_dump,
        ),
        ControlMode::Workspace => handle_workspace_control_key(
            key_event,
            workspace,
            palette_open,
            control_mode,
            overview_selection,
        ),
    }
}

fn handle_workspace_control_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(control_mode);
        return true;
    }

    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_control_surface(palette_open);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_overview_selection(workspace, palette_open, overview_selection);
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_overview_selection(workspace, overview_selection, -1);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_overview_selection(workspace, overview_selection, 1);
            true
        }
        _ => false,
    }
}

fn is_control_mode_switch_key(event: &KeyEvent) -> bool {
    matches!(event.key.logical_key, Key::Named(NamedKey::Tab))
}

fn switch_control_mode(control_mode: RwSignal<ControlMode>) {
    control_mode.update(|mode| {
        *mode = match *mode {
            ControlMode::Commands => ControlMode::Workspace,
            ControlMode::Workspace => ControlMode::Commands,
        };
    });
}

pub(crate) fn open_palette(
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
) {
    palette_query.set(String::new());
    palette_selection.set(0);
    palette_open.set(true);
    palette_focus_request.update(|request| *request += 1);
}

fn close_palette(palette_open: RwSignal<bool>, palette_query: RwSignal<String>) {
    palette_open.set(false);
    palette_query.set(String::new());
}

fn close_control_surface(palette_open: RwSignal<bool>) {
    palette_open.set(false);
}

fn execute_overview_selection(
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    overview_selection: RwSignal<usize>,
) {
    let selection = overview_selection.get_untracked();
    let item = workspace.with_untracked(|ws| {
        let items = overview_items(ws);
        items
            .get(clamp_palette_selection(selection, items.len()))
            .cloned()
    });

    let Some(item) = item else {
        return;
    };

    close_control_surface(palette_open);
    workspace.update(|ws| match item {
        OverviewItem::Tab { index, .. } => {
            ws.activate_tab_index(index);
        }
        OverviewItem::DetachedSession { session_id, .. } => {
            ws.attach_existing_session_to_split(session_id);
        }
        OverviewItem::Pane {
            tab_index,
            pane_index,
            ..
        } => {
            ws.activate_pane_index(tab_index, pane_index);
        }
    });
}

fn move_overview_selection(
    workspace: RwSignal<Workspace>,
    overview_selection: RwSignal<usize>,
    delta: isize,
) {
    let item_count = workspace.with_untracked(|ws| overview_items(ws).len());
    if item_count == 0 {
        overview_selection.set(0);
        return;
    }

    overview_selection.update(|selection| {
        let next = (*selection as isize + delta).clamp(0, item_count.saturating_sub(1) as isize);
        *selection = next as usize;
    });
}

fn execute_palette_selection(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    let query = palette_query.get_untracked();
    let selection = palette_selection.get_untracked();
    let item = workspace.with_untracked(|ws| {
        let items = palette_items(ws, &query);
        items
            .get(clamp_palette_selection(selection, items.len()))
            .cloned()
    });

    let Some(item) = item else {
        return;
    };

    if !item.enabled() {
        return;
    }

    close_palette(palette_open, palette_query);
    match item {
        PaletteItem::Command(entry) => execute_command(
            entry.spec.id,
            workspace,
            frames,
            sessions,
            pane_focus_requests,
            agent_state_status,
            agent_config,
            terminal_dump,
            clipboard_dump,
        ),
        PaletteItem::DetachedSession { session_id, .. } => {
            workspace.update(|ws| {
                ws.attach_existing_session_to_split(session_id);
            });
            request_active_pane_focus(workspace, pane_focus_requests);
        }
        PaletteItem::Tab { index, .. } => {
            workspace.update(|ws| {
                ws.activate_tab_index(index);
            });
            request_active_pane_focus(workspace, pane_focus_requests);
        }
    }
}

fn update_palette_query(
    workspace: RwSignal<Workspace>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    update: impl FnOnce(&mut String),
) {
    palette_query.update(update);
    clamp_current_palette_selection(workspace, palette_query, palette_selection);
}

fn clamp_current_palette_selection(
    workspace: RwSignal<Workspace>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
) {
    let query = palette_query.get_untracked();
    let item_count = workspace.with_untracked(|ws| palette_items(ws, &query).len());
    palette_selection.update(|selection| {
        *selection = clamp_palette_selection(*selection, item_count);
    });
}

fn move_palette_selection(
    workspace: RwSignal<Workspace>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    delta: isize,
) {
    let query = palette_query.get_untracked();
    let item_count = workspace.with_untracked(|ws| palette_items(ws, &query).len());
    if item_count == 0 {
        palette_selection.set(0);
        return;
    }

    palette_selection.update(|selection| {
        let next = (*selection as isize + delta).clamp(0, item_count.saturating_sub(1) as isize);
        *selection = next as usize;
    });
}
