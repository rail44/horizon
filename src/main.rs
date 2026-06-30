use floem::prelude::*;
use floem::{
    action::{set_ime_allowed, set_ime_cursor_area},
    event::{Event, EventListener, EventPropagation},
    keyboard::{Key, KeyEvent, NamedKey},
    peniko::kurbo::{Point, Size},
    window::WindowConfig,
    Application, Clipboard,
};
use horizon::agent::{AgentCommand, AgentFrame, AgentToolCallId};
use horizon::agent_config::AgentConfig;
use horizon::app_commands::{
    active_agent, active_text_input_pane, close_tab, close_visible_pane, execute_command,
    request_active_pane_focus, PaneFocusRequests, MAX_VISIBLE_PANES,
};
use horizon::app_runtime::{spawn_agent_session, spawn_terminal_session};
use horizon::commands::clamp_palette_selection;
use horizon::control_surface::{
    overview_items, overview_visible_start, palette_items, palette_visible_start, ControlMode,
    OverviewItem, PaletteItem,
};
use horizon::fonts::HORIZON_FONT_FAMILY;
use horizon::input::{
    agent_draft_action, is_palette_open_key, is_terminal_copy_key, is_terminal_paste_key,
    palette_accepts_text_input, pop_last_grapheme_approx, terminal_input_from_key,
    terminal_key_from_key, termwiz_modifiers, AgentDraftAction,
};
use horizon::session::SessionRegistry;
use horizon::session_frames::SessionFrames;
use horizon::terminal::{TerminalCommand, TerminalFrame};
use horizon::workspace::{PaneKind, Workspace};
use std::path::PathBuf;

mod agent_view;
mod terminal_view;

type AgentDrafts = [RwSignal<String>; MAX_VISIBLE_PANES];

fn main() {
    Application::new()
        .window(
            |_| app_view(),
            Some(
                WindowConfig::default()
                    .title("Horizon")
                    .size((1100.0, 720.0))
                    .show_titlebar(true)
                    .undecorated(false),
            ),
        )
        .run();
}

