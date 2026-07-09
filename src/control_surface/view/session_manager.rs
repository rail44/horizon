use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::commands::clamp_palette_selection;
use crate::control_surface::{
    close_session_manager, interpret_session_manager_key, session_manager_items,
    SessionManagerAction, SessionManagerHandle, SessionManagerRow, SESSION_MANAGER_VISIBLE_ROWS,
};
use crate::session::SessionId;
use crate::ui::list_row::{list_row, ListRow, ListRowStyle};
use crate::ui::selectable_list::selectable_list;
use crate::ui::spacing;
use crate::ui::theme;
use crate::workspace::Workspace;
use floem::event::{Event, EventListener, EventPropagation};
use floem::keyboard::KeyEvent;
use floem::prelude::*;
use floem::reactive::{create_effect, create_memo};

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

    // Keeps `handle.selection` pointing at the same *session* across a
    // background list change (e.g. `attach_sessions` reordering the modal's
    // rows while it's open), not just the same numeric slot -- see
    // `SessionManagerHandle::selected_id`'s doc comment for why a bare
    // index can't survive that on its own. Tracks `items` (reruns on any
    // list change) and `handle.open` (reruns once when the modal opens, so
    // `open_session_manager`'s forced `selection = 0` gets an identity to
    // remember without every call site needing workspace access).
    create_effect(move |_| {
        let current_items = items.get();
        let is_open = handle.open.get();
        if !is_open {
            return;
        }

        let prev_selected_id = handle.selected_id.get_untracked();
        let old_index = handle.selection.get_untracked();
        let new_index = reanchor_selection(&current_items, prev_selected_id, old_index);
        if new_index != old_index {
            handle.selection.set(new_index);
        }
        let new_id = current_items.get(new_index).map(|row| row.session_id);
        if new_id != prev_selected_id {
            handle.selected_id.set(new_id);
        }
    });

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
                    handle.selected_id.set(
                        items.with_untracked(|items| items.get(index).map(|item| item.session_id)),
                    );
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
/// clamp_palette_selection`). Used for activation (diving into a row) and
/// for deciding *which* action an `x` press means (see
/// `handle_session_manager_key`'s `pending_terminate_active` gate) -- never
/// for resolving what a confirmed terminate actually acts on. That target
/// is [`terminate_target`], resolved from the identity stashed at
/// `RequestTerminate` rather than a fresh index lookup, precisely because a
/// background list change between request and confirm can make this
/// function's index-based answer point at a different session than the one
/// the user armed.
fn current_row(workspace: RwSignal<Workspace>, selection: usize) -> Option<SessionManagerRow> {
    workspace.with_untracked(|ws| {
        let items = session_manager_items(ws);
        items
            .get(clamp_palette_selection(selection, items.len()))
            .cloned()
    })
}

/// The session a confirmed terminate should act on: the identity captured
/// at `RequestTerminate` (`SessionManagerHandle::pending_terminate`), never
/// a fresh index lookup at confirm time. If that session is no longer in
/// `items` (removed from under the modal before the second `x`), there is
/// nothing safe to terminate -- the caller must cancel rather than
/// substitute whatever now happens to be selected.
fn terminate_target(
    items: &[SessionManagerRow],
    pending_terminate: Option<SessionId>,
) -> Option<SessionId> {
    let target = pending_terminate?;
    items
        .iter()
        .any(|row| row.session_id == target)
        .then_some(target)
}

