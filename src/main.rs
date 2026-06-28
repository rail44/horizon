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
use horizon::session::SessionRegistry;
use horizon::terminal::{
    TerminalCommand, TerminalFrame, TerminalSession, TerminalSize, TerminalUpdate,
};
use horizon::workspace::{PaneKind, SessionId, Workspace};
use std::path::PathBuf;
use termwiz::input::{KeyCode as TermKeyCode, Modifiers as TermModifiers};

mod terminal_view;

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

    v_stack((
        toolbar(
            workspace,
            sessions,
            terminal_dump.clone(),
            clipboard_dump.clone(),
        ),
        tab_strip(workspace, sessions),
        workspace_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
        ),
        status_bar(workspace, status_dump),
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
    .style(|s| {
        s.size_full()
            .flex()
            .flex_col()
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

fn toolbar(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> impl IntoView {
    let terminal_dump_for_open = terminal_dump.clone();
    let terminal_dump_for_split = terminal_dump.clone();
    let clipboard_dump_for_open = clipboard_dump.clone();
    let clipboard_dump_for_split = clipboard_dump.clone();
    h_stack((
        button("Terminal").action(move || {
            let session_id = SessionId::new();
            workspace.update(|ws| {
                ws.open_tab(PaneKind::Terminal, Some(session_id));
            });
            spawn_terminal_session(
                session_id,
                workspace,
                sessions,
                terminal_dump_for_open.clone(),
                clipboard_dump_for_open.clone(),
            );
        }),
        button("Agent").action(move || {
            workspace.update(|ws| {
                ws.open_tab(PaneKind::Agent, None);
            });
        }),
        button("Split").action(move || {
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
                let Some(session_id) =
                    workspace.with_untracked(|ws| ws.active_terminal_session_id())
                else {
                    return;
                };
                spawn_terminal_session(
                    session_id,
                    workspace,
                    sessions,
                    terminal_dump_for_split.clone(),
                    clipboard_dump_for_split.clone(),
                );
            }
        }),
        button("Next").action(move || {
            workspace.update(Workspace::focus_next);
        }),
    ))
    .style(|s| {
        s.width_full()
            .height(44)
            .items_center()
            .gap(8)
            .padding_horiz(12)
            .background(floem::peniko::Color::rgb8(31, 34, 41))
    })
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
    sessions: RwSignal<SessionRegistry>,
    index: usize,
) {
    let mut closed_session = None;
    let mut still_referenced = false;
    workspace.update(|ws| {
        closed_session = ws.close_visible_pane(index);
        if let Some(session_id) = closed_session {
            still_referenced = ws.session_is_referenced(session_id);
        }
    });

    if let Some(session_id) = closed_session {
        sessions.update(|registry| {
            registry.shutdown_terminal_if_unreferenced(session_id, still_referenced);
        });
    }
}

fn close_tab(workspace: RwSignal<Workspace>, sessions: RwSignal<SessionRegistry>, index: usize) {
    let mut closed_sessions = Vec::new();
    let mut session_references = Vec::new();
    workspace.update(|ws| {
        closed_sessions = ws.close_tab_index(index);
        session_references = closed_sessions
            .iter()
            .map(|session_id| (*session_id, ws.session_is_referenced(*session_id)))
            .collect();
    });

    sessions.update(|registry| {
        for (session_id, is_referenced) in session_references {
            registry.shutdown_terminal_if_unreferenced(session_id, is_referenced);
        }
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
) -> impl IntoView {
    h_stack((
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            0,
        ),
        pane_view(
            workspace,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            1,
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
            ws.visible_panes()
                .get(index)
                .map(|pane| pane.title())
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
                "{} tab(s), {} pane(s), active: {}, active pane: {}",
                ws.tab_count(),
                ws.visible_panes().len(),
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
