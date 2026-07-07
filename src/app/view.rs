use floem::event::EventListener;
use floem::prelude::*;

use crate::control_surface::view::{command_palette, session_manager_modal};
use crate::workspace::request_active_pane_focus;
use crate::workspace::view::{tab_strip, workspace_view};

use super::command_actions::{execute_command, CommandInvocation};
use super::commands::CommandId;
use super::context::{command_action_state, command_palette_state, workspace_view_state};
use super::input::AppInput;
use super::state::AppState;
use super::status_bar::status_bar;

pub fn app_view() -> impl IntoView {
    let state = AppState::new();
    state.spawn_initial_sessions();

    let input = AppInput::new(&state);
    let workspace = state.workspace;
    let pane_focus_requests = state.pane_focus_requests.clone();
    wire_config_reload_requests(&state);
    let content = app_content(state);

    // Every pane_view's own `request_focus` (floem's `request_focus`
    // decorator, `workspace::view::pane`) fires unconditionally the instant
    // it's created -- regardless of the tracked signal's value -- so
    // building every currently-visible pane above always ends the mount
    // with the *last-created* pane holding keyboard focus, not necessarily
    // the actually-active one. Left alone, no click ever lands because
    // nothing owns focus in the first place. Bumping the active pane's own
    // request here -- after every pane's initial effect has already run --
    // re-triggers just that one pane's `request_focus` effect (each pane
    // only tracks its own entry in `pane_focus_requests`) and wins the race
    // last, so the workspace starts with real keyboard focus on the pane
    // the user actually sees as active.
    request_active_pane_focus(workspace, pane_focus_requests);

    let focus_input = input.clone();
    let lost_focus_input = input.clone();
    let ime_enabled_input = input.clone();
    let ime_disabled_input = input.clone();
    let ime_preedit_input = input.clone();
    let ime_commit_input = input.clone();
    let key_input = input.clone();

    content
        // Each handler below is wrapped in `profiling::timed`, Horizon's
        // opt-in (`HORIZON_UI_PROFILE`) UI-thread event-timing capture --
        // see `crate::profiling`'s module doc for what "frame" means here
        // (event-handling latency, not floem's internal paint pass, which
        // this global `on_event` chain can't observe). A no-op wrapper
        // (one cached bool check) when capture is disabled, which it is by
        // default.
        .on_event(EventListener::WindowGotFocus, move |_| {
            crate::profiling::timed("WindowGotFocus", || focus_input.handle_window_focus())
        })
        .on_event(EventListener::WindowLostFocus, move |_| {
            crate::profiling::timed("WindowLostFocus", || {
                lost_focus_input.handle_window_lost_focus()
            })
        })
        .on_event(EventListener::ImeEnabled, move |_| {
            crate::profiling::timed("ImeEnabled", || ime_enabled_input.handle_ime_enabled())
        })
        .on_event(EventListener::ImeDisabled, move |_| {
            crate::profiling::timed("ImeDisabled", || ime_disabled_input.handle_ime_disabled())
        })
        .on_event(EventListener::ImePreedit, move |event| {
            crate::profiling::timed("ImePreedit", || ime_preedit_input.handle_ime_preedit(event))
        })
        .on_event(EventListener::ImeCommit, move |event| {
            crate::profiling::timed("ImeCommit", || ime_commit_input.handle_ime_commit(event))
        })
        .keyboard_navigable()
        .on_event(EventListener::KeyDown, move |event| {
            crate::profiling::timed("KeyDown", || key_input.handle_key_down(event))
        })
        .style(move |s| {
            s.size_full()
                .background(floem::peniko::Color::from_rgb8(22, 24, 29))
        })
}

/// Answers a `config_reload_requests` bump (a session's successful
/// `config.write`, observed by `agent::agentd_runtime::
/// fold_agent_session_events`) by executing the `Reload Config` command --
/// the same `execute_command` path the palette/keybinding/CLI use, so the
/// automatic reload is just another binding to the command, not a second
/// reload implementation (AGENTS.md, "Operations go through the command
/// model"). The previous-value comparison skips the effect's initial run
/// (counter still at its startup value), so merely mounting the app never
/// reloads anything.
fn wire_config_reload_requests(state: &AppState) {
    let requests = state.config_reload_requests;
    let action_state = command_action_state(state);
    floem::reactive::create_effect(move |previous: Option<u64>| {
        let count = requests.get();
        if let Some(previous) = previous {
            if count > previous {
                execute_command(
                    CommandInvocation::Simple(CommandId::ReloadConfig),
                    action_state.clone(),
                );
            }
        }
        count
    });
}

fn app_content(state: AppState) -> impl IntoView {
    let workspace = state.workspace;
    let agent_state_status = state.agent_state_status;
    let status_dump = state.status_dump.clone();

    stack((
        v_stack((
            tab_strip(command_action_state(&state)),
            workspace_view(workspace_view_state(&state)),
            status_bar(workspace, agent_state_status, status_dump),
        ))
        .style(|s| s.size_full().flex().flex_col()),
        command_palette(command_palette_state(&state)),
        session_manager_modal(command_action_state(&state)),
    ))
}
