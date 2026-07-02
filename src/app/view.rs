use floem::event::EventListener;
use floem::prelude::*;

use crate::control_surface::view::{command_palette, workspace_overview};
use crate::workspace::view::{tab_strip, workspace_view};

use super::context::{command_palette_state, workspace_overview_state, workspace_view_state};
use super::input::AppInput;
use super::state::AppState;
use super::status_bar::status_bar;

pub fn app_view() -> impl IntoView {
    let state = AppState::new();
    state.spawn_initial_sessions();

    let input = AppInput::new(&state);
    let content = app_content(state);

    let focus_input = input.clone();
    let ime_enabled_input = input.clone();
    let ime_disabled_input = input.clone();
    let ime_preedit_input = input.clone();
    let ime_commit_input = input.clone();
    let key_input = input.clone();

    content
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

fn app_content(state: AppState) -> impl IntoView {
    let workspace = state.workspace;
    let agent_state_status = state.agent_state_status;
    let status_dump = state.status_dump.clone();

    stack((
        v_stack((
            tab_strip(workspace),
            workspace_view(workspace_view_state(&state)),
            status_bar(workspace, agent_state_status, status_dump),
        ))
        .style(|s| s.size_full().flex().flex_col()),
        command_palette(command_palette_state(&state)),
        workspace_overview(workspace_overview_state(&state)),
    ))
}
