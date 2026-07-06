use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::commands::clamp_palette_selection;
use crate::control_surface::{
    close_session_manager, interpret_session_manager_key, session_manager_items,
    SessionManagerAction, SessionManagerHandle, SessionManagerRow, SESSION_MANAGER_VISIBLE_ROWS,
};
use crate::ui::list_row::{list_row, ListRow, ListRowStyle};
use crate::ui::selectable_list::selectable_list;
use crate::ui::spacing;
use crate::ui::theme;
use crate::workspace::Workspace;
use floem::event::{Event, EventListener, EventPropagation};
use floem::keyboard::KeyEvent;
use floem::prelude::*;
use floem::reactive::create_memo;

const SESSION_MANAGER_ROW_HEIGHT: f64 = 52.0;
const SESSION_MANAGER_ROW_STYLE: ListRowStyle = ListRowStyle {
    badge_width: 86.0,
    row_height: SESSION_MANAGER_ROW_HEIGHT,
    padding_horiz: spacing::SPACING_LG,
};

/// The session manager modal (`docs/plans/application-ui/01-session-
/// manager.md`): lists every session the workspace knows about (detached
/// first), letting `Enter` attach a detached one or jump to an attached
/// one's pane, and `x` `x` terminate it -- all through the command model,
/// via the `CommandActionState` this is handed (which carries its own
/// `session_manager` handle, see `control_surface::actions::
/// SessionManagerHandle`'s doc comment for why that's bundled there rather
/// than threaded separately). Reuses the workspace overview's former
/// overlay pattern: absolute position, a `z_index` above the workspace, and
/// `.keyboard_navigable().request_focus(...)` so it grabs real keyboard
/// focus the instant it opens.
pub(crate) fn session_manager_modal(state: CommandActionState) -> impl IntoView {
    let handle = state.session_manager;
    let workspace = state.workspace();

    let items = create_memo(move |_| workspace.with(session_manager_items));

    let row_state_source = state.clone();
    let list = selectable_list(
        move || items.with(|items| items.len().max(1)),
        move || handle.selection.get(),
        move |index| {
            let row_state = row_state_source.clone();
            let row = move || {
                items.with(|items| {
                    if items.is_empty() {
                        return Some(empty_row());
                    }
                    items.get(index).map(|item| {
                        let pending = handle.pending_terminate.get() == Some(item.session_id);
                        session_manager_row_view(item, pending)
                    })
                })
            };

            list_row(
                row,
                move || handle.selection.get() == index,
                SESSION_MANAGER_ROW_STYLE,
                move || {
                    handle.selection.set(index);
                    activate_selected_row(row_state.clone());
                },
            )
        },
        SESSION_MANAGER_VISIBLE_ROWS as f64 * SESSION_MANAGER_ROW_HEIGHT,
    );

    container(
        v_stack((
            v_stack((
                label(|| "Manage Sessions".to_string())
                    .style(|s| s.width_full().font_size(16).color(theme::text_primary())),
                label(move || {
                    items.with(|items| {
                        let detached = items.iter().filter(|item| !item.attached).count();
                        format!("{} session(s) · {} detached", items.len(), detached)
                    })
                })
                .style(|s| s.width_full().font_size(12).color(theme::text_muted())),
            ))
            .style(|s| {
                s.width_full()
                    .padding_horiz(spacing::SPACING_LG)
                    .padding_vert(spacing::SPACING_MD)
                    .gap(4)
                    .background(theme::surface_raised())
            }),
            list,
            label(move || {
                if handle.pending_terminate.get().is_some() {
                    "x: confirm terminate · esc: cancel".to_string()
                } else {
                    "enter: attach · x: terminate · esc: close".to_string()
                }
            })
            .style(|s| {
                s.width_full()
                    .height(28)
                    .items_center()
                    .padding_horiz(spacing::SPACING_LG)
                    .font_size(11)
                    .color(theme::text_muted())
                    .background(theme::surface_chrome())
            }),
        ))
        .style(|s| s.width_full()),
    )
    .keyboard_navigable()
    .request_focus(move || {
        handle.focus_request.get();
    })
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            handle_session_manager_key(key_event, state.clone());
        }
        EventPropagation::Stop
    })
    .style(move |s| {
        if !handle.open.get() {
            return s.hide();
        }

        s.absolute()
            .inset_top(74.0)
            .inset_left(240.0)
            .width(680)
            .z_index(10)
            .border(1.0)
            .border_color(theme::accent())
            .background(theme::surface_base())
    })
}

/// Resolves the row currently at `selection`, clamped like every other
/// selection-indexed lookup in `control_surface` (`app::commands::
/// clamp_palette_selection`).
fn current_row(workspace: RwSignal<Workspace>, selection: usize) -> Option<SessionManagerRow> {
    workspace.with_untracked(|ws| {
        let items = session_manager_items(ws);
        items
            .get(clamp_palette_selection(selection, items.len()))
            .cloned()
    })
}

