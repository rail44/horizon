use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::commands::{clamp_palette_selection, CommandId};
use crate::control_surface::items::palette_rows;
use crate::control_surface::{PaletteItem, PaletteRow, PaletteStage, Placement};
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

/// The palette's full open/reset state -- bundled so `CommandId::SplitPane`/
/// `CommandId::NewTab` (`app::command_actions::execute_simple_command`) can
/// open the second-stage view chooser identically whether invoked from a
/// palette row or a `[keybindings]` chord, the same way `SessionManagerHandle`
/// lets `CommandId::OpenSessionManager` open that modal from either surface.
/// Carried on `CommandActionState` (`command_actions::CommandActionState::
/// palette`) for that reason, and reused as-is by the normal palette-open
/// path (`open_palette`, called from `app::input`) since it needs the exact
/// same reset shape, just landing on `PaletteStage::Commands` instead of a
/// `ViewChooser`.
#[derive(Clone, Copy)]
pub(crate) struct OpenPaletteState {
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
    pub(crate) palette_stage: RwSignal<PaletteStage>,
    pub(crate) palette_focus_request: RwSignal<u64>,
}

impl OpenPaletteState {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            palette_open: RwSignal::new(false),
            palette_query: RwSignal::new(String::new()),
            palette_selection: RwSignal::new(0),
            palette_stage: RwSignal::new(PaletteStage::Commands),
            palette_focus_request: RwSignal::new(0),
        }
    }
}

/// Resets and opens the palette at `stage` -- the shared worker behind
/// [`open_palette`] (always `PaletteStage::Commands`) and
/// [`open_view_chooser`] (always `PaletteStage::ViewChooser`).
fn open_palette_at_stage(state: OpenPaletteState, stage: PaletteStage) {
    state.palette_query.set(String::new());
    state.palette_selection.set(0);
    state.palette_stage.set(stage);
    state.palette_open.set(true);
    state.palette_focus_request.update(|request| *request += 1);
}

pub(crate) fn open_palette(state: OpenPaletteState) {
    open_palette_at_stage(state, PaletteStage::Commands);
}

/// Opens the palette straight into the second-stage view chooser, tagged
/// with `placement` -- `CommandId::SplitPane`/`CommandId::NewTab`'s shared
/// implementation (`app::command_actions::execute_simple_command`), reached
/// identically from a palette row (`execute_palette_selection` dispatching
/// `CommandInvocation::Simple` like any other catalog command) or a
/// `[keybindings]` chord (`app::input::AppInput`'s fallback, which also
/// just dispatches `Simple`). Resets query/selection exactly like
/// [`open_palette`] -- see `docs/roadmap.md`'s "Placement-first session
/// creation" for why the chooser always starts unfiltered rather than
/// preserving whatever was typed in the Commands stage.
pub(crate) fn open_view_chooser(state: OpenPaletteState, placement: Placement) {
    open_palette_at_stage(state, PaletteStage::ViewChooser { placement });
}