/// Where the selection cursor should land after `items` changes: following
/// `prev_selected_id` (the session `selection` used to resolve to) to its
/// new position if it's still present, or clamping `old_index` into the new
/// list otherwise (the session was removed, or there was no prior identity
/// to follow at all -- e.g. right after `open_session_manager` resets the
/// cursor to the top with `selected_id` cleared).
fn reanchor_selection(
    items: &[SessionManagerRow],
    prev_selected_id: Option<SessionId>,
    old_index: usize,
) -> usize {
    prev_selected_id
        .and_then(|id| items.iter().position(|row| row.session_id == id))
        .unwrap_or_else(|| clamp_palette_selection(old_index, items.len()))
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
    let items = workspace.with_untracked(session_manager_items);
    if items.is_empty() {
        handle.selection.set(0);
        handle.selected_id.set(None);
        return;
    }

    let next = handle.selection.with_untracked(|selection| {
        (*selection as isize + delta).clamp(0, items.len().saturating_sub(1) as isize) as usize
    });
    handle.selection.set(next);
    handle
        .selected_id
        .set(items.get(next).map(|row| row.session_id));
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
            // Deliberately does not use `row` (index-derived above): the
            // target is the identity stashed at `RequestTerminate`, guarded
            // against having vanished from the list in the meantime. This
            // is the fix for the bug where a background list change could
            // make the confirm act on whatever session now sits at the old
            // index rather than the one the user actually armed.
            let current_items = workspace.with_untracked(session_manager_items);
            let pending = handle.pending_terminate.get_untracked();
            if let Some(target_id) = terminate_target(&current_items, pending) {
                execute_command(
                    CommandInvocation::TerminateSession {
                        session_id: target_id,
                    },
                    state,
                );
            }
            handle.pending_terminate.set(None);
            let new_items = workspace.with_untracked(session_manager_items);
            let clamped =
                clamp_palette_selection(handle.selection.get_untracked(), new_items.len());
            handle.selection.set(clamped);
            handle
                .selected_id
                .set(new_items.get(clamped).map(|row| row.session_id));
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

    // --- terminate_target ---------------------------------------------

    #[test]
    fn terminate_target_resolves_the_pending_id_when_still_present() {
        let a = row(false);
        let b = row(true);
        let items = vec![a.clone(), b.clone()];
        assert_eq!(
            terminate_target(&items, Some(a.session_id)),
            Some(a.session_id)
        );
    }

    #[test]
    fn terminate_target_is_none_without_a_pending_id() {
        let items = vec![row(false)];
        assert_eq!(terminate_target(&items, None), None);
    }

    /// The vanished-pending case: if the armed session was removed from the
    /// list before the second `x`, confirm must cancel rather than
    /// terminate whatever now happens to be in the list.
    #[test]
    fn terminate_target_cancels_when_the_pending_session_vanished() {
        let removed = row(false);
        let items = vec![row(true)]; // `removed` is no longer present
        assert_eq!(terminate_target(&items, Some(removed.session_id)), None);
    }

    /// The core regression: a detached session is armed for termination,
    /// then a background list mutation (e.g. `attach_sessions` reordering
    /// the modal's rows) shifts the active/attached session into the old
    /// index. Confirming must still act on the originally-selected
    /// (detached) session -- never on whatever now sits at that old index.
    #[test]
    fn confirm_terminate_targets_the_originally_selected_session_even_after_the_list_shifts() {
        let detached = row(false);
        let active = row(true);
        let old_index = 0;

        let before_shift = [detached.clone(), active.clone()];
        assert_eq!(before_shift[old_index].session_id, detached.session_id);
        let pending = Some(detached.session_id);

        // Background mutation: `active` now sits at the old index,
        // `detached` has moved elsewhere in the list.
        let after_shift = [active.clone(), detached.clone()];
        assert_eq!(after_shift[old_index].session_id, active.session_id);

        let target = terminate_target(&after_shift, pending);
        assert_eq!(target, Some(detached.session_id));
        assert_ne!(target, Some(active.session_id));
    }

    // --- reanchor_selection ---------------------------------------------

    #[test]
    fn reanchor_selection_follows_the_session_to_its_new_index() {
        let a = row(false);
        let b = row(true);
        // A session gets inserted ahead of `a`, pushing it from index 0 to 1.
        let inserted = row(false);
        let new_items = vec![inserted, a.clone(), b];
        assert_eq!(reanchor_selection(&new_items, Some(a.session_id), 0), 1);
    }

    #[test]
    fn reanchor_selection_clamps_when_the_selected_session_is_gone() {
        let removed = row(false);
        let items = vec![row(true)]; // `removed` is no longer present
        assert_eq!(reanchor_selection(&items, Some(removed.session_id), 2), 0);
    }

    #[test]
    fn reanchor_selection_clamps_when_there_is_no_prior_identity() {
        let items = vec![row(false), row(true)];
        assert_eq!(reanchor_selection(&items, None, 5), 1);
    }
}
