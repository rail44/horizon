use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::commands::{clamp_palette_selection, CommandId};
use crate::control_surface::{palette_items, PaletteItem};
use crate::session::{Frames, SessionId};
use crate::workspace::Workspace;
use floem::prelude::*;

#[derive(Clone)]
pub(crate) struct PaletteActionState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct OpenPaletteState {
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
    pub(crate) palette_focus_request: RwSignal<u64>,
}

pub(crate) fn open_palette(state: OpenPaletteState) {
    state.palette_query.set(String::new());
    state.palette_selection.set(0);
    state.palette_open.set(true);
    state.palette_focus_request.update(|request| *request += 1);
}

pub(crate) fn close_palette(palette_open: RwSignal<bool>, palette_query: RwSignal<String>) {
    palette_open.set(false);
    palette_query.set(String::new());
}

/// The session manager modal's own signals (`control_surface::view::
/// session_manager`): whether it's open, the selected row, the row (if any)
/// currently awaiting a second `x` press to confirm termination, and a
/// focus-request counter mirroring `OpenPaletteState::palette_focus_request`
/// (a plain `open.set(true)` wouldn't retrigger `request_focus` on a no-op
/// re-open, since the signal's value wouldn't actually change).
///
/// Bundled as its own handle (rather than living on `CommandActionState`
/// directly) so it's just one field to add there -- `CommandActionState`
/// carries it precisely so `CommandId::OpenSessionManager` can open the
/// modal identically whether it's invoked from the palette or from a
/// `[keybindings]` chord (see `app::command_actions::execute_simple_command`).
#[derive(Clone, Copy)]
pub(crate) struct SessionManagerHandle {
    pub(crate) open: RwSignal<bool>,
    pub(crate) selection: RwSignal<usize>,
    pub(crate) pending_terminate: RwSignal<Option<SessionId>>,
    pub(crate) focus_request: RwSignal<u64>,
}

impl SessionManagerHandle {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            open: RwSignal::new(false),
            selection: RwSignal::new(0),
            pending_terminate: RwSignal::new(None),
            focus_request: RwSignal::new(0),
        }
    }
}

pub(crate) fn open_session_manager(handle: SessionManagerHandle) {
    handle.selection.set(0);
    handle.pending_terminate.set(None);
    handle.open.set(true);
    handle.focus_request.update(|request| *request += 1);
}

pub(crate) fn close_session_manager(handle: SessionManagerHandle) {
    handle.open.set(false);
    handle.pending_terminate.set(None);
}

pub(crate) fn execute_palette_selection(state: PaletteActionState) {
    let command = state.command;
    let workspace = command.workspace();
    let frames = command.frames();
    let palette_query = state.palette_query;
    let palette_selection = state.palette_selection;

    let query = palette_query.get_untracked();
    let selection = palette_selection.get_untracked();
    let item = workspace.with_untracked(|ws| {
        frames.with_untracked(|fr| {
            let items = palette_items(ws, fr, &query);
            items
                .get(clamp_palette_selection(selection, items.len()))
                .cloned()
        })
    });

    let Some(item) = item else {
        return;
    };

    if !item.enabled() {
        return;
    }

    close_palette(state.palette_open, palette_query);
    match item {
        PaletteItem::Command(entry) => {
            execute_command(CommandInvocation::Simple(entry.spec.id), command)
        }
        PaletteItem::DetachedSession { session_id, .. } => {
            execute_command(
                CommandInvocation::AttachSession {
                    session_id,
                    activate: true,
                },
                command,
            );
        }
        PaletteItem::Tab { index, .. } => {
            execute_command(CommandInvocation::ActivateTab { index }, command);
        }
        PaletteItem::TerminateSession { session_id, .. } => {
            execute_command(CommandInvocation::TerminateSession { session_id }, command);
        }
        PaletteItem::TerminateAllDetached { .. } => {
            execute_command(
                CommandInvocation::Simple(CommandId::TerminateAllDetachedSessions),
                command,
            );
        }
    }
}

pub(crate) fn update_palette_query(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    update: impl FnOnce(&mut String),
) {
    palette_query.update(update);
    clamp_current_palette_selection(workspace, frames, palette_query, palette_selection);
}

fn clamp_current_palette_selection(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
) {
    let query = palette_query.get_untracked();
    let item_count = workspace
        .with_untracked(|ws| frames.with_untracked(|fr| palette_items(ws, fr, &query).len()));
    palette_selection.update(|selection| {
        *selection = clamp_palette_selection(*selection, item_count);
    });
}

pub(crate) fn move_palette_selection(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    delta: isize,
) {
    let query = palette_query.get_untracked();
    let item_count = workspace
        .with_untracked(|ws| frames.with_untracked(|fr| palette_items(ws, fr, &query).len()));
    if item_count == 0 {
        palette_selection.set(0);
        return;
    }

    palette_selection.update(|selection| {
        let next = (*selection as isize + delta).clamp(0, item_count.saturating_sub(1) as isize);
        *selection = next as usize;
    });
}
