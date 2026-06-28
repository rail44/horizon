use floem::prelude::*;
use floem::{
    action::{set_ime_allowed, set_ime_cursor_area},
    event::{Event, EventListener, EventPropagation},
    ext_event::create_signal_from_channel,
    keyboard::{Key, KeyEvent, Modifiers, NamedKey},
    peniko::kurbo::{Point, Size},
    reactive::create_effect,
    window::WindowConfig,
    Application, Clipboard,
};
use horizon::commands::{
    clamp_palette_selection, command_enabled, command_entries, filter_command_entries,
    CommandEntry, CommandId, CommandState,
};
use horizon::session::SessionRegistry;
use horizon::terminal::{
    TerminalCommand, TerminalFrame, TerminalSession, TerminalSize, TerminalUpdate,
};
use horizon::workspace::{PaneKind, SessionId, SessionKind, Workspace};
use std::path::PathBuf;
use termwiz::input::{KeyCode as TermKeyCode, Modifiers as TermModifiers};

mod terminal_view;

const PALETTE_VISIBLE_ROWS: usize = 6;
const OVERVIEW_VISIBLE_ROWS: usize = 8;

#[derive(Clone, Debug)]
enum PaletteItem {
    Command(CommandEntry),
    DetachedSession {
        session_id: SessionId,
        kind: SessionKind,
        display_number: usize,
        title: String,
    },
    Tab {
        index: usize,
        title: String,
        pane_count: usize,
        active: bool,
    },
}

