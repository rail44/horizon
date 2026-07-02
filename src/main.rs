use floem::prelude::*;
use floem::{
    action::{set_ime_allowed, set_ime_cursor_area},
    event::{Event, EventListener, EventPropagation},
    peniko::kurbo::{Point, Size},
    window::WindowConfig,
    Application,
};
use horizon::agent_config::AgentConfig;
use horizon::app::commands::{active_agent, active_text_input_pane};
use horizon::app::runtime::{spawn_agent_session, spawn_terminal_session};
use horizon::control_surface::{handle_control_key, open_palette, ControlMode};
use horizon::input::is_palette_open_key;
use horizon::session::Frames;
use horizon::session::Registry;
use horizon::terminal::TerminalCommand;
use horizon::workspace::{active_agent_draft, active_terminal_sender, trace_ime, Workspace};
use std::path::PathBuf;

mod status_bar;

use horizon::control_surface::view::{command_palette, workspace_overview};
use horizon::workspace::view::{tab_strip, workspace_view};
use status_bar::status_bar;

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
    let frames = RwSignal::new(Frames::default());
    let sessions = RwSignal::new(Registry::default());
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