pub(crate) fn close_palette(
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_stage: RwSignal<PaletteStage>,
) {
    palette_open.set(false);
    palette_query.set(String::new());
    palette_stage.set(PaletteStage::Commands);
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
    let stage_signal = command.palette.palette_stage;

    let query = palette_query.get_untracked();
    let selection = palette_selection.get_untracked();
    let stage = stage_signal.get_untracked();
    let row = workspace.with_untracked(|ws| {
        frames.with_untracked(|fr| {
            let rows = palette_rows(ws, fr, stage, &query);
            rows.get(clamp_palette_selection(selection, rows.len()))
                .cloned()
        })
    });

    let Some(row) = row else {
        return;
    };

    match row {
        PaletteRow::Catalog(item) => {
            if !item.enabled() {
                return;
            }
            close_palette(state.palette_open, palette_query, stage_signal);
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
        PaletteRow::Chooser(chooser_row) => {
            // Only reachable while `stage` is `ViewChooser`, whose
            // `placement` decides `split_target`: `SplitPane` targets the
            // session behind the active pane at commit time (mirroring
            // `SplitActivePane`'s retired active-pane resolution), `NewTab`
            // never splits.
            let PaletteStage::ViewChooser { placement } = stage else {
                return;
            };
            let split_target = match placement {
                Placement::SplitPane => workspace.with_untracked(|ws| ws.active_session_id()),
                Placement::NewTab => None,
            };
            close_palette(state.palette_open, palette_query, stage_signal);
            execute_command(
                CommandInvocation::CreateSession {
                    kind: chooser_row.kind,
                    role_id: chooser_row.role_id,
                    split_target,
                    activate: true,
                    prompt: None,
                },
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
    palette_stage: RwSignal<PaletteStage>,
    update: impl FnOnce(&mut String),
) {
    palette_query.update(update);
    clamp_current_palette_selection(
        workspace,
        frames,
        palette_query,
        palette_selection,
        palette_stage,
    );
}

fn clamp_current_palette_selection(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_stage: RwSignal<PaletteStage>,
) {
    let query = palette_query.get_untracked();
    let stage = palette_stage.get_untracked();
    let item_count = workspace
        .with_untracked(|ws| frames.with_untracked(|fr| palette_rows(ws, fr, stage, &query).len()));
    palette_selection.update(|selection| {
        *selection = clamp_palette_selection(*selection, item_count);
    });
}

pub(crate) fn move_palette_selection(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_stage: RwSignal<PaletteStage>,
    delta: isize,
) {
    let query = palette_query.get_untracked();
    let stage = palette_stage.get_untracked();
    let item_count = workspace
        .with_untracked(|ws| frames.with_untracked(|fr| palette_rows(ws, fr, stage, &query).len()));
    if item_count == 0 {
        palette_selection.set(0);
        return;
    }

    palette_selection.update(|selection| {
        let next = (*selection as isize + delta).clamp(0, item_count.saturating_sub(1) as isize);
        *selection = next as usize;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::command_actions::CommandActionState;
    use crate::workspace::PaneKind;

    fn test_state(workspace: Workspace) -> PaletteActionState {
        let command = CommandActionState::for_test(workspace);
        PaletteActionState {
            palette_open: command.palette.palette_open,
            palette_query: command.palette.palette_query,
            palette_selection: command.palette.palette_selection,
            command,
        }
    }

    #[test]
    fn open_palette_resets_to_the_commands_stage() {
        let state = OpenPaletteState::for_test();
        state.palette_stage.set(PaletteStage::ViewChooser {
            placement: Placement::NewTab,
        });
        state.palette_query.set("stale".to_string());
        state.palette_selection.set(3);

        open_palette(state);

        assert_eq!(state.palette_stage.get_untracked(), PaletteStage::Commands);
        assert_eq!(state.palette_query.get_untracked(), "");
        assert_eq!(state.palette_selection.get_untracked(), 0);
        assert!(state.palette_open.get_untracked());
    }

    #[test]
    fn open_view_chooser_opens_directly_into_the_chooser_stage() {
        let state = OpenPaletteState::for_test();

        open_view_chooser(state, Placement::SplitPane);

        assert_eq!(
            state.palette_stage.get_untracked(),
            PaletteStage::ViewChooser {
                placement: Placement::SplitPane
            }
        );
        assert!(state.palette_open.get_untracked());
    }

    #[test]
    fn close_palette_resets_stage_alongside_open_and_query() {
        let open = RwSignal::new(true);
        let query = RwSignal::new("split".to_string());
        let stage = RwSignal::new(PaletteStage::ViewChooser {
            placement: Placement::NewTab,
        });

        close_palette(open, query, stage);

        assert!(!open.get_untracked());
        assert_eq!(query.get_untracked(), "");
        assert_eq!(stage.get_untracked(), PaletteStage::Commands);
    }

    #[test]
    fn execute_palette_selection_on_split_pane_row_opens_the_chooser_without_creating_a_session() {
        let state = test_state(Workspace::mvp());
        let before_session_count = state
            .command
            .workspace()
            .with_untracked(|ws| ws.session_count());

        // Selection 0 in the default Commands stage is `Split Pane…` (see
        // `commands::core_commands`'s order).
        execute_palette_selection(state.clone());

        assert_eq!(
            state.command.palette.palette_stage.get_untracked(),
            PaletteStage::ViewChooser {
                placement: Placement::SplitPane
            }
        );
        assert!(state.command.palette.palette_open.get_untracked());
        assert_eq!(
            state
                .command
                .workspace()
                .with_untracked(|ws| ws.session_count()),
            before_session_count,
            "opening the chooser must not itself create a session"
        );
    }

    #[test]
    fn execute_palette_selection_on_a_chooser_row_creates_a_session_and_closes_the_palette() {
        let state = test_state(Workspace::mvp());
        state
            .command
            .palette
            .palette_stage
            .set(PaletteStage::ViewChooser {
                placement: Placement::NewTab,
            });
        state.command.palette.palette_open.set(true);
        state.palette_selection.set(0);
        let before_tab_count = state
            .command
            .workspace()
            .with_untracked(|ws| ws.tab_count());

        // Row 0 of the chooser is always `Terminal` (see
        // `items::view_chooser_rows`'s kinds-first order).
        execute_palette_selection(state.clone());

        assert!(!state.command.palette.palette_open.get_untracked());
        assert_eq!(
            state.command.palette.palette_stage.get_untracked(),
            PaletteStage::Commands
        );
        assert_eq!(
            state
                .command
                .workspace()
                .with_untracked(|ws| ws.tab_count()),
            before_tab_count + 1
        );
        assert!(state
            .command
            .workspace()
            .with_untracked(|ws| ws.active_session_id())
            .is_some());
        assert_eq!(
            state
                .command
                .workspace()
                .with_untracked(|ws| ws.visible_pane_kind(ws.active_visible_index())),
            Some(PaneKind::Terminal)
        );
    }

    #[test]
    fn execute_palette_selection_on_split_pane_placement_splits_next_to_the_active_session() {
        let workspace = Workspace::mvp();
        let state = test_state(workspace);
        state
            .command
            .palette
            .palette_stage
            .set(PaletteStage::ViewChooser {
                placement: Placement::SplitPane,
            });
        state.command.palette.palette_open.set(true);
        state.palette_selection.set(0);
        let before_tab_count = state
            .command
            .workspace()
            .with_untracked(|ws| ws.tab_count());

        execute_palette_selection(state.clone());

        assert_eq!(
            state
                .command
                .workspace()
                .with_untracked(|ws| ws.tab_count()),
            before_tab_count,
            "splitting next to a session must not open a new tab"
        );
        assert_eq!(
            state
                .command
                .workspace()
                .with_untracked(|ws| ws.visible_panes().len()),
            2
        );
    }
}
