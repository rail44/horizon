use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::commands::{clamp_palette_selection, CommandId};
use crate::control_surface::{overview_items, palette_items, OverviewItem, PaletteItem};
use crate::session::Frames;
use crate::workspace::Workspace;
use floem::prelude::*;

#[derive(Clone)]
pub(crate) struct PaletteActionState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
}

#[derive(Clone)]
pub(crate) struct OverviewActionState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) overview_selection: RwSignal<usize>,
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

pub(crate) fn close_control_surface(palette_open: RwSignal<bool>) {
    palette_open.set(false);
}

pub(crate) fn execute_overview_selection(state: OverviewActionState) {
    let command = state.command;
    let workspace = command.workspace();
    let overview_selection = state.overview_selection;

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

    close_control_surface(state.palette_open);
    match item {
        OverviewItem::Tab { index, .. } => {
            execute_command(CommandInvocation::ActivateTab { index }, command);
        }
        OverviewItem::DetachedSession { session_id, .. } => {
            execute_command(CommandInvocation::AttachSession { session_id }, command);
        }
        OverviewItem::Pane {
            tab_index,
            pane_index,
            ..
        } => {
            execute_command(
                CommandInvocation::ActivatePane {
                    tab_index,
                    pane_index,
                },
                command,
            );
        }
    }
}

pub(crate) fn move_overview_selection(
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
            execute_command(CommandInvocation::AttachSession { session_id }, command);
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