fn app_view() -> impl IntoView {
    let workspace = RwSignal::new(Workspace::mvp());
    let frames = RwSignal::new(SessionFrames::default());
    let sessions = RwSignal::new(SessionRegistry::default());
    let ime_composing = RwSignal::new(false);
    let ime_preedit = RwSignal::new(None::<String>);
    let ime_cursor_area = RwSignal::new((Point::new(12.0, 64.0), Size::new(8.0, 18.0)));
    let palette_open = RwSignal::new(false);
    let palette_query = RwSignal::new(String::new());
    let palette_selection = RwSignal::new(0_usize);
    let palette_focus_request = RwSignal::new(0_u64);
    let pane_focus_requests = [
        RwSignal::new(0_u64),
        RwSignal::new(0_u64),
        RwSignal::new(0_u64),
        RwSignal::new(0_u64),
    ];
    let agent_drafts = [
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    ];
    let control_mode = RwSignal::new(ControlMode::Commands);
    let overview_selection = RwSignal::new(0_usize);
    let agent_state_status = RwSignal::new(None::<String>);
    let agent_config = AgentConfig::from_env();
    let terminal_dump = std::env::var_os("HORIZON_TERMINAL_DUMP").map(PathBuf::from);
    let clipboard_dump = std::env::var_os("HORIZON_CLIPBOARD_DUMP").map(PathBuf::from);
    let status_dump = std::env::var_os("HORIZON_STATUS_DUMP").map(PathBuf::from);

    for session_id in workspace.with(|ws| ws.terminal_session_ids()) {
        spawn_terminal_session(
            session_id,
            frames,
            sessions,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        );
    }
    for session_id in workspace.with(|ws| ws.agent_session_ids()) {
        spawn_agent_session(
            session_id,
            workspace,
            frames,
            sessions,
            agent_state_status,
            agent_config.clone(),
        );
    }

    stack((
        v_stack((
            tab_strip(workspace, sessions),
            workspace_view(
                workspace,
                frames,
                sessions,
                ime_composing,
                ime_preedit,
                ime_cursor_area,
                palette_open,
                palette_query,
                palette_selection,
                palette_focus_request,
                pane_focus_requests,
                agent_drafts,
                agent_config.clone(),
                control_mode,
                overview_selection,
                terminal_dump.clone(),
                clipboard_dump.clone(),
                agent_state_status,
            ),
            status_bar(workspace, agent_state_status, status_dump),
        ))
        .style(|s| s.size_full().flex().flex_col()),
        command_palette(
            workspace,
            frames,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests,
            agent_state_status,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        ),
        workspace_overview(
            workspace,
            palette_open,
            control_mode,
            overview_selection,
            palette_focus_request,
        ),
    ))
    .on_event(EventListener::WindowGotFocus, move |_| {
        set_ime_allowed(active_text_input_pane(workspace));
        let (position, size) = ime_cursor_area.get_untracked();
        set_ime_cursor_area(position, size);
        EventPropagation::Continue
    })
    .on_event(EventListener::ImeEnabled, move |_| {
        trace_ime("enabled");
        EventPropagation::Continue
    })
    .on_event(EventListener::ImeDisabled, move |_| {
        trace_ime("disabled");
        EventPropagation::Continue
    })
    .on_event(EventListener::ImePreedit, move |event| {
        if !active_text_input_pane(workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImePreedit { text, cursor } = event {
            let (position, size) = ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("preedit text={text:?} cursor={cursor:?}"));
            if text.is_empty() {
                ime_composing.set(false);
                ime_preedit.set(None);
            } else {
                ime_composing.set(true);
                ime_preedit.set(Some(text.clone()));
            }
            return EventPropagation::Stop;
        }

        EventPropagation::Continue
    })
    .on_event(EventListener::ImeCommit, move |event| {
        if !active_text_input_pane(workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImeCommit(text) = event {
            let (position, size) = ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("commit text={text:?}"));
            ime_composing.set(false);
            ime_preedit.set(None);
            if active_agent(workspace) {
                if let Some(draft) = active_agent_draft(workspace, agent_drafts) {
                    draft.update(|draft| draft.push_str(text));
                    return EventPropagation::Stop;
                }
            }
            if let Some(tx) = active_terminal_sender(workspace, sessions) {
                let _ = tx.send(TerminalCommand::Input(text.as_bytes().to_vec()));
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    })
    .keyboard_navigable()
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            if palette_open.get_untracked() {
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
                    terminal_dump.clone(),
                    clipboard_dump.clone(),
                ) {
                    return EventPropagation::Stop;
                }
            }

            if is_palette_open_key(key_event) {
                ime_composing.set(false);
                ime_preedit.set(None);
                set_ime_allowed(false);
                control_mode.set(ControlMode::Commands);
                open_palette(
                    palette_open,
                    palette_query,
                    palette_selection,
                    palette_focus_request,
                );
                return EventPropagation::Stop;
            }
        }
        EventPropagation::Continue
    })
    .style(move |s| {
        s.size_full()
            .background(floem::peniko::Color::rgb8(22, 24, 29))
    })
}

fn command_palette(
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

fn workspace_overview(
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

fn handle_control_key(
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

fn open_palette(
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

fn tab_strip(workspace: RwSignal<Workspace>, sessions: RwSignal<SessionRegistry>) -> impl IntoView {
    h_stack((
        tab_chip(workspace, sessions, 0),
        tab_chip(workspace, sessions, 1),
        tab_chip(workspace, sessions, 2),
        tab_chip(workspace, sessions, 3),
        tab_chip(workspace, sessions, 4),
        tab_chip(workspace, sessions, 5),
    ))
    .style(|s| {
        s.width_full()
            .height(35)
            .items_center()
            .gap(6)
            .padding_horiz(10)
            .background(floem::peniko::Color::rgb8(21, 24, 30))
    })
}

fn tab_chip(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    index: usize,
) -> impl IntoView {
    let exists = move || workspace.with(|ws| ws.tab_summaries().get(index).is_some());
    let active = move || {
        workspace.with(|ws| {
            ws.tab_summaries()
                .get(index)
                .is_some_and(|summary| summary.active)
        })
    };
    let title = move || {
        workspace.with(|ws| {
            ws.tab_summaries()
                .get(index)
                .map(|summary| {
                    format!(
                        "{}: {} [{}]",
                        summary.index + 1,
                        summary.title,
                        summary.pane_count
                    )
                })
                .unwrap_or_default()
        })
    };
    let closeable = move || workspace.with(|ws| ws.tab_count() > 1);

    h_stack((
        label(title).style(|s| {
            s.max_width(170)
                .font_size(12)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
        }),
        chrome_close_button(
            move || closeable(),
            move || close_tab(workspace, sessions, index),
        ),
    ))
    .on_click_stop(move |_| {
        workspace.update(|ws| {
            ws.activate_tab_index(index);
        });
    })
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let background = if active() {
            floem::peniko::Color::rgb8(39, 44, 54)
        } else {
            floem::peniko::Color::rgb8(21, 24, 30)
        };
        let border = if active() {
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(42, 46, 55)
        };
        s.height(27)
            .min_width(0.0)
            .items_center()
            .gap(7)
            .padding_left(10)
            .padding_right(3)
            .background(background)
            .border(1.0)
            .border_color(border)
    })
}

fn chrome_close_button(
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

fn pane_header(
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

fn active_agent_draft(
    workspace: RwSignal<Workspace>,
    agent_drafts: AgentDrafts,
) -> Option<RwSignal<String>> {
    if !active_agent(workspace) {
        return None;
    }

    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    agent_drafts.get(index).copied()
}

fn active_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.active_terminal_session_id())?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

fn pane_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_terminal_session_id(index))?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

fn pane_agent_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<AgentCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_agent_session_id(index))?;
    sessions.with_untracked(|registry| registry.agent_sender(session_id))
}

fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}

fn workspace_view(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    pane_focus_requests: PaneFocusRequests,
    agent_drafts: AgentDrafts,
    agent_config: AgentConfig,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agent_state_status: RwSignal<Option<String>>,
) -> impl IntoView {
    h_stack((
        pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            0,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[0],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            1,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[1],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            2,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[2],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            3,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[3],
            pane_focus_requests,
            agent_drafts,
            agent_config,
            control_mode,
            overview_selection,
            terminal_dump,
            clipboard_dump,
            agent_state_status,
        ),
    ))
    .style(|s| {
        s.flex()
            .flex_row()
            .width_full()
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .gap(1)
            .padding(1)
            .background(floem::peniko::Color::rgb8(42, 46, 55))
    })
}

fn pane_view(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<SessionFrames>,
    sessions: RwSignal<SessionRegistry>,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    index: usize,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    focus_request: RwSignal<u64>,
    pane_focus_requests: PaneFocusRequests,
    agent_drafts: AgentDrafts,
    agent_config: AgentConfig,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agent_state_status: RwSignal<Option<String>>,
) -> impl IntoView {
    let terminal_frame = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_terminal_session_id(index)) else {
            return TerminalFrame::from_text("No split yet".to_string());
        };
        frames.with(|frames| frames.terminal_frame(session_id))
    };
    let agent_frame = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_agent_session_id(index)) else {
            return AgentFrame::empty();
        };
        frames.with(|frames| frames.agent_frame(session_id))
    };

    let title = move || {
        workspace.with(|ws| {
            ws.visible_pane_title(index)
                .unwrap_or_else(|| "Empty".to_string())
        })
    };

    let active = move || workspace.with(|ws| ws.active_visible_index() == index);
    let exists = move || workspace.with(|ws| ws.visible_panes().get(index).is_some());
    let closeable = move || workspace.with(|ws| ws.visible_panes().len() > 1);
    let is_agent = move || {
        workspace.with(|ws| {
            ws.visible_panes()
                .get(index)
                .is_some_and(|pane| pane.kind == PaneKind::Agent)
        })
    };
    let is_terminal = move || {
        workspace.with(|ws| {
            ws.visible_panes()
                .get(index)
                .is_some_and(|pane| pane.kind == PaneKind::Terminal)
        })
    };
    let pending_approval = move || {
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        frames.with(|frames| frames.agent_frame(session_id).pending_approval_call_id())
    };
    let agent_draft = agent_drafts[index];

    v_stack((
        pane_header(title, active, closeable, move || {
            close_visible_pane(workspace, sessions, index)
        }),
        terminal_output(
            terminal_frame,
            move || {
                if active() {
                    ime_preedit.get()
                } else {
                    None
                }
            },
            pane_terminal_sender(workspace, sessions, index),
            ime_cursor_area,
            is_terminal,
        ),
        agent_view::agent_frame_view(agent_frame, is_agent),
        agent_approval_actions(
            is_agent,
            pending_approval,
            move |call_id| {
                if let Some(tx) = pane_agent_sender(workspace, sessions, index) {
                    let _ = tx.send(AgentCommand::ApproveToolCall { call_id });
                }
            },
            move |call_id| {
                if let Some(tx) = pane_agent_sender(workspace, sessions, index) {
                    let _ = tx.send(AgentCommand::DenyToolCall {
                        call_id,
                        reason: Some("Denied by user".to_string()),
                    });
                }
            },
        ),
        agent_composer(
            is_agent,
            active,
            agent_draft,
            move || {
                if active() && is_agent() {
                    ime_preedit.get()
                } else {
                    None
                }
            },
            ime_cursor_area,
        ),
    ))
    .style(|s| {
        s.flex()
            .flex_col()
            .size_full()
            .min_width(0.0)
            .justify_start()
    })
    .keyboard_navigable()
    .request_focus(move || {
        focus_request.get();
    })
    .on_event(EventListener::PointerDown, move |_| {
        focus_request.update(|request| *request += 1);
        workspace.update(|ws| {
            ws.activate_visible_pane(index);
        });
        if workspace.with(|ws| {
            ws.active_visible_index() == index
                && ws
                    .visible_panes()
                    .get(index)
                    .is_some_and(|pane| matches!(pane.kind, PaneKind::Terminal | PaneKind::Agent))
        }) {
            set_ime_allowed(true);
        }
        EventPropagation::Stop
    })
    .on_event(EventListener::FocusGained, move |_| {
        set_ime_allowed(workspace.with(|ws| {
            ws.active_visible_index() == index
                && ws
                    .visible_panes()
                    .get(index)
                    .is_some_and(|pane| matches!(pane.kind, PaneKind::Terminal | PaneKind::Agent))
        }));
        EventPropagation::Continue
    })
    .on_event(EventListener::FocusLost, move |_| {
        ime_composing.set(false);
        ime_preedit.set(None);
        set_ime_allowed(false);
        EventPropagation::Continue
    })
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            if palette_open.get_untracked() {
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
                    terminal_dump.clone(),
                    clipboard_dump.clone(),
                ) {
                    return EventPropagation::Stop;
                }
            }

            if is_palette_open_key(key_event) {
                ime_composing.set(false);
                ime_preedit.set(None);
                set_ime_allowed(false);
                control_mode.set(ControlMode::Commands);
                open_palette(
                    palette_open,
                    palette_query,
                    palette_selection,
                    palette_focus_request,
                );
                return EventPropagation::Stop;
            }
        }

        if !workspace.with(|ws| {
            ws.active_visible_index() == index
                && ws
                    .visible_panes()
                    .get(index)
                    .is_some_and(|pane| pane.kind == PaneKind::Terminal)
        }) {
            if let Event::KeyDown(key_event) = event {
                if ime_composing.get_untracked()
                    && matches!(key_event.key.logical_key, Key::Character(_))
                {
                    return EventPropagation::Stop;
                }

                if workspace.with(|ws| {
                    ws.active_visible_index() == index
                        && ws
                            .visible_panes()
                            .get(index)
                            .is_some_and(|pane| pane.kind == PaneKind::Agent)
                }) && handle_agent_key(
                    key_event,
                    agent_draft,
                    pane_agent_sender(workspace, sessions, index),
                ) {
                    return EventPropagation::Stop;
                }
            }
            return EventPropagation::Continue;
        }

        if let Event::KeyDown(key_event) = event {
            if ime_composing.get_untracked()
                && matches!(key_event.key.logical_key, Key::Character(_))
            {
                return EventPropagation::Stop;
            }

            if is_terminal_paste_key(key_event) {
                if let (Some(tx), Ok(text)) = (
                    pane_terminal_sender(workspace, sessions, index),
                    Clipboard::get_contents(),
                ) {
                    let _ = tx.send(TerminalCommand::Paste(text));
                    return EventPropagation::Stop;
                }
            }

            if is_terminal_copy_key(key_event) {
                if let Some(tx) = pane_terminal_sender(workspace, sessions, index) {
                    let _ = tx.send(TerminalCommand::CopySelection);
                    return EventPropagation::Stop;
                }
            }

            if let Some(key) = terminal_key_from_key(key_event) {
                if let Some(tx) = pane_terminal_sender(workspace, sessions, index) {
                    let _ = tx.send(TerminalCommand::Key {
                        key,
                        modifiers: termwiz_modifiers(key_event.modifiers),
                        is_down: true,
                    });
                    return EventPropagation::Stop;
                }
            }

            if let Some(bytes) = terminal_input_from_key(key_event) {
                if let Some(tx) = pane_terminal_sender(workspace, sessions, index) {
                    let _ = tx.send(TerminalCommand::Input(bytes));
                    return EventPropagation::Stop;
                }
            }
        }

        EventPropagation::Continue
    })
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let border = if active() {
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(54, 59, 70)
        };
        s.height_full()
            .min_width(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .background(floem::peniko::Color::rgb8(24, 27, 32))
            .border(1.0)
            .border_color(border)
    })
}