/// `Enter`, and a row's own click -- attach a detached row (diving, since
/// this is a human surface, per `docs/workspace-mode-design.md`'s Amended
/// second-round decision) or jump to an attached row's own pane. Closes the
/// modal either way.
fn activate_selected_row(state: CommandActionState) {
    let handle = state.session_manager;
    let workspace = state.workspace();
    let Some(row) = current_row(workspace, handle.selection.get_untracked()) else {
        return;
    };

    close_session_manager(handle);
    if row.attached {
        if let Some((tab_index, pane_index)) =
            workspace.with_untracked(|ws| ws.pane_location_for_session(row.session_id))
        {
            execute_command(
                CommandInvocation::ActivatePane {
                    tab_index,
                    pane_index,
                },
                state,
            );
        }
    } else {
        execute_command(
            CommandInvocation::AttachSession {
                session_id: row.session_id,
                activate: true,
            },
            state,
        );
    }
}

fn move_session_manager_selection(
    workspace: RwSignal<Workspace>,
    handle: SessionManagerHandle,
    delta: isize,
) {
    handle.pending_terminate.set(None);
    let len = workspace.with_untracked(|ws| session_manager_items(ws).len());
    if len == 0 {
        handle.selection.set(0);
        return;
    }

    handle.selection.update(|selection| {
        let next = (*selection as isize + delta).clamp(0, len.saturating_sub(1) as isize);
        *selection = next as usize;
    });
}

/// `Event::KeyDown` entry point for the modal -- classifies via
/// `session_manager_input::interpret_session_manager_key` (pure), then
/// dispatches the resulting action through the command model itself,
/// mirroring `workspace::view::pane`'s inline `ModeAction` dispatch.
fn handle_session_manager_key(key_event: &KeyEvent, state: CommandActionState) {
    let handle = state.session_manager;
    let workspace = state.workspace();
    let selection = handle.selection.get_untracked();
    let row = current_row(workspace, selection);
    let pending_terminate_active = row
        .as_ref()
        .is_some_and(|row| handle.pending_terminate.get_untracked() == Some(row.session_id));

    let Some(action) = interpret_session_manager_key(key_event, pending_terminate_active) else {
        return;
    };

    match action {
        SessionManagerAction::MoveSelection(delta) => {
            move_session_manager_selection(workspace, handle, delta);
        }
        SessionManagerAction::Activate => activate_selected_row(state),
        SessionManagerAction::RequestTerminate => {
            if let Some(row) = row {
                handle.pending_terminate.set(Some(row.session_id));
            }
        }
        SessionManagerAction::ConfirmTerminate => {
            if let Some(row) = row {
                execute_command(
                    CommandInvocation::TerminateSession {
                        session_id: row.session_id,
                    },
                    state,
                );
                handle.pending_terminate.set(None);
                let new_len = workspace.with_untracked(|ws| session_manager_items(ws).len());
                handle.selection.update(|selection| {
                    *selection = clamp_palette_selection(*selection, new_len);
                });
            }
        }
        SessionManagerAction::CancelPendingTerminate => {
            handle.pending_terminate.set(None);
        }
        SessionManagerAction::Close => {
            close_session_manager(handle);
        }
    }
}

fn empty_row() -> ListRow {
    ListRow {
        badge: String::new(),
        badge_color: theme::text_subtle(),
        title: "No sessions".to_string(),
        description: "Nothing to attach or terminate yet.".to_string(),
        enabled: false,
        destructive: false,
    }
}

/// One session row's presentation: the badge is the attach-state chip
/// (`docs/plans/application-ui/01-session-manager.md`'s "attach 状態チップ"),
/// bright for a detached session (worth hunting for) and muted for an
/// already-attached one; `title` is already `"{Kind} #{number}"` (see
/// `workspace::types::entity::session_title`), so kind and display number
/// don't need restating separately. `destructive` (and therefore the
/// danger-colored badge override, `ui::list_row::effective_badge_color`)
/// is set exactly when this row is the one currently awaiting a second `x`
/// press.
fn session_manager_row_view(row: &SessionManagerRow, pending_terminate: bool) -> ListRow {
    let (badge, badge_color) = if row.attached {
        ("ATTACHED", theme::text_muted())
    } else {
        ("DETACHED", theme::cursor_accent())
    };
    let description = if row.attached {
        format!("{} session · Enter jumps to its pane", row.kind.label())
    } else {
        format!(
            "{} session · Enter attaches it as a split in the active tab",
            row.kind.label()
        )
    };

    ListRow {
        badge: badge.to_string(),
        badge_color,
        title: row.title.clone(),
        description,
        enabled: true,
        destructive: pending_terminate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::workspace::SessionKind;

    fn row(attached: bool) -> SessionManagerRow {
        SessionManagerRow {
            session_id: SessionId::new(),
            kind: SessionKind::Terminal,
            display_number: 1,
            title: "Terminal #1".to_string(),
            attached,
        }
    }

    #[test]
    fn detached_row_gets_the_bright_chip_and_dive_hint() {
        let view = session_manager_row_view(&row(false), false);
        assert_eq!(view.badge, "DETACHED");
        assert!(!view.destructive);
        assert_eq!(view.title, "Terminal #1");
    }

    #[test]
    fn attached_row_gets_the_muted_chip() {
        let view = session_manager_row_view(&row(true), false);
        assert_eq!(view.badge, "ATTACHED");
        assert!(!view.destructive);
    }

    #[test]
    fn a_row_pending_termination_is_marked_destructive() {
        let view = session_manager_row_view(&row(false), true);
        assert!(view.destructive);
    }
}
