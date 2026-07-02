use std::path::PathBuf;

use floem::prelude::*;
use floem::{
    event::EventListener,
    peniko::kurbo::{Point, Size},
};

use crate::agent_config::AgentConfig;
use crate::app::runtime::{spawn_agent_session, spawn_terminal_session};
use crate::control_surface::view::{command_palette, workspace_overview};
use crate::control_surface::ControlMode;
use crate::session::{Frames, Registry};
use crate::workspace::view::{tab_strip, workspace_view};
use crate::workspace::Workspace;

use super::input::AppInput;
use super::status_bar::status_bar;

pub fn app_view() -> impl IntoView {
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

    let input = AppInput::new(
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
        control_mode,
        overview_selection,
        agent_state_status,
        agent_config.clone(),
        terminal_dump.clone(),
        clipboard_dump.clone(),
    );

    let focus_input = input.clone();
    let ime_enabled_input = input.clone();
    let ime_disabled_input = input.clone();
    let ime_preedit_input = input.clone();
    let ime_commit_input = input.clone();
    let key_input = input.clone();

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
        focus_input.handle_window_focus()
    })
    .on_event(EventListener::ImeEnabled, move |_| {
        ime_enabled_input.handle_ime_enabled()
    })
    .on_event(EventListener::ImeDisabled, move |_| {
        ime_disabled_input.handle_ime_disabled()
    })
    .on_event(EventListener::ImePreedit, move |event| {
        ime_preedit_input.handle_ime_preedit(event)
    })
    .on_event(EventListener::ImeCommit, move |event| {
        ime_commit_input.handle_ime_commit(event)
    })
    .keyboard_navigable()
    .on_event(EventListener::KeyDown, move |event| {
        key_input.handle_key_down(event)
    })
    .style(move |s| {
        s.size_full()
            .background(floem::peniko::Color::rgb8(22, 24, 29))
    })
}