#[derive(Clone, Debug)]
enum OverviewItem {
    Tab {
        index: usize,
        title: String,
        pane_count: usize,
        active: bool,
    },
    DetachedSession {
        session_id: SessionId,
        title: String,
        kind: SessionKind,
        display_number: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlMode {
    Commands,
    Workspace,
}

fn main() {
    Application::new()
        .window(
            |_| app_view(),
            Some(
                WindowConfig::default()
                    .title("Horizon")
                    .size((1100.0, 720.0)),
            ),
        )
        .run();
}

fn app_view() -> impl IntoView {
    let workspace = RwSignal::new(Workspace::mvp());
    let sessions = RwSignal::new(SessionRegistry::default());
    let ime_composing = RwSignal::new(false);
    let ime_preedit = RwSignal::new(None::<String>);
    let ime_cursor_area = RwSignal::new((Point::new(12.0, 64.0), Size::new(8.0, 18.0)));
    let palette_open = RwSignal::new(false);
    let palette_query = RwSignal::new(String::new());
    let palette_selection = RwSignal::new(0_usize);
    let palette_focus_request = RwSignal::new(0_u64);
    let control_mode = RwSignal::new(ControlMode::Commands);
    let overview_selection = RwSignal::new(0_usize);
    let terminal_dump = std::env::var_os("HORIZON_TERMINAL_DUMP").map(PathBuf::from);
    let clipboard_dump = std::env::var_os("HORIZON_CLIPBOARD_DUMP").map(PathBuf::from);
    let status_dump = std::env::var_os("HORIZON_STATUS_DUMP").map(PathBuf::from);

    for session_id in workspace.with(|ws| ws.terminal_session_ids()) {
        spawn_terminal_session(
            session_id,
            workspace,
            sessions,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        );
    }

    stack((
        v_stack((
            tab_strip(workspace, sessions),
            workspace_view(
                workspace,
                sessions,
                ime_composing,
                ime_preedit,
                ime_cursor_area,
                palette_open,
                palette_query,
                palette_selection,
                palette_focus_request,
                control_mode,
                overview_selection,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            status_bar(workspace, status_dump),
        ))
        .style(|s| s.size_full().flex().flex_col()),
        command_palette(
            workspace,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
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
        set_ime_allowed(active_terminal(workspace));
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
        if !active_terminal(workspace) {
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
        if !active_terminal(workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImeCommit(text) = event {
            let (position, size) = ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("commit text={text:?}"));
            ime_composing.set(false);
            ime_preedit.set(None);
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
                    sessions,
                    palette_open,
                    palette_query,
                    palette_selection,
                    control_mode,
                    overview_selection,
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

fn spawn_terminal_session(
    session_id: SessionId,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    match TerminalSession::spawn(TerminalSize::default()) {
        Ok(session) => {
            sessions.update(|registry| {
                registry.insert_terminal(session_id, session.sender());
            });
            let updates = create_signal_from_channel(session.updates());
            create_effect(move |_| {
                if let Some(update) = updates.get() {
                    match update {
                        TerminalUpdate::Snapshot(output) => {
                            if let Some(path) = &terminal_dump {
                                let _ = std::fs::write(path, &output.text);
                            }
                            workspace.update(|ws| ws.update_terminal_frame(session_id, output));
                        }
                        TerminalUpdate::Error(error) => {
                            workspace.update(|ws| {
                                ws.update_terminal_output(
                                    session_id,
                                    format!("Terminal error: {error}"),
                                )
                            });
                        }
                        TerminalUpdate::Exited => {
                            workspace.update(|ws| {
                                ws.update_terminal_output(session_id, "Terminal exited".to_string())
                            });
                        }
                        TerminalUpdate::Title(_) | TerminalUpdate::Bell => {}
                        TerminalUpdate::Clipboard(text) => {
                            if let Some(path) = &clipboard_dump {
                                let _ = std::fs::write(path, &text);
                            }
                            let _ = Clipboard::set_contents(text);
                        }
                    }
                }
            });
        }
        Err(error) => {
            workspace.update(|ws| {
                ws.update_terminal_output(session_id, format!("Terminal error: {error}"))
            });
        }
    }
}

fn command_palette(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
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
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                0,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                1,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                2,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                3,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                4,
                terminal_dump.clone(),
                clipboard_dump.clone(),
            ),
            palette_row(
                workspace,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                5,
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
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                control_mode,
                overview_selection,
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
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    index: usize,
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
            sessions,
            palette_open,
            palette_query,
            palette_selection,
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
                            "{} tab(s) · {} visible pane(s) · {} detached session(s)",
                            ws.tab_count(),
                            ws.visible_panes().len(),
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

fn execute_command(
    command_id: CommandId,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    let state = workspace.with_untracked(command_state);
    if !command_enabled(command_id, state) {
        return;
    }

    match command_id {
        CommandId::NewTerminal => {
            open_terminal_tab(workspace, sessions, terminal_dump, clipboard_dump)
        }
        CommandId::NewAgent => {
            workspace.update(|ws| {
                ws.open_tab(PaneKind::Agent, None);
            });
        }
        CommandId::SplitActivePane => {
            split_active_pane(workspace, sessions, terminal_dump, clipboard_dump);
        }
        CommandId::FocusNextPane => {
            workspace.update(Workspace::focus_next);
        }
        CommandId::CloseActivePane => {
            let index = workspace.with_untracked(|ws| ws.active_visible_index());
            close_visible_pane(workspace, sessions, index);
        }
        CommandId::CloseActiveTab => {
            let index = workspace.with_untracked(|ws| ws.active_tab_index());
            close_tab(workspace, sessions, index);
        }
        CommandId::TerminateActiveSession => {
            terminate_active_session(workspace, sessions);
        }
    }
}

fn handle_palette_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
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
                sessions,
                palette_open,
                palette_query,
                palette_selection,
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
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> bool {
    if let Some(reverse) = control_mode_switch_direction(key_event) {
        switch_control_mode(control_mode, reverse);
        return true;
    }

    match control_mode.get_untracked() {
        ControlMode::Commands => handle_palette_key(
            key_event,
            workspace,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
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
    if let Some(reverse) = control_mode_switch_direction(key_event) {
        switch_control_mode(control_mode, reverse);
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

fn control_mode_switch_direction(event: &KeyEvent) -> Option<bool> {
    match event.key.logical_key {
        Key::Named(NamedKey::ArrowLeft) => Some(true),
        Key::Named(NamedKey::ArrowRight) => Some(false),
        _ => None,
    }
}

fn switch_control_mode(control_mode: RwSignal<ControlMode>, _reverse: bool) {
    control_mode.update(|mode| {
        *mode = match *mode {
            ControlMode::Commands => ControlMode::Workspace,
            ControlMode::Workspace => ControlMode::Commands,
        };
    });
}

fn command_state(workspace: &Workspace) -> CommandState {
    CommandState {
        tab_count: workspace.tab_count(),
        visible_pane_count: workspace.visible_panes().len(),
        has_active_session: workspace.active_session_id().is_some(),
    }
}

impl PaletteItem {
    fn kind_label(&self) -> String {
        match self {
            Self::Command(_) => "COMMAND".to_string(),
            Self::DetachedSession { .. } => "SESSION".to_string(),
            Self::Tab { .. } => "TAB".to_string(),
        }
    }

    fn kind_color(&self) -> floem::peniko::Color {
        match self {
            Self::Command(_) => floem::peniko::Color::rgb8(132, 220, 198),
            Self::DetachedSession { .. } => floem::peniko::Color::rgb8(126, 170, 255),
            Self::Tab { .. } => floem::peniko::Color::rgb8(224, 184, 104),
        }
    }

    fn title(&self) -> String {
        match self {
            Self::Command(entry) => entry.spec.title.to_string(),
            Self::DetachedSession { title, .. } => format!("Attach {title}"),
            Self::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Command(entry) => entry.spec.description.to_string(),
            Self::DetachedSession {
                kind,
                display_number,
                ..
            } => {
                format!(
                    "Detached {} session #{}; attach to the active tab as a split.",
                    session_kind_label(*kind),
                    display_number
                )
            }
            Self::Tab {
                pane_count, active, ..
            } => {
                if *active {
                    format!("Current tab with {pane_count} pane(s).")
                } else {
                    format!("Switch to tab with {pane_count} pane(s).")
                }
            }
        }
    }

    fn enabled(&self) -> bool {
        match self {
            Self::Command(entry) => entry.enabled,
            Self::DetachedSession { .. } | Self::Tab { .. } => true,
        }
    }
}

impl OverviewItem {
    fn kind_label(&self) -> String {
        match self {
            Self::Tab { .. } => "TAB".to_string(),
            Self::DetachedSession { .. } => "DETACHED".to_string(),
        }
    }

    fn kind_color(&self) -> floem::peniko::Color {
        match self {
            Self::Tab { .. } => floem::peniko::Color::rgb8(224, 184, 104),
            Self::DetachedSession { .. } => floem::peniko::Color::rgb8(126, 170, 255),
        }
    }

    fn title(&self) -> String {
        match self {
            Self::Tab { index, title, .. } => format!("Tab {}: {title}", index + 1),
            Self::DetachedSession { title, .. } => format!("Attach {title}"),
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Tab {
                pane_count, active, ..
            } => {
                if *active {
                    format!("Current tab · {pane_count} pane(s)")
                } else {
                    format!("Switch to tab · {pane_count} pane(s)")
                }
            }
            Self::DetachedSession {
                kind,
                display_number,
                ..
            } => format!(
                "Detached {} session #{} · Enter attaches as split",
                session_kind_label(*kind),
                display_number
            ),
        }
    }
}

fn overview_items(workspace: &Workspace) -> Vec<OverviewItem> {
    let mut items: Vec<_> = workspace
        .tab_summaries()
        .into_iter()
        .map(|tab| OverviewItem::Tab {
            index: tab.index,
            title: tab.title,
            pane_count: tab.pane_count,
            active: tab.active,
        })
        .collect();

    items.extend(
        workspace
            .detached_session_summaries()
            .into_iter()
            .map(|session| OverviewItem::DetachedSession {
                session_id: session.id,
                title: session.title,
                kind: session.kind,
                display_number: session.display_number,
            }),
    );

    items
}

fn overview_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= OVERVIEW_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(OVERVIEW_VISIBLE_ROWS - 1)
}

fn palette_items(workspace: &Workspace, query: &str) -> Vec<PaletteItem> {
    let mut items: Vec<_> =
        filter_command_entries(command_entries(command_state(workspace)), query)
            .into_iter()
            .map(PaletteItem::Command)
            .collect();
    let query = normalize_palette_query(query);

    items.extend(
        workspace
            .detached_session_summaries()
            .into_iter()
            .filter(|session| {
                let display_number = session.display_number.to_string();
                palette_matches(
                    &query,
                    &[
                        "detached",
                        "session",
                        session.title.as_str(),
                        session_kind_label(session.kind),
                        display_number.as_str(),
                    ],
                )
            })
            .map(|session| PaletteItem::DetachedSession {
                session_id: session.id,
                kind: session.kind,
                display_number: session.display_number,
                title: session.title,
            }),
    );

    items.extend(
        workspace
            .tab_summaries()
            .into_iter()
            .filter(|tab| {
                let index_label = format!("tab {}", tab.index + 1);
                palette_matches(
                    &query,
                    &["tab", index_label.as_str(), tab.title.as_str(), "switch"],
                )
            })
            .map(|tab| PaletteItem::Tab {
                index: tab.index,
                title: tab.title,
                pane_count: tab.pane_count,
                active: tab.active,
            }),
    );

    items
}

fn palette_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= PALETTE_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(PALETTE_VISIBLE_ROWS - 1)
}

fn palette_matches(query: &str, fields: &[&str]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .any(|field| normalize_palette_query(field).contains(query))
}

fn normalize_palette_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn session_kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "terminal",
        SessionKind::Agent => "agent",
    }
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
    sessions: RwSignal<SessionRegistry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
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
            sessions,
            terminal_dump,
            clipboard_dump,
        ),
        PaletteItem::DetachedSession { session_id, .. } => {
            workspace.update(|ws| {
                ws.attach_existing_session_to_split(session_id);
            });
        }
        PaletteItem::Tab { index, .. } => {
            workspace.update(|ws| {
                ws.activate_tab_index(index);
            });
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

fn open_terminal_tab(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    let session_id = SessionId::new();
    workspace.update(|ws| {
        ws.open_tab(PaneKind::Terminal, Some(session_id));
    });
    spawn_terminal_session(
        session_id,
        workspace,
        sessions,
        terminal_dump,
        clipboard_dump,
    );
}

fn split_active_pane(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    let kind = workspace.with_untracked(|ws| {
        ws.active_terminal_session_id()
            .map(|_| PaneKind::Terminal)
            .unwrap_or(PaneKind::Agent)
    });
    workspace.update(|ws| {
        if kind == PaneKind::Terminal {
            ws.split_active(PaneKind::Terminal, Some(SessionId::new()));
        } else {
            ws.split_active(PaneKind::Agent, None);
        }
    });
    if kind == PaneKind::Terminal {
        let Some(session_id) = workspace.with_untracked(|ws| ws.active_terminal_session_id())
        else {
            return;
        };
        spawn_terminal_session(
            session_id,
            workspace,
            sessions,
            terminal_dump,
            clipboard_dump,
        );
    }
}

fn terminate_active_session(workspace: RwSignal<Workspace>, sessions: RwSignal<SessionRegistry>) {
    let Some(session_id) = workspace.with_untracked(|ws| ws.active_session_id()) else {
        return;
    };

    workspace.update(|ws| {
        ws.terminate_session(session_id);
    });
    sessions.update(|registry| {
        registry.shutdown_terminal(session_id);
    });
}

fn tab_strip(workspace: RwSignal<Workspace>, sessions: RwSignal<SessionRegistry>) -> impl IntoView {
    h_stack((
        tab_button(workspace, sessions, 0),
        tab_button(workspace, sessions, 1),
        tab_button(workspace, sessions, 2),
        tab_button(workspace, sessions, 3),
        tab_button(workspace, sessions, 4),
        tab_button(workspace, sessions, 5),
    ))
    .style(|s| {
        s.width_full()
            .height(34)
            .items_center()
            .gap(4)
            .padding_horiz(8)
            .background(floem::peniko::Color::rgb8(25, 28, 34))
    })
}

fn tab_button(
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
        button(label(title)).action(move || {
            workspace.update(|ws| {
                ws.activate_tab_index(index);
            });
        }),
        button("x")
            .action(move || close_tab(workspace, sessions, index))
            .style(move |s| if closeable() { s } else { s.hide() }),
    ))
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let background = if active() {
            floem::peniko::Color::rgb8(54, 59, 70)
        } else {
            floem::peniko::Color::rgb8(37, 40, 48)
        };
        let border = if active() {
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(54, 59, 70)
        };
        s.height(26)
            .items_center()
            .gap(4)
            .padding_horiz(4)
            .font_size(12)
            .color(floem::peniko::Color::rgb8(233, 236, 242))
            .background(background)
            .border(1.0)
            .border_color(border)
    })
}

fn active_terminal(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| {
        ws.visible_panes()
            .get(ws.active_visible_index())
            .is_some_and(|pane| pane.kind == PaneKind::Terminal)
    })
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

fn close_visible_pane(
    workspace: RwSignal<Workspace>,
    _sessions: RwSignal<SessionRegistry>,
    index: usize,
) {
    workspace.update(|ws| {
        ws.close_visible_pane(index);
    });
}

fn close_tab(workspace: RwSignal<Workspace>, _sessions: RwSignal<SessionRegistry>, index: usize) {
    workspace.update(|ws| {
        ws.close_tab_index(index);
    });
}

fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}

fn workspace_view(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> impl IntoView {
    h_stack((
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            0,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        ),
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            1,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        ),
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            2,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        ),
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            3,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            control_mode,
            overview_selection,
            terminal_dump,
            clipboard_dump,
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
    sessions: RwSignal<SessionRegistry>,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    index: usize,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> impl IntoView {
    let focus_request = RwSignal::new(0_u64);

    let output = move || {
        workspace.with(|ws| {
            ws.visible_panes()
                .get(index)
                .map(|pane| pane.output.clone())
                .unwrap_or_else(|| "No split yet".to_string())
        })
    };

    let terminal_frame = move || {
        workspace.with(|ws| {
            ws.visible_panes()
                .get(index)
                .and_then(|pane| pane.terminal_frame.clone())
                .unwrap_or_else(|| TerminalFrame::from_text(output()))
        })
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

    v_stack((
        h_stack((
            label(title).style(|s| {
                s.font_size(13)
                    .color(floem::peniko::Color::rgb8(233, 236, 242))
            }),
            label(move || {
                if active() {
                    "active".to_string()
                } else {
                    String::new()
                }
            })
            .style(|s| {
                s.font_size(11)
                    .color(floem::peniko::Color::rgb8(132, 220, 198))
            }),
            button("x")
                .action(move || close_visible_pane(workspace, sessions, index))
                .style(move |s| if closeable() { s } else { s.hide() }),
        ))
        .style(|s| {
            s.width_full()
                .height(34)
                .items_center()
                .gap(10)
                .padding_horiz(10)
                .background(floem::peniko::Color::rgb8(37, 40, 48))
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
                    .is_some_and(|pane| pane.kind == PaneKind::Terminal)
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
                    .is_some_and(|pane| pane.kind == PaneKind::Terminal)
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
                    sessions,
                    palette_open,
                    palette_query,
                    palette_selection,
                    control_mode,
                    overview_selection,
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
    .style(|s| {
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

fn terminal_input_from_key(event: &KeyEvent) -> Option<Vec<u8>> {
    match &event.key.logical_key {
        Key::Character(text) => character_input(text.as_str(), event.modifiers),
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

fn terminal_key_from_key(event: &KeyEvent) -> Option<TermKeyCode> {
    terminal_key_from_input(&event.key.logical_key)
}

fn terminal_key_from_input(key: &Key) -> Option<TermKeyCode> {
    match key {
        Key::Named(NamedKey::Enter) => Some(TermKeyCode::Enter),
        Key::Named(NamedKey::Tab) => Some(TermKeyCode::Tab),
        Key::Named(NamedKey::Backspace) => Some(TermKeyCode::Backspace),
        Key::Named(NamedKey::Escape) => Some(TermKeyCode::Escape),
        Key::Named(NamedKey::ArrowUp) => Some(TermKeyCode::UpArrow),
        Key::Named(NamedKey::ArrowDown) => Some(TermKeyCode::DownArrow),
        Key::Named(NamedKey::ArrowRight) => Some(TermKeyCode::RightArrow),
        Key::Named(NamedKey::ArrowLeft) => Some(TermKeyCode::LeftArrow),
        Key::Named(NamedKey::Home) => Some(TermKeyCode::Home),
        Key::Named(NamedKey::End) => Some(TermKeyCode::End),
        Key::Named(NamedKey::PageUp) => Some(TermKeyCode::PageUp),
        Key::Named(NamedKey::PageDown) => Some(TermKeyCode::PageDown),
        Key::Named(NamedKey::Delete) => Some(TermKeyCode::Delete),
        _ => None,
    }
}

fn termwiz_modifiers(modifiers: Modifiers) -> TermModifiers {
    let mut term_modifiers = TermModifiers::NONE;
    if modifiers.shift() {
        term_modifiers |= TermModifiers::SHIFT;
    }
    if modifiers.control() {
        term_modifiers |= TermModifiers::CTRL;
    }
    if modifiers.alt() {
        term_modifiers |= TermModifiers::ALT;
    }
    if modifiers.meta() {
        term_modifiers |= TermModifiers::SUPER;
    }
    term_modifiers
}

fn is_terminal_paste_key(event: &KeyEvent) -> bool {
    is_terminal_paste_input(&event.key.logical_key, event.modifiers)
}

fn is_palette_open_key(event: &KeyEvent) -> bool {
    match &event.key.logical_key {
        Key::Character(text) => event.modifiers.control() && text.eq_ignore_ascii_case("p"),
        _ => false,
    }
}

fn palette_accepts_text_input(modifiers: Modifiers) -> bool {
    !modifiers.control() && !modifiers.alt() && !modifiers.meta()
}

fn is_terminal_paste_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Paste) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("v")
        }
        _ => false,
    }
}

fn is_terminal_copy_key(event: &KeyEvent) -> bool {
    is_terminal_copy_input(&event.key.logical_key, event.modifiers)
}

fn is_terminal_copy_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Copy) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("c")
        }
        _ => false,
    }
}

fn scroll_lines_from_wheel(delta_y: f64) -> Option<i32> {
    if delta_y.abs() < f64::EPSILON {
        return None;
    }

    Some(if delta_y > 0.0 { 3 } else { -3 })
}

fn character_input(text: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let single_char = chars.next().is_none();

    if modifiers.control() && single_char {
        return control_input(first);
    }

    if modifiers.meta() {
        return None;
    }

    let mut bytes = Vec::new();
    if modifiers.alt() {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

fn control_input(c: char) -> Option<Vec<u8>> {
    let c = c.to_ascii_lowercase();
    let byte = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(vec![byte])
}

fn status_bar(workspace: RwSignal<Workspace>, status_dump: Option<PathBuf>) -> impl IntoView {
    label(move || {
        workspace.with(|ws| {
            let status = format!(
                "{} tab(s), {} pane(s), {} detached session(s), active: {}, active pane: {} | Ctrl+Shift+P: control surface",
                ws.tab_count(),
                ws.visible_panes().len(),
                ws.detached_session_count(),
                ws.active_title(),
                ws.active_visible_index() + 1
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_input_keeps_space() {
        assert_eq!(
            character_input(" ", Modifiers::default()),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn character_input_keeps_utf8_text() {
        assert_eq!(
            character_input("日本語", Modifiers::default()),
            Some("日本語".as_bytes().to_vec())
        );
    }

    #[test]
    fn control_space_input_is_nul() {
        assert_eq!(control_input(' '), None);
    }

    #[test]
    fn wheel_up_scrolls_into_history() {
        assert_eq!(scroll_lines_from_wheel(1.0), Some(3));
    }

    #[test]
    fn wheel_down_scrolls_to_bottom() {
        assert_eq!(scroll_lines_from_wheel(-1.0), Some(-3));
    }

    #[test]
    fn command_state_reflects_workspace_counts() {
        let mut workspace = Workspace::mvp();
        assert_eq!(
            command_state(&workspace),
            horizon::commands::CommandState {
                tab_count: 1,
                visible_pane_count: 1,
                has_active_session: true,
            }
        );

        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        assert_eq!(
            command_state(&workspace),
            horizon::commands::CommandState {
                tab_count: 1,
                visible_pane_count: 2,
                has_active_session: true,
            }
        );
    }

    #[test]
    fn palette_items_include_detached_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);

        let items = palette_items(&workspace, "detached");

        assert!(items.iter().any(|item| matches!(
            item,
            PaletteItem::DetachedSession {
                session_id: id,
                kind: SessionKind::Terminal,
                ..
            } if *id == session_id
        )));
    }

    #[test]
    fn palette_items_include_tabs_by_index() {
        let mut workspace = Workspace::mvp();
        workspace.open_tab(PaneKind::Agent, None);

        let items = palette_items(&workspace, "tab 1");

        assert!(items.iter().any(|item| matches!(
            item,
            PaletteItem::Tab {
                index: 0,
                title,
                active: false,
                ..
            } if title == "Terminal #1"
        )));
    }

    #[test]
    fn overview_items_include_tabs_and_detached_sessions() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.split_active(PaneKind::Terminal, Some(session_id));
        workspace.close_visible_pane(1);

        let items = overview_items(&workspace);

        assert!(matches!(
            items[0],
            OverviewItem::Tab {
                index: 0,
                active: true,
                ..
            }
        ));
        assert!(items.iter().any(|item| matches!(
            item,
            OverviewItem::DetachedSession {
                session_id: id,
                title,
                ..
            } if *id == session_id && title == "Terminal #2"
        )));
    }

    #[test]
    fn overview_visible_start_keeps_selection_in_rendered_rows() {
        assert_eq!(overview_visible_start(0, 12), 0);
        assert_eq!(overview_visible_start(7, 12), 0);
        assert_eq!(overview_visible_start(8, 12), 1);
        assert_eq!(overview_visible_start(11, 12), 4);
    }

    #[test]
    fn palette_visible_start_keeps_selection_in_rendered_rows() {
        assert_eq!(palette_visible_start(0, 10), 0);
        assert_eq!(palette_visible_start(5, 10), 0);
        assert_eq!(palette_visible_start(6, 10), 1);
        assert_eq!(palette_visible_start(9, 10), 4);
    }

    #[test]
    fn palette_visible_start_handles_short_lists() {
        assert_eq!(palette_visible_start(0, 0), 0);
        assert_eq!(palette_visible_start(3, 4), 0);
    }

    #[test]
    fn ctrl_shift_v_is_terminal_paste() {
        assert!(is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_v_remains_terminal_control_input() {
        assert!(!is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("v", Modifiers::CONTROL), Some(vec![0x16]));
    }

    #[test]
    fn ctrl_shift_c_is_terminal_copy() {
        assert!(is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_c_remains_terminal_control_input() {
        assert!(!is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("c", Modifiers::CONTROL), Some(vec![0x03]));
    }

    #[test]
    fn named_arrow_uses_termwiz_key_path() {
        assert_eq!(
            terminal_key_from_input(&Key::Named(NamedKey::ArrowUp)),
            Some(TermKeyCode::UpArrow)
        );
    }

    #[test]
    fn modifiers_convert_to_termwiz() {
        assert_eq!(
            termwiz_modifiers(Modifiers::CONTROL | Modifiers::SHIFT),
            TermModifiers::CTRL | TermModifiers::SHIFT
        );
    }
}