fn terminal_output(
    output: impl Fn() -> TerminalFrame + Copy + 'static,
    preedit: impl Fn() -> Option<String> + 'static,
    terminal_tx: Option<crossbeam_channel::Sender<TerminalCommand>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    visible: impl Fn() -> bool + 'static,
) -> impl IntoView {
    let terminal_origin = RwSignal::new(floem::peniko::kurbo::Point::ZERO);
    terminal_view::terminal_text_view(
        output,
        preedit,
        terminal_tx,
        move || terminal_origin.get(),
        move |position, size| ime_cursor_area.set((position, size)),
    )
    .on_move(move |origin| terminal_origin.set(origin))
    .style(move |s| {
        if !visible() {
            return s.hide();
        }

        s.absolute()
            .inset_left(0.0)
            .inset_right(0.0)
            .inset_top(34.0)
            .inset_bottom(0.0)
            .width_full()
            .height_full()
            .min_width(0.0)
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
    })
}

fn agent_composer(
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

fn agent_approval_actions(
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<AgentToolCallId> + 'static + Copy,
    on_approve: impl Fn(AgentToolCallId) + 'static + Copy,
    on_deny: impl Fn(AgentToolCallId) + 'static + Copy,
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
    pending_approval: impl Fn() -> Option<AgentToolCallId> + 'static + Copy,
    on_click: impl Fn(AgentToolCallId) + 'static + Copy,
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

fn handle_agent_key(
    event: &KeyEvent,
    draft: RwSignal<String>,
    agent_tx: Option<crossbeam_channel::Sender<AgentCommand>>,
) -> bool {
    if is_terminal_paste_key(event) {
        if let Ok(text) = Clipboard::get_contents() {
            draft.update(|draft| draft.push_str(&text));
            return true;
        }
    }

    match agent_draft_action(&event.key.logical_key, event.modifiers) {
        Some(AgentDraftAction::Insert(text)) => {
            draft.update(|draft| draft.push_str(&text));
            true
        }
        Some(AgentDraftAction::Backspace) => {
            draft.update(|draft| {
                pop_last_grapheme_approx(draft);
            });
            true
        }
        Some(AgentDraftAction::Submit) => {
            let text = draft.with_untracked(|draft| draft.trim().to_string());
            if text.is_empty() {
                return true;
            }
            if let Some(tx) = agent_tx {
                let command = AgentCommand::UserMessage { text };
                let _ = tx.send(command);
                draft.set(String::new());
            }
            true
        }
        None => false,
    }
}

fn status_bar(
    workspace: RwSignal<Workspace>,
    agent_state_status: RwSignal<Option<String>>,
    status_dump: Option<PathBuf>,
) -> impl IntoView {
    label(move || {
        let agent_state_status = agent_state_status.get();
        workspace.with(|ws| {
            let status = status_bar_text(ws, agent_state_status.as_deref());
            if let Some(path) = &status_dump {
                let _ = std::fs::write(path, &status);
            }
            status
        })
    })
    .style(|s| {
        s.width_full()
            .height(26)
            .padding_horiz(10)
            .items_center()
            .font_size(12)
            .color(floem::peniko::Color::rgb8(178, 185, 198))
            .background(floem::peniko::Color::rgb8(31, 34, 41))
    })
}

fn status_bar_text(workspace: &Workspace, agent_state_status: Option<&str>) -> String {
    let base = format!(
        "{} tab(s), {} pane(s), {} detached session(s), active: {}, active pane: {} | Ctrl+Shift+P: control surface",
        workspace.tab_count(),
        workspace.visible_panes().len(),
        workspace.detached_session_count(),
        workspace.active_title(),
        workspace.active_visible_index() + 1
    );
    match agent_state_status {
        Some(status) if !status.is_empty() => format!("{base} | {status}"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_text_includes_agent_state_diagnostic() {
        let workspace = Workspace::mvp();
        let status = status_bar_text(&workspace, Some("Agent state: /tmp/horizon.duckdb"));

        assert!(status.contains("Ctrl+Shift+P: control surface"));
        assert!(status.contains("Agent state: /tmp/horizon.duckdb"));
    }
}
